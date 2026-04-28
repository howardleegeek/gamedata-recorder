//! `action_camera.json` sink — buyer plugin wire contract.
//!
//! The buyer's training plugin requires per-frame records joining mouse,
//! keyboard, and (downstream-filled) camera state into a single JSON array.
//! The format is fixed; this module is the on-recorder implementation of
//! the same contract that the post-hoc Python adapter at
//! `oyster-enrichment/bin/convert_to_action_camera.py` implements.
//!
//! ## Why a separate sink (and not modify inputs.jsonl)
//!
//! `inputs.jsonl` is the lossless event stream — sub-frame mouse motion,
//! discrete keyboard up/down events, scrolls, gamepad samples. The buyer's
//! plugin wants per-frame snapshots, with cursor position accumulated and
//! held-keys reduced to a sorted list. Computing that on the recorder side
//! avoids every consumer re-implementing the replay logic, and avoids
//! bundle adapters (the Python script) being a hard dependency on the
//! ingest pipeline.
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
//! ## Schema (per record, in array order)
//!
//! ```json
//! {
//!   "frame_index": <u64>,
//!   "timestamp": <f64 seconds>,
//!   "mouseX": <f64 in [0, 1]>,
//!   "mouseY": <f64 in [0, 1]>,
//!   "mouse_dx": <f64 pixels, per-frame delta>,
//!   "mouse_dy": <f64 pixels, per-frame delta>,
//!   "keyCode": <sorted ascending list of held VK codes at this frame>,
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

/// Per-frame record matching the buyer plugin's wire contract.
///
/// Field naming intentionally uses camelCase for `mouseX`/`mouseY`/`keyCode`
/// (the buyer's spec) and snake_case for `mouse_dx`/`mouse_dy`/`camera_*`
/// (also the buyer's spec). serde tags pin each field name explicitly so
/// the serialized output is identical to the Python adapter regardless of
/// future struct refactors.
#[derive(Debug, Serialize, PartialEq)]
pub struct ActionCameraRecord {
    pub frame_index: u64,
    pub timestamp: f64,
    #[serde(rename = "mouseX")]
    pub mouse_x: f64,
    #[serde(rename = "mouseY")]
    pub mouse_y: f64,
    pub mouse_dx: f64,
    pub mouse_dy: f64,
    #[serde(rename = "keyCode")]
    pub key_code: Vec<u16>,
    /// Always `None` at the recorder layer. Schema preserved so downstream
    /// enrichment can fill it without changing the JSON shape.
    pub camera_position: Option<[f64; 3]>,
    /// Always `None` at the recorder layer. Quaternion convention when
    /// filled: `[w, x, y, z]` (scalar first).
    pub camera_rotation_quaternion: Option<[f64; 4]>,
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
                _ => {
                    // START / END / VIDEO_* / MOUSE_BUTTON / SCROLL / GAMEPAD_*
                    // are not used to compute mouseX/mouseY/keyCode and are
                    // intentionally ignored here. They remain available in
                    // inputs.jsonl for any consumer that wants the lossless
                    // event stream.
                }
            }

            input_idx += 1;
        }

        records.push(ActionCameraRecord {
            frame_index: frame.idx,
            timestamp: frame_t_session_sec,
            mouse_x: cursor_x / screen_w_f,
            mouse_y: cursor_y / screen_h_f,
            mouse_dx: cursor_x - prev_x,
            mouse_dy: cursor_y - prev_y,
            key_code: held.iter().copied().collect(),
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
        let frames = vec![frame_row(0, 0)];
        let inputs: Vec<InputRow> = vec![];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert_eq!(recs.len(), 1);
        assert!((recs[0].mouse_x - 0.5).abs() < 1e-12);
        assert!((recs[0].mouse_y - 0.5).abs() < 1e-12);
        assert_eq!(recs[0].mouse_dx, 0.0);
        assert_eq!(recs[0].mouse_dy, 0.0);
        assert!(recs[0].key_code.is_empty());
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
        assert!((recs[0].mouse_x - 0.5).abs() < 1e-12);
        assert!((recs[0].mouse_y - 0.5).abs() < 1e-12);
        assert_eq!(recs[0].mouse_dx, 0.0);
        // Frame 1: cursor accumulated +12, +7 in pixels.
        let expect_x = (1920.0 / 2.0 + 12.0) / 1920.0;
        let expect_y = (1080.0 / 2.0 + 7.0) / 1080.0;
        assert!((recs[1].mouse_x - expect_x).abs() < 1e-12);
        assert!((recs[1].mouse_y - expect_y).abs() < 1e-12);
        // mouse_dx / mouse_dy are per-frame pixel deltas (NOT normalized).
        assert!((recs[1].mouse_dx - 12.0).abs() < 1e-12);
        assert!((recs[1].mouse_dy - 7.0).abs() < 1e-12);
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
        assert!((recs[0].mouse_x - 0.0).abs() < 1e-12);
        assert!((recs[0].mouse_y - 0.0).abs() < 1e-12);

        let inputs2 = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "MOUSE_MOVE", serde_json::json!([5000, 5000])),
        ];
        let recs2 = build_records(&inputs2, &frames, 1920, 1080);
        assert!((recs2[0].mouse_x - 1.0).abs() < 1e-12);
        assert!((recs2[0].mouse_y - 1.0).abs() < 1e-12);
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
        assert_eq!(recs[0].key_code, vec![65, 87]); // A, W (sorted ascending)
        assert_eq!(recs[1].key_code, vec![68, 87]); // D, W
        assert!(recs[2].key_code.is_empty());
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
        assert_eq!(recs[0].key_code, vec![87]);
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
        assert!(recs[0].key_code.is_empty());
    }

    #[test]
    fn unrelated_events_do_not_affect_cursor_or_held_keys() {
        // MOUSE_BUTTON / SCROLL / GAMEPAD_* / VIDEO_* / HOOK_START all must
        // pass through without mutating cursor or held-key state.
        let inputs = vec![
            input_row(1000.000, "START", serde_json::json!([])),
            input_row(1000.001, "MOUSE_BUTTON", serde_json::json!([1, true])),
            input_row(1000.002, "SCROLL", serde_json::json!([3])),
            input_row(1000.003, "GAMEPAD_BUTTON", serde_json::json!([1, true])),
            input_row(1000.004, "VIDEO_START", serde_json::json!([])),
            input_row(1000.005, "HOOK_START", serde_json::json!([])),
        ];
        let frames = vec![frame_row(0, 10_000_000)];
        let recs = build_records(&inputs, &frames, 1920, 1080);
        assert!((recs[0].mouse_x - 0.5).abs() < 1e-12);
        assert!((recs[0].mouse_y - 0.5).abs() < 1e-12);
        assert!(recs[0].key_code.is_empty());
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
        assert!((recs[0].mouse_x - expect_x).abs() < 1e-12);
        assert!((recs[0].mouse_y - expect_y).abs() < 1e-12);
        assert_eq!(recs[0].key_code, vec![87]);
    }

    #[test]
    fn serialized_shape_matches_buyer_contract() {
        // Round-trip a record through serde_json and verify the field names
        // and types match the buyer's exact wire contract — mouseX/mouseY
        // (camelCase), keyCode (camelCase), camera_position (snake_case,
        // null), etc.
        let rec = ActionCameraRecord {
            frame_index: 1,
            timestamp: 0.0333,
            mouse_x: 0.506,
            mouse_y: 0.507,
            mouse_dx: 12.0,
            mouse_dy: 7.0,
            key_code: vec![65, 87],
            camera_position: None,
            camera_rotation_quaternion: None,
        };
        let json = serde_json::to_value(&rec).expect("serialize");
        // Object keys
        let obj = json.as_object().expect("record is object");
        assert!(obj.contains_key("frame_index"));
        assert!(obj.contains_key("timestamp"));
        assert!(obj.contains_key("mouseX"));
        assert!(obj.contains_key("mouseY"));
        assert!(obj.contains_key("mouse_dx"));
        assert!(obj.contains_key("mouse_dy"));
        assert!(obj.contains_key("keyCode"));
        assert!(obj.contains_key("camera_position"));
        assert!(obj.contains_key("camera_rotation_quaternion"));
        // Nullable camera fields render as JSON null (NOT omitted).
        assert!(obj["camera_position"].is_null());
        assert!(obj["camera_rotation_quaternion"].is_null());
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
        assert!(recs[0].mouse_x.is_finite());
        assert!(recs[0].mouse_y.is_finite());
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
        assert!((arr[0]["mouseX"].as_f64().unwrap() - 0.5).abs() < 1e-9);
        assert!((arr[0]["mouseY"].as_f64().unwrap() - 0.5).abs() < 1e-9);
        assert!(arr[0]["keyCode"].as_array().unwrap().is_empty());
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
}
