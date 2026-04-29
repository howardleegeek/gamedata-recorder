//! `action_camera.json` sink — buyer plugin wire contract.
//!
//! The buyer's training plugin requires per-frame records joining mouse,
//! keyboard, gamepad, and (downstream-filled) camera state into a single
//! JSON array. The format is fixed; this module is the on-recorder
//! implementation of the same contract that the post-hoc Python adapter at
//! `oyster-enrichment/bin/convert_to_action_camera.py` implements.
//!
//! ## Why a separate sink (and not modify inputs.jsonl)
//!
//! `inputs.jsonl` is the lossless event stream — sub-frame mouse motion,
//! discrete keyboard up/down events, scrolls, gamepad samples. The buyer's
//! plugin wants per-frame snapshots, with cursor position accumulated,
//! held-keys reduced to a sorted list, gamepad sticks/triggers reduced to
//! the latest seen sample, and gamepad buttons remapped to an XInput-shape
//! u16 bitmask. Computing that on the recorder side avoids every consumer
//! re-implementing the replay logic, and avoids bundle adapters (the
//! Python script) being a hard dependency on the ingest pipeline.
//!
//! ## Data flow
//!
//! At session finalize, AFTER `inputs.jsonl` and `frames.jsonl` have been
//! flushed and durably written to disk, this module reads both back,
//! replays the input events up to each frame's timestamp (mirroring the
//! Python adapter's logic), and writes the per-frame array to
//! `action_camera.json` next to the other artifacts.
//!
//! Reading the on-disk artifacts (rather than tee'ing the in-flight event
//! stream) keeps this writer fully decoupled from the input/fps pipelines
//! and makes it byte-for-byte equivalent to the Python adapter for the
//! same source data — the two implementations can be cross-validated.
//!
//! ## Input modality
//!
//! Each session is one of three modalities:
//!
//! - `keyboard_mouse` — only keyboard / mouse events. The mouse + key
//!   fields are populated; the gamepad fields are emitted as `null`.
//! - `gamepad` — only gamepad events. The gamepad fields are populated;
//!   the mouse + key fields are emitted as `null`.
//! - `mixed` — both kinds of events. All fields populated.
//!
//! Modality is auto-detected from the event-type distribution in
//! `inputs.jsonl` and reported on every per-frame record under
//! `input_modality`.
//!
//! ## Schema (per record, in array order)
//!
//! ```json
//! {
//!   "frame_index": <u64>,
//!   "timestamp": <f64 seconds>,
//!   "input_modality": "keyboard_mouse" | "gamepad" | "mixed",
//!   "mouseX": <f64 in [0, 1]> | null,
//!   "mouseY": <f64 in [0, 1]> | null,
//!   "mouse_dx": <f64 pixels, per-frame delta> | null,
//!   "mouse_dy": <f64 pixels, per-frame delta> | null,
//!   "keyCode": <sorted ascending list of held VK codes at this frame> | null,
//!   "gamepad_left_stick_x":  <f64 in [-1, 1]> | null,
//!   "gamepad_left_stick_y":  <f64 in [-1, 1]> | null,
//!   "gamepad_right_stick_x": <f64 in [-1, 1]> | null,
//!   "gamepad_right_stick_y": <f64 in [-1, 1]> | null,
//!   "gamepad_left_trigger":  <f64 in [0, 1]>  | null,
//!   "gamepad_right_trigger": <f64 in [0, 1]>  | null,
//!   "gamepad_buttons": <u16 XInput bitmask> | null,
//!   "camera_position": null,
//!   "camera_rotation_quaternion": null
//! }
//! ```
//!
//! `camera_position` and `camera_rotation_quaternion` are ALWAYS `null` at
//! the recorder layer — the recorder has no engine-state access. Downstream
//! enrichment fills these from a pose backend; the field shape is preserved
//! here so the buyer's plugin sees a stable schema regardless of whether
//! pose data exists yet.

use std::path::Path;

use color_eyre::Result;
use color_eyre::eyre::WrapErr as _;
use serde::Serialize;

use crate::util::durable_write;

/// Detected input modality for a recording session.
///
/// Stored on every per-frame record under `input_modality` so the buyer's
/// plugin can branch on it without inferring from null patterns.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputModality {
    KeyboardMouse,
    Gamepad,
    Mixed,
}

impl InputModality {
    fn has_kbm(self) -> bool {
        matches!(self, Self::KeyboardMouse | Self::Mixed)
    }
    fn has_pad(self) -> bool {
        matches!(self, Self::Gamepad | Self::Mixed)
    }
}

/// Per-frame record matching the buyer plugin's wire contract.
///
/// Field naming intentionally uses camelCase for `mouseX`/`mouseY`/`keyCode`
/// (the buyer's spec) and snake_case for everything else (also the buyer's
/// spec). serde tags pin each field name explicitly so the serialized
/// output is identical to the Python adapter regardless of future struct
/// refactors.
///
/// The mouse / keyboard fields are `Option`s rather than always-populated:
/// a gamepad-only session emits them as JSON `null` (and the gamepad
/// fields likewise null on a keyboard-mouse-only session). The modality
/// flag tells consumers which subset is meaningful for any given record.
#[derive(Debug, Serialize, PartialEq)]
pub struct ActionCameraRecord {
    pub frame_index: u64,
    pub timestamp: f64,
    pub input_modality: InputModality,
    #[serde(rename = "mouseX")]
    pub mouse_x: Option<f64>,
    #[serde(rename = "mouseY")]
    pub mouse_y: Option<f64>,
    pub mouse_dx: Option<f64>,
    pub mouse_dy: Option<f64>,
    #[serde(rename = "keyCode")]
    pub key_code: Option<Vec<u16>>,
    /// Left stick X axis, `[-1.0, 1.0]`. `None` on keyboard_mouse sessions.
    pub gamepad_left_stick_x: Option<f64>,
    /// Left stick Y axis, `[-1.0, 1.0]`. `None` on keyboard_mouse sessions.
    pub gamepad_left_stick_y: Option<f64>,
    /// Right stick X axis, `[-1.0, 1.0]`. `None` on keyboard_mouse sessions.
    pub gamepad_right_stick_x: Option<f64>,
    /// Right stick Y axis, `[-1.0, 1.0]`. `None` on keyboard_mouse sessions.
    pub gamepad_right_stick_y: Option<f64>,
    /// Left trigger, `[0.0, 1.0]`. `None` on keyboard_mouse sessions.
    pub gamepad_left_trigger: Option<f64>,
    /// Right trigger, `[0.0, 1.0]`. `None` on keyboard_mouse sessions.
    pub gamepad_right_trigger: Option<f64>,
    /// XInput-shaped u16 bitmask of currently-held buttons. `None` on
    /// keyboard_mouse sessions; `Some(0)` means "gamepad active, no
    /// buttons held this frame" — distinct from `None`.
    pub gamepad_buttons: Option<u16>,
    /// Always `None` at the recorder layer. Schema preserved so downstream
    /// enrichment can fill it without changing the JSON shape.
    pub camera_position: Option<[f64; 3]>,
    /// Always `None` at the recorder layer. Quaternion convention when
    /// filled: `[w, x, y, z]` (scalar first).
    pub camera_rotation_quaternion: Option<[f64; 4]>,
}

/// gilrs button index → XInput u16 bitmask. Mirrors the Python adapter.
/// gilrs indices not in this map contribute nothing to the bitmask.
fn gilrs_button_to_xinput_bit(idx: u16) -> u16 {
    match idx {
        16 => 0x0001, // DPAD_UP
        17 => 0x0002, // DPAD_DOWN
        18 => 0x0004, // DPAD_LEFT
        19 => 0x0008, // DPAD_RIGHT
        12 => 0x0010, // START
        11 => 0x0020, // BACK / SELECT
        14 => 0x0040, // LSTICK / LTHUMB
        15 => 0x0080, // RSTICK / RTHUMB
        7 => 0x0100,  // LB / LT
        8 => 0x0200,  // RB / RT
        1 => 0x1000,  // A / SOUTH
        2 => 0x2000,  // B / EAST
        5 => 0x4000,  // X / WEST
        4 => 0x8000,  // Y / NORTH
        _ => 0x0000,
    }
}

// gilrs axis indices, mirrored from
// crates/input-capture/src/gamepad_capture.rs.
const AXIS_LSTICKX: u16 = 1;
const AXIS_LSTICKY: u16 = 2;
const AXIS_LEFTZ: u16 = 3; // left trigger
const AXIS_RSTICKX: u16 = 4;
const AXIS_RSTICKY: u16 = 5;
const AXIS_RIGHTZ: u16 = 6; // right trigger

/// Detect the input modality of a recording from the event-type histogram
/// of `inputs.jsonl`. Mirrors the Python adapter's `_detect_input_modality`.
fn detect_input_modality(inputs: &[InputRow]) -> InputModality {
    let mut keyboard_or_mouse = 0usize;
    let mut gamepad = 0usize;
    for row in inputs {
        match row.event_type.as_str() {
            "MOUSE_MOVE" | "MOUSE_BUTTON" | "SCROLL" | "KEYBOARD" => {
                keyboard_or_mouse += 1;
            }
            "GAMEPAD_BUTTON" | "GAMEPAD_BUTTON_VALUE" | "GAMEPAD_AXIS" => {
                gamepad += 1;
            }
            _ => {}
        }
    }
    if gamepad > 0 && keyboard_or_mouse == 0 {
        InputModality::Gamepad
    } else if gamepad > 0 && keyboard_or_mouse > 0 {
        InputModality::Mixed
    } else {
        // Empty bundles, START-only bundles, etc. fall back to legacy KBM
        // schema so old recordings continue to lint cleanly.
        InputModality::KeyboardMouse
    }
}

/// One row from `inputs.jsonl`. Only fields we use here.
#[derive(Debug, serde::Deserialize)]
struct InputRow {
    timestamp: f64,
    event_type: String,
    /// JSON array of args — schema depends on event_type. We treat as raw Value
    /// and downcast in the replay step.
    #[serde(default)]
    event_args: serde_json::Value,
}

/// One row from `frames.jsonl`. Only fields we use here.
#[derive(Debug, serde::Deserialize)]
struct FrameRow {
    /// Zero-based frame index. Field name matches `FrameTimestamp::idx`.
    idx: u64,
    /// Nanoseconds since recording start. Field name matches
    /// `FrameTimestamp::t_ns`.
    t_ns: u64,
}

/// Build the per-frame `action_camera.json` records by replaying the events
/// from `inputs.jsonl` up to each frame timestamp.
///
/// `screen_w` / `screen_h` are the dimensions used to normalize cursor
/// position into [0, 1]. These should be the recorder's `game_resolution`.
///
/// The replay logic mirrors `convert_to_action_camera.py`:
///   - Modality is auto-detected from the event-type distribution.
///   - Cursor starts at `(screen_w/2, screen_h/2)` (screen center).
///   - `MOUSE_MOVE` events accumulate as `(dx, dy)` deltas, clamped to the
///     screen rect. The recorder's MOUSE_MOVE arg is `[dx, dy]` (signed
///     i32 per `output_types.rs::InputEventType::MouseMove`).
///   - `KEYBOARD` arg `[keycode, pressed]` toggles a held-key set. `keyCode`
///     in the output is the sorted ascending list of currently-held VK codes
///     at the frame's timestamp.
///   - `mouse_dx` / `mouse_dy` are per-frame pixel deltas: `cursor_now -
///     cursor_at_previous_frame` (matches the Python adapter — NOT raw
///     summed event deltas; this is the visually-meaningful per-frame
///     motion).
///   - `GAMEPAD_AXIS [axis_idx, value]` rolls the latest seen value per axis
///     forward; sticks clamp to `[-1, 1]`, triggers to `[0, 1]`.
///   - `GAMEPAD_BUTTON [idx, pressed]` toggles a held-button set; the
///     bitmask is `OR(GILRS_TO_XINPUT_BIT[idx])`. Indices outside the
///     XInput map are dropped.
///   - For `keyboard_mouse` modality, the gamepad fields are emitted as
///     `null`. For `gamepad`, the mouse + key fields are emitted as `null`.
///     For `mixed`, all fields populated.
///
/// Inputs are expected to already be in chronological order (which the
/// recorder produces by construction — InputEventStream uses an ordered
/// mpsc channel and the writer flushes in receive order). We do not re-sort
/// to keep the cost linear in the number of events.
fn build_records(
    inputs: &[InputRow],
    frames: &[FrameRow],
    screen_w: u32,
    screen_h: u32,
) -> Vec<ActionCameraRecord> {
    let mut records: Vec<ActionCameraRecord> = Vec::with_capacity(frames.len());

    let modality = detect_input_modality(inputs);
    let has_kbm = modality.has_kbm();
    let has_pad = modality.has_pad();

    let screen_w_f = (screen_w.max(1)) as f64;
    let screen_h_f = (screen_h.max(1)) as f64;

    // Cursor state — start at screen center, same default as the Python
    // adapter. The recorder doesn't capture absolute cursor position, only
    // raw deltas, so any starting position is a guess; center is the most
    // neutral.
    let mut cursor_x: f64 = screen_w_f / 2.0;
    let mut cursor_y: f64 = screen_h_f / 2.0;
    let mut prev_x: f64 = cursor_x;
    let mut prev_y: f64 = cursor_y;
    let mut held: std::collections::BTreeSet<u16> = std::collections::BTreeSet::new();

    // Gamepad state. We carry the rolling latest-value per axis and a
    // held-set of gilrs button indices, mapped to the XInput bitmask at
    // emit time. Axis defaults are 0.0 (resting position) so a gamepad
    // session that hasn't yet emitted a deflection event still produces
    // reasonable values.
    let mut axis_lstick_x: f64 = 0.0;
    let mut axis_lstick_y: f64 = 0.0;
    let mut axis_rstick_x: f64 = 0.0;
    let mut axis_rstick_y: f64 = 0.0;
    let mut axis_left_trigger: f64 = 0.0;
    let mut axis_right_trigger: f64 = 0.0;
    let mut held_pad_buttons: std::collections::BTreeSet<u16> = std::collections::BTreeSet::new();

    let mut input_idx = 0usize;

    for frame in frames {
        // Frame timestamp in seconds since recording start. `inputs.jsonl`
        // timestamps are wall-clock unix seconds (see InputEvent::new_at_now),
        // not session-relative. To compare them we therefore align both
        // streams against the recording-start anchor — but we don't actually
        // know the wall-clock anchor here. Use the FIRST input event's
        // timestamp as the wall-clock anchor and offset frame timestamps by
        // it. This matches what the Python adapter does implicitly: it
        // sorts all events to a single timeline and walks them together.
        //
        // For the on-recorder version we use the simpler convention: both
        // streams go through the same Recording::stop path AFTER the input
        // and frame logs are written, and the frame `t_ns` field is exactly
        // "nanoseconds since recording start" (see fps_logger.rs). The first
        // input event written by InputEventWriter::start is a START event
        // whose timestamp is the wall-clock at recording-start. So we can
        // anchor: `input_t_session = input_row.timestamp - first_input_ts`.
        let frame_t_session_sec = frame.t_ns as f64 / 1_000_000_000.0;

        // Apply all inputs whose session-relative timestamp <= this frame.
        while input_idx < inputs.len() {
            let row = &inputs[input_idx];
            let row_session_sec = if let Some(first) = inputs.first() {
                row.timestamp - first.timestamp
            } else {
                row.timestamp
            };
            if row_session_sec > frame_t_session_sec {
                break;
            }

            match row.event_type.as_str() {
                "MOUSE_MOVE" => {
                    if let Some(args) = row.event_args.as_array()
                        && args.len() >= 2
                        && let (Some(dx), Some(dy)) = (args[0].as_i64(), args[1].as_i64())
                    {
                        cursor_x = (cursor_x + dx as f64).clamp(0.0, screen_w_f);
                        cursor_y = (cursor_y + dy as f64).clamp(0.0, screen_h_f);
                    }
                }
                "KEYBOARD" => {
                    if let Some(args) = row.event_args.as_array()
                        && args.len() >= 2
                        && let (Some(keycode), Some(pressed)) =
                            (args[0].as_u64(), args[1].as_bool())
                    {
                        // VK codes fit in u16; defensively clamp out-of-range.
                        let kc = keycode as u16;
                        if pressed {
                            held.insert(kc);
                        } else {
                            held.remove(&kc);
                        }
                    }
                }
                "GAMEPAD_AXIS" => {
                    if let Some(args) = row.event_args.as_array()
                        && args.len() >= 2
                        && let (Some(axis_idx_u), Some(value)) =
                            (args[0].as_u64(), args[1].as_f64())
                    {
                        let axis_idx = axis_idx_u as u16;
                        match axis_idx {
                            AXIS_LSTICKX => axis_lstick_x = value.clamp(-1.0, 1.0),
                            AXIS_LSTICKY => axis_lstick_y = value.clamp(-1.0, 1.0),
                            AXIS_RSTICKX => axis_rstick_x = value.clamp(-1.0, 1.0),
                            AXIS_RSTICKY => axis_rstick_y = value.clamp(-1.0, 1.0),
                            AXIS_LEFTZ => axis_left_trigger = value.clamp(0.0, 1.0),
                            AXIS_RIGHTZ => axis_right_trigger = value.clamp(0.0, 1.0),
                            _ => {
                                // DPAD-as-axis or unknown axis — ignored;
                                // DPAD also fires as button events.
                            }
                        }
                    }
                }
                "GAMEPAD_BUTTON" => {
                    if let Some(args) = row.event_args.as_array()
                        && args.len() >= 2
                        && let (Some(btn_idx_u), Some(pressed)) =
                            (args[0].as_u64(), args[1].as_bool())
                    {
                        let btn_idx = btn_idx_u as u16;
                        if pressed {
                            held_pad_buttons.insert(btn_idx);
                        } else {
                            held_pad_buttons.remove(&btn_idx);
                        }
                    }
                }
                _ => {
                    // START / END / VIDEO_* / MOUSE_BUTTON / SCROLL /
                    // GAMEPAD_BUTTON_VALUE / HOOK_START / FOCUS / UNFOCUS
                    // are not used to compute the per-frame state and are
                    // intentionally ignored here. They remain available in
                    // inputs.jsonl for any consumer that wants the lossless
                    // event stream.
                }
            }

            input_idx += 1;
        }

        let mut bitmask: u16 = 0;
        for btn in held_pad_buttons.iter() {
            bitmask |= gilrs_button_to_xinput_bit(*btn);
        }

        records.push(ActionCameraRecord {
            frame_index: frame.idx,
            timestamp: frame_t_session_sec,
            input_modality: modality,
            mouse_x: if has_kbm {
                Some(cursor_x / screen_w_f)
            } else {
                None
            },
            mouse_y: if has_kbm {
                Some(cursor_y / screen_h_f)
            } else {
                None
            },
            mouse_dx: if has_kbm {
                Some(cursor_x - prev_x)
            } else {
                None
            },
            mouse_dy: if has_kbm {
                Some(cursor_y - prev_y)
            } else {
                None
            },
            key_code: if has_kbm {
                Some(held.iter().copied().collect())
            } else {
                None
            },
            gamepad_left_stick_x: if has_pad { Some(axis_lstick_x) } else { None },
            gamepad_left_stick_y: if has_pad { Some(axis_lstick_y) } else { None },
            gamepad_right_stick_x: if has_pad { Some(axis_rstick_x) } else { None },
            gamepad_right_stick_y: if has_pad { Some(axis_rstick_y) } else { None },
            gamepad_left_trigger: if has_pad {
                Some(axis_left_trigger)
            } else {
                None
            },
            gamepad_right_trigger: if has_pad {
                Some(axis_right_trigger)
            } else {
                None
            },
            gamepad_buttons: if has_pad { Some(bitmask) } else { None },
            camera_position: None,
            camera_rotation_quaternion: None,
        });

        prev_x = cursor_x;
        prev_y = cursor_y;
    }

    records
}

/// Read `inputs.jsonl` from disk into a `Vec<InputRow>`. Tolerates blank
/// lines and lines that don't parse — same forgiving policy as the Python
/// adapter, since a single corrupt event in the middle of a recording
/// shouldn't kill the whole `action_camera.json` output.
fn read_inputs_jsonl(path: &Path) -> std::io::Result<Vec<InputRow>> {
    let raw = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<InputRow>(line) {
            out.push(row);
        }
    }
    Ok(out)
}

/// Read `frames.jsonl` from disk into a `Vec<FrameRow>`. Same tolerant
/// parsing as `read_inputs_jsonl`.
fn read_frames_jsonl(path: &Path) -> std::io::Result<Vec<FrameRow>> {
    let raw = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<FrameRow>(line) {
            out.push(row);
        }
    }
    Ok(out)
}

/// Compose `action_camera.json` from `inputs.jsonl` + `frames.jsonl` in the
/// session directory and write it durably to `action_camera.json`.
///
/// Returns `Ok(record_count)` on success. Failures are surfaced; the caller
/// in `Recording::stop` logs a warning and continues — `action_camera.json`
/// is additive and its absence must never invalidate the recording.
pub async fn write_action_camera_json(
    session_dir: &Path,
    screen_w: u32,
    screen_h: u32,
) -> Result<usize> {
    let inputs_path = session_dir.join(constants::filename::recording::INPUTS);
    let frames_path = session_dir.join(constants::filename::recording::FRAMES_JSONL);
    let out_path = session_dir.join(constants::filename::recording::ACTION_CAMERA_JSON);

    // Run the synchronous CPU work (read + parse + replay + serialize) on a
    // blocking thread so we don't hold the tokio reactor through what could
    // be tens of MB of JSON for a long session.
    let session_dir_owned = session_dir.to_path_buf();
    let (bytes, count) = tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, usize)> {
        let inputs = read_inputs_jsonl(&inputs_path)
            .with_context(|| format!("read {}", inputs_path.display()))?;
        let frames = read_frames_jsonl(&frames_path)
            .with_context(|| format!("read {}", frames_path.display()))?;

        let records = build_records(&inputs, &frames, screen_w, screen_h);
        let count = records.len();

        // Buyer plugin expects a top-level JSON array — not an array under a
        // key, not JSON-Lines. `serde_json::to_vec` (compact) is what the
        // wire contract specifies.
        let bytes = serde_json::to_vec(&records).wrap_err("serialize action_camera records")?;
        Ok((bytes, count))
    })
    .await
    .wrap_err("action_camera.json builder task panicked")??;

    let bytes_len = bytes.len();
    durable_write::write_atomic_async(&out_path, bytes)
        .await
        .with_context(|| format!("write {}", out_path.display()))?;

    tracing::info!(
        "action_camera.json saved: {count} records, {bytes_len} bytes to {}, session_dir={}",
        out_path.display(),
        session_dir_owned.display()
    );

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a synthetic InputRow (event_type + raw args).
    fn input_row(timestamp: f64, event_type: &str, args: serde_json::Value) -> InputRow {
        InputRow {
            timestamp,
            event_type: event_type.to_string(),
            event_args: args,
        }
    }

    /// Helper: build a synthetic FrameRow.
    fn frame_row(idx: u64, t_ns: u64) -> FrameRow {
        FrameRow { idx, t_ns }
    }

    #[test]
    fn cursor_starts_at_center_when_no_inputs() {
        // No events, single frame at t=0. Cursor must be reported at the
        // exact center, with zero per-frame delta.
        // No events → modality falls back to keyboard_mouse (legacy schema).
        let frames = vec![frame_row(0, 0)];
        let inputs: Vec<InputRow> = vec![];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].input_modality, InputModality::KeyboardMouse);
        assert!((recs[0].mouse_x.unwrap() - 0.5).abs() < 1e-12);
        assert!((recs[0].mouse_y.unwrap() - 0.5).abs() < 1e-12);
        assert_eq!(recs[0].mouse_dx, Some(0.0));
        assert_eq!(recs[0].mouse_dy, Some(0.0));
        assert!(recs[0].key_code.as_ref().unwrap().is_empty());
        // gamepad fields null on KBM modality.
        assert!(recs[0].gamepad_left_stick_x.is_none());
        assert!(recs[0].gamepad_buttons.is_none());
        assert!(recs[0].camera_position.is_none());
        assert!(recs[0].camera_rotation_quaternion.is_none());
        assert_eq!(recs[0].frame_index, 0);
        assert_eq!(recs[0].timestamp, 0.0);
    }

    #[test]
    fn mouse_move_accumulates_in_pixels_and_normalizes() {
        // Frame 0 at t=0 (no events applied yet — cursor still at center).
        // Frame 1 at t=1/30s, with two MOUSE_MOVE events totalling (+12, +7).
        // Inputs.jsonl uses wall-clock timestamps with the first event as
        // the session anchor. We synthesize input timestamps so the first
        // is at wall-clock 1000.0 (anchor) and the moves are at 1000.01 /
        // 1000.02 — both before frame 1's session-relative t = 0.0333s.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.010, "MOUSE_MOVE", serde_json::json!([10, 5])),
            input_row(1000.020, "MOUSE_MOVE", serde_json::json!([2, 2])),
        ];
        let frames = vec![
            frame_row(0, 0),
            frame_row(1, 33_333_333), // ~30 fps
        ];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs.len(), 2);
        // Frame 0 at t=0: only the START event (timestamp 0 session) has
        // been applied; cursor is still at center, delta is 0.
        assert_eq!(recs[0].input_modality, InputModality::KeyboardMouse);
        assert!((recs[0].mouse_x.unwrap() - 0.5).abs() < 1e-12);
        assert!((recs[0].mouse_y.unwrap() - 0.5).abs() < 1e-12);
        assert_eq!(recs[0].mouse_dx, Some(0.0));
        // Frame 1: cursor accumulated +12, +7 in pixels.
        let expect_x = (1920.0 / 2.0 + 12.0) / 1920.0;
        let expect_y = (1080.0 / 2.0 + 7.0) / 1080.0;
        assert!((recs[1].mouse_x.unwrap() - expect_x).abs() < 1e-12);
        assert!((recs[1].mouse_y.unwrap() - expect_y).abs() < 1e-12);
        // mouse_dx / mouse_dy are per-frame pixel deltas (NOT normalized).
        assert!((recs[1].mouse_dx.unwrap() - 12.0).abs() < 1e-12);
        assert!((recs[1].mouse_dy.unwrap() - 7.0).abs() < 1e-12);
    }

    #[test]
    fn cursor_clamps_at_screen_edges() {
        // Negative delta past the left edge clamps to 0; large positive
        // delta past the right edge clamps to screen_w. Mirrors the Python
        // adapter's clamp behaviour.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "MOUSE_MOVE", serde_json::json!([-5000, -5000])),
        ];
        let frames = vec![frame_row(0, 16_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert!((recs[0].mouse_x.unwrap() - 0.0).abs() < 1e-12);
        assert!((recs[0].mouse_y.unwrap() - 0.0).abs() < 1e-12);

        let inputs2 = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "MOUSE_MOVE", serde_json::json!([5000, 5000])),
        ];
        let recs2 = build_records(&inputs2, &frames, 1920, 1080);
        assert!((recs2[0].mouse_x.unwrap() - 1.0).abs() < 1e-12);
        assert!((recs2[0].mouse_y.unwrap() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn keyboard_held_set_transitions() {
        // Press W and A before frame 0; release A and press D before frame
        // 1; release everything before frame 2. Held set must reflect each
        // transition AT each frame's timestamp.
        // VK codes: W=87, A=65, D=68, SHIFT=16
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "KEYBOARD", serde_json::json!([87, true])), // W down
            input_row(1000.002, "KEYBOARD", serde_json::json!([65, true])), // A down
            input_row(1000.020, "KEYBOARD", serde_json::json!([65, false])), // A up
            input_row(1000.021, "KEYBOARD", serde_json::json!([68, true])), // D down
            input_row(1000.040, "KEYBOARD", serde_json::json!([87, false])), // W up
            input_row(1000.041, "KEYBOARD", serde_json::json!([68, false])), // D up
        ];
        let frames = vec![
            frame_row(0, 10_000_000), // 0.010s — W and A held
            frame_row(1, 30_000_000), // 0.030s — W and D held
            frame_row(2, 50_000_000), // 0.050s — none held
        ];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs[0].key_code, Some(vec![65, 87])); // A, W (sorted ascending)
        assert_eq!(recs[1].key_code, Some(vec![68, 87])); // D, W
        assert!(recs[2].key_code.as_ref().unwrap().is_empty());
    }

    #[test]
    fn keyboard_repeated_press_idempotent() {
        // Some games / OS-level repeat keys can deliver multiple `pressed=true`
        // for the same VK. The held-set must still contain a single entry.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "KEYBOARD", serde_json::json!([87, true])),
            input_row(1000.002, "KEYBOARD", serde_json::json!([87, true])),
            input_row(1000.003, "KEYBOARD", serde_json::json!([87, true])),
        ];
        let frames = vec![frame_row(0, 10_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs[0].key_code, Some(vec![87]));
    }

    #[test]
    fn keyboard_release_without_press_does_not_panic() {
        // Defensive: a `pressed=false` for a key that was never pressed
        // (e.g. recording started mid-key-hold and we missed the down event)
        // must not panic, and must leave the held set empty.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "KEYBOARD", serde_json::json!([87, false])),
        ];
        let frames = vec![frame_row(0, 10_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert!(recs[0].key_code.as_ref().unwrap().is_empty());
    }

    #[test]
    fn unrelated_events_do_not_affect_cursor_or_held_keys() {
        // MOUSE_BUTTON / SCROLL / VIDEO_* / HOOK_START must pass through
        // without mutating cursor or held-key state. NOTE: GAMEPAD_BUTTON
        // is now a state-mutating event in `mixed` modality (KBM events
        // present here keep us in the kbm/mixed set), so this test omits
        // gamepad events to retain the original semantics.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "MOUSE_BUTTON", serde_json::json!([1, true])),
            input_row(1000.002, "SCROLL", serde_json::json!([3])),
            input_row(1000.004, "VIDEO_START", serde_json::json!([])),
            input_row(1000.005, "HOOK_START", serde_json::json!([])),
        ];
        let frames = vec![frame_row(0, 10_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs[0].input_modality, InputModality::KeyboardMouse);
        assert!((recs[0].mouse_x.unwrap() - 0.5).abs() < 1e-12);
        assert!((recs[0].mouse_y.unwrap() - 0.5).abs() < 1e-12);
        assert!(recs[0].key_code.as_ref().unwrap().is_empty());
        // Gamepad fields remain null on KBM modality.
        assert!(recs[0].gamepad_buttons.is_none());
    }

    #[test]
    fn malformed_event_args_are_skipped() {
        // A MOUSE_MOVE with a string instead of a number, or a KEYBOARD with
        // missing args, must NOT panic and must NOT corrupt the cursor /
        // held-key state. They're treated as no-ops.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "MOUSE_MOVE", serde_json::json!(["bogus", 5])),
            input_row(1000.002, "KEYBOARD", serde_json::json!([87])), // missing pressed
            input_row(1000.003, "KEYBOARD", serde_json::json!([87, true])), // valid
            input_row(1000.004, "MOUSE_MOVE", serde_json::json!([5, 5])), // valid
        ];
        let frames = vec![frame_row(0, 10_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        // The two valid events: cursor moves +5,+5 and W is held.
        let expect_x = (1920.0 / 2.0 + 5.0) / 1920.0;
        let expect_y = (1080.0 / 2.0 + 5.0) / 1080.0;
        assert!((recs[0].mouse_x.unwrap() - expect_x).abs() < 1e-12);
        assert!((recs[0].mouse_y.unwrap() - expect_y).abs() < 1e-12);
        assert_eq!(recs[0].key_code, Some(vec![87]));
    }

    #[test]
    fn serialized_shape_matches_buyer_contract() {
        // Round-trip a record through serde_json and verify the field names
        // and types match the buyer's exact wire contract — mouseX/mouseY
        // (camelCase), keyCode (camelCase), camera_position (snake_case,
        // null), gamepad_* (snake_case) — every documented field appears.
        let rec = ActionCameraRecord {
            frame_index: 1,
            timestamp: 0.0333,
            input_modality: InputModality::KeyboardMouse,
            mouse_x: Some(0.506),
            mouse_y: Some(0.507),
            mouse_dx: Some(12.0),
            mouse_dy: Some(7.0),
            key_code: Some(vec![65, 87]),
            gamepad_left_stick_x: None,
            gamepad_left_stick_y: None,
            gamepad_right_stick_x: None,
            gamepad_right_stick_y: None,
            gamepad_left_trigger: None,
            gamepad_right_trigger: None,
            gamepad_buttons: None,
            camera_position: None,
            camera_rotation_quaternion: None,
        };
        let json = serde_json::to_value(&rec).expect("serialize");
        // Object keys
        let obj = json.as_object().expect("record is object");
        assert!(obj.contains_key("frame_index"));
        assert!(obj.contains_key("timestamp"));
        assert!(obj.contains_key("input_modality"));
        assert!(obj.contains_key("mouseX"));
        assert!(obj.contains_key("mouseY"));
        assert!(obj.contains_key("mouse_dx"));
        assert!(obj.contains_key("mouse_dy"));
        assert!(obj.contains_key("keyCode"));
        assert!(obj.contains_key("gamepad_left_stick_x"));
        assert!(obj.contains_key("gamepad_left_stick_y"));
        assert!(obj.contains_key("gamepad_right_stick_x"));
        assert!(obj.contains_key("gamepad_right_stick_y"));
        assert!(obj.contains_key("gamepad_left_trigger"));
        assert!(obj.contains_key("gamepad_right_trigger"));
        assert!(obj.contains_key("gamepad_buttons"));
        assert!(obj.contains_key("camera_position"));
        assert!(obj.contains_key("camera_rotation_quaternion"));
        // input_modality renders as snake_case string.
        assert_eq!(obj["input_modality"].as_str(), Some("keyboard_mouse"));
        // Nullable camera fields render as JSON null (NOT omitted).
        assert!(obj["camera_position"].is_null());
        assert!(obj["camera_rotation_quaternion"].is_null());
        // gamepad fields render as JSON null when None.
        assert!(obj["gamepad_left_stick_x"].is_null());
        assert!(obj["gamepad_buttons"].is_null());
        // keyCode is a JSON array of integers.
        assert!(obj["keyCode"].is_array());
        assert_eq!(obj["keyCode"][0].as_u64(), Some(65));
    }

    #[test]
    fn empty_frames_produces_empty_array() {
        // Zero frames in frames.jsonl (e.g. recording stopped before the
        // first frame was captured) must produce an empty top-level array,
        // not a null or a malformed file.
        let inputs = vec![input_row(1000.0, "START", serde_json::json!([]))];
        let frames: Vec<FrameRow> = vec![];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert!(recs.is_empty());
        let json = serde_json::to_string(&recs).expect("serialize");
        assert_eq!(json, "[]");
    }

    #[test]
    fn screen_zero_dimensions_does_not_divide_by_zero() {
        // Defensive: if game_resolution somehow comes through as (0, 0)
        // (shouldn't happen but the type allows it), normalization must
        // not produce NaN — we'd be writing invalid JSON.
        let inputs = vec![input_row(1000.0, "START", serde_json::json!([]))];
        let frames = vec![frame_row(0, 0)];
        let recs = build_records(&inputs, &frames, 0, 0);
        assert!(recs[0].mouse_x.unwrap().is_finite());
        assert!(recs[0].mouse_y.unwrap().is_finite());
    }

    #[test]
    fn jsonl_parser_tolerates_blank_and_garbage_lines() {
        // Mid-recording disk hiccup or partially-written line shouldn't kill
        // the whole conversion. Blank lines, comment-style lines (`#...`),
        // and lines that fail to parse as JSON are skipped silently —
        // matching the Python adapter's tolerance.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("inputs.jsonl");
        std::fs::write(
            &path,
            "\
{\"timestamp\":1000.0,\"event_type\":\"START\",\"event_args\":[]}
\n
# this is a comment
{not json
{\"timestamp\":1000.001,\"event_type\":\"MOUSE_MOVE\",\"event_args\":[10,5]}
",
        )
        .expect("write");
        let parsed = read_inputs_jsonl(&path).expect("parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].event_type, "START");
        assert_eq!(parsed[1].event_type, "MOUSE_MOVE");
    }

    #[tokio::test]
    async fn end_to_end_writes_action_camera_json_to_session_dir() {
        // Full happy-path: synthesize an `inputs.jsonl` and `frames.jsonl`
        // in a session tempdir, call `write_action_camera_json`, and verify
        // the resulting `action_camera.json` parses back to the expected
        // shape AND contains the expected per-frame state.
        let dir = tempfile::tempdir().expect("tempdir");

        // Synthesize inputs.jsonl: START, then W down, then a +12,+7 mouse move.
        let inputs_path = dir.path().join(constants::filename::recording::INPUTS);
        std::fs::write(
            &inputs_path,
            "\
{\"timestamp\":1000.000,\"event_type\":\"START\",\"event_args\":[]}
{\"timestamp\":1000.005,\"event_type\":\"KEYBOARD\",\"event_args\":[87,true]}
{\"timestamp\":1000.010,\"event_type\":\"MOUSE_MOVE\",\"event_args\":[12,7]}
",
        )
        .expect("write inputs");

        // Synthesize frames.jsonl: two frames at 0 and ~0.0333s.
        let frames_path = dir
            .path()
            .join(constants::filename::recording::FRAMES_JSONL);
        std::fs::write(
            &frames_path,
            "\
{\"idx\":0,\"t_ns\":0}
{\"idx\":1,\"t_ns\":33333333}
",
        )
        .expect("write frames");

        let count = write_action_camera_json(dir.path(), 1920, 1080)
            .await
            .expect("write_action_camera_json");
        assert_eq!(count, 2);

        // Read back the file and verify shape.
        let out_path = dir
            .path()
            .join(constants::filename::recording::ACTION_CAMERA_JSON);
        assert!(
            out_path.exists(),
            "action_camera.json must exist after write"
        );
        let raw = std::fs::read_to_string(&out_path).expect("read out");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse out");
        let arr = parsed.as_array().expect("top-level array");
        assert_eq!(arr.len(), 2);

        // Frame 0: cursor at center, no held keys yet (events at session
        // t=0.005 and t=0.010 are AFTER frame 0's t_ns=0, so they have
        // not been applied).
        assert_eq!(arr[0]["frame_index"].as_u64(), Some(0));
        assert_eq!(arr[0]["input_modality"].as_str(), Some("keyboard_mouse"));
        assert!((arr[0]["mouseX"].as_f64().unwrap() - 0.5).abs() < 1e-9);
        assert!((arr[0]["mouseY"].as_f64().unwrap() - 0.5).abs() < 1e-9);
        assert!(arr[0]["keyCode"].as_array().unwrap().is_empty());
        // gamepad fields are null on a KBM session.
        assert!(arr[0]["gamepad_left_stick_x"].is_null());
        assert!(arr[0]["gamepad_buttons"].is_null());
        assert!(arr[0]["camera_position"].is_null());
        assert!(arr[0]["camera_rotation_quaternion"].is_null());

        // Frame 1: W down (87) was at session t=0.005, mouse +12,+7 at
        // session t=0.010 — both before frame 1's t=0.0333s.
        assert_eq!(arr[1]["frame_index"].as_u64(), Some(1));
        let key_codes: Vec<u64> = arr[1]["keyCode"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(key_codes, vec![87]);
        let expect_x = (1920.0 / 2.0 + 12.0) / 1920.0;
        let expect_y = (1080.0 / 2.0 + 7.0) / 1080.0;
        assert!((arr[1]["mouseX"].as_f64().unwrap() - expect_x).abs() < 1e-9);
        assert!((arr[1]["mouseY"].as_f64().unwrap() - expect_y).abs() < 1e-9);
        // mouse_dx/dy are per-frame pixel deltas.
        assert!((arr[1]["mouse_dx"].as_f64().unwrap() - 12.0).abs() < 1e-9);
        assert!((arr[1]["mouse_dy"].as_f64().unwrap() - 7.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn end_to_end_propagates_io_error_when_inputs_missing() {
        // If inputs.jsonl is missing entirely (e.g. recording was killed
        // before InputEventWriter::start completed), the writer must
        // surface the error rather than write a misleading partial file.
        let dir = tempfile::tempdir().expect("tempdir");
        // Only frames.jsonl present.
        std::fs::write(
            dir.path()
                .join(constants::filename::recording::FRAMES_JSONL),
            "{\"idx\":0,\"t_ns\":0}\n",
        )
        .expect("write frames");
        let result = write_action_camera_json(dir.path(), 1920, 1080).await;
        assert!(result.is_err(), "expected error on missing inputs.jsonl");
    }

    // --------------------------------------------------------------- gamepad

    #[test]
    fn gamepad_only_session_nulls_kbm_fields() {
        // A pure-gamepad recording: only gamepad events present. The
        // mouse / keyboard fields must serialize as `null` and the
        // gamepad fields must carry real values.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            // Left stick deflected up-right; gilrs AXIS_LSTICKX=1,
            // AXIS_LSTICKY=2.
            input_row(1000.001, "GAMEPAD_AXIS", serde_json::json!([1, 0.5])),
            input_row(1000.002, "GAMEPAD_AXIS", serde_json::json!([2, -0.25])),
            // Right trigger pulled.
            input_row(1000.003, "GAMEPAD_AXIS", serde_json::json!([6, 0.75])),
            // A button (gilrs BTN_SOUTH=1) and START (gilrs idx 12) held.
            input_row(1000.004, "GAMEPAD_BUTTON", serde_json::json!([1, true])),
            input_row(1000.005, "GAMEPAD_BUTTON", serde_json::json!([12, true])),
        ];
        let frames = vec![frame_row(0, 10_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs[0].input_modality, InputModality::Gamepad);
        // KBM fields nulled out.
        assert!(recs[0].mouse_x.is_none());
        assert!(recs[0].mouse_y.is_none());
        assert!(recs[0].mouse_dx.is_none());
        assert!(recs[0].mouse_dy.is_none());
        assert!(recs[0].key_code.is_none());
        // Gamepad fields populated.
        assert_eq!(recs[0].gamepad_left_stick_x, Some(0.5));
        assert_eq!(recs[0].gamepad_left_stick_y, Some(-0.25));
        assert_eq!(recs[0].gamepad_right_stick_x, Some(0.0));
        assert_eq!(recs[0].gamepad_right_stick_y, Some(0.0));
        assert_eq!(recs[0].gamepad_left_trigger, Some(0.0));
        assert_eq!(recs[0].gamepad_right_trigger, Some(0.75));
        // A=0x1000 | START=0x0010 = 0x1010
        assert_eq!(recs[0].gamepad_buttons, Some(0x1010));
    }

    #[test]
    fn mixed_modality_populates_all_fields() {
        // Both keyboard and gamepad events present → modality=mixed,
        // both field families populated.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "KEYBOARD", serde_json::json!([87, true])), // W
            input_row(1000.002, "GAMEPAD_AXIS", serde_json::json!([1, 0.3])),
            input_row(1000.003, "GAMEPAD_BUTTON", serde_json::json!([2, true])), // B
        ];
        let frames = vec![frame_row(0, 10_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs[0].input_modality, InputModality::Mixed);
        assert_eq!(recs[0].key_code, Some(vec![87]));
        assert!(recs[0].mouse_x.is_some());
        assert_eq!(recs[0].gamepad_left_stick_x, Some(0.3));
        // B = 0x2000
        assert_eq!(recs[0].gamepad_buttons, Some(0x2000));
    }

    #[test]
    fn gamepad_axis_values_are_clamped() {
        // Stick events outside [-1, 1] and trigger events outside [0, 1]
        // must be clamped silently — the recorder occasionally reports
        // slightly out-of-range values from gilrs.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "GAMEPAD_AXIS", serde_json::json!([1, 5.0])), // overflow stick
            input_row(1000.002, "GAMEPAD_AXIS", serde_json::json!([2, -3.0])), // underflow stick
            input_row(1000.003, "GAMEPAD_AXIS", serde_json::json!([3, -0.5])), // underflow trigger
            input_row(1000.004, "GAMEPAD_AXIS", serde_json::json!([6, 2.0])), // overflow trigger
        ];
        let frames = vec![frame_row(0, 10_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs[0].gamepad_left_stick_x, Some(1.0));
        assert_eq!(recs[0].gamepad_left_stick_y, Some(-1.0));
        assert_eq!(recs[0].gamepad_left_trigger, Some(0.0));
        assert_eq!(recs[0].gamepad_right_trigger, Some(1.0));
    }

    #[test]
    fn gamepad_buttons_xinput_mapping_is_correct() {
        // Spot-check the gilrs→XInput mapping used by `gamepad_buttons`.
        // We press one button per documented mapping and verify the OR'd
        // bitmask matches the table in ACTION_CAMERA_FORMAT.md.
        // Pairs: (gilrs_idx, xinput_bit).
        let pairs: &[(u16, u16)] = &[
            (1, 0x1000),  // A
            (2, 0x2000),  // B
            (4, 0x8000),  // Y
            (5, 0x4000),  // X
            (7, 0x0100),  // LB
            (8, 0x0200),  // RB
            (11, 0x0020), // BACK
            (12, 0x0010), // START
            (14, 0x0040), // LSTICK
            (15, 0x0080), // RSTICK
            (16, 0x0001), // DPAD_UP
            (17, 0x0002), // DPAD_DOWN
            (18, 0x0004), // DPAD_LEFT
            (19, 0x0008), // DPAD_RIGHT
        ];
        let mut expected: u16 = 0;
        let mut inputs = vec![input_row(1000.0, "START", serde_json::json!([]))];
        let mut t = 1000.001f64;
        for (idx, bit) in pairs {
            inputs.push(input_row(
                t,
                "GAMEPAD_BUTTON",
                serde_json::json!([*idx, true]),
            ));
            t += 0.001;
            expected |= bit;
        }
        let frames = vec![frame_row(0, 100_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs[0].gamepad_buttons, Some(expected));
        assert_eq!(expected, 0xF3FF); // sanity: all 14 mapped slots set
    }

    #[test]
    fn unknown_gamepad_button_indices_drop_from_bitmask() {
        // gilrs indices that don't appear in the XInput map (e.g.
        // BTN_MODE=13, BTN_C=3, BTN_Z=6, BTN_LT2=9, BTN_RT2=10) must
        // contribute nothing to `gamepad_buttons`. The bitmask stays 0
        // when only unmapped buttons are held.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "GAMEPAD_BUTTON", serde_json::json!([13, true])), // MODE
            input_row(1000.002, "GAMEPAD_BUTTON", serde_json::json!([3, true])),  // C
            input_row(1000.003, "GAMEPAD_BUTTON", serde_json::json!([9, true])),  // LT2
        ];
        let frames = vec![frame_row(0, 10_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        // Modality is gamepad (we have gamepad events) — bitmask is Some(0).
        assert_eq!(recs[0].input_modality, InputModality::Gamepad);
        assert_eq!(recs[0].gamepad_buttons, Some(0));
    }

    #[test]
    fn gamepad_button_release_without_press_does_not_panic() {
        // Defensive: a `pressed=false` for a button we never saw `true`
        // for (e.g. recording started with the button already held) must
        // be a no-op, not a panic. Mirrors keyboard_release_without_press_does_not_panic.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "GAMEPAD_BUTTON", serde_json::json!([1, false])),
        ];
        let frames = vec![frame_row(0, 10_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs[0].input_modality, InputModality::Gamepad);
        assert_eq!(recs[0].gamepad_buttons, Some(0));
    }

    #[test]
    fn detect_input_modality_classifies_correctly() {
        // Pure KBM.
        let inputs_kbm = vec![
            input_row(1000.0, "START", serde_json::json!([])),
            input_row(1000.001, "MOUSE_MOVE", serde_json::json!([1, 1])),
            input_row(1000.002, "KEYBOARD", serde_json::json!([87, true])),
        ];
        assert_eq!(
            detect_input_modality(&inputs_kbm),
            InputModality::KeyboardMouse
        );

        // Pure gamepad.
        let inputs_pad = vec![
            input_row(1000.0, "START", serde_json::json!([])),
            input_row(1000.001, "GAMEPAD_AXIS", serde_json::json!([1, 0.5])),
        ];
        assert_eq!(detect_input_modality(&inputs_pad), InputModality::Gamepad);

        // Mixed.
        let inputs_mix = vec![
            input_row(1000.0, "START", serde_json::json!([])),
            input_row(1000.001, "KEYBOARD", serde_json::json!([87, true])),
            input_row(1000.002, "GAMEPAD_AXIS", serde_json::json!([1, 0.5])),
        ];
        assert_eq!(detect_input_modality(&inputs_mix), InputModality::Mixed);

        // Empty / meta-only → fallback to KBM (legacy default).
        let inputs_empty = vec![input_row(1000.0, "START", serde_json::json!([]))];
        assert_eq!(
            detect_input_modality(&inputs_empty),
            InputModality::KeyboardMouse
        );
    }
}
