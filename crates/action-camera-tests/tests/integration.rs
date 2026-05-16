//! Integration tests for `action_camera_writer`.
//!
//! Exercises the public surface (`ActionCameraRecord`, `write_action_camera_json`)
//! end-to-end on a real tmpdir. The writer's private replay logic is covered
//! transitively by feeding crafted `inputs.jsonl` + `frames.jsonl` and asserting
//! on the resulting `action_camera.json`.
//!
//! These run on macOS/Linux because this crate deliberately excludes the
//! Windows-only deps of the top-level `gamedata-recorder` crate.

use action_camera_tests::util::durable_write;
use action_camera_tests::{ActionCameraRecord, write_action_camera_json};
use std::path::Path;

/// Helper: write a minimal `inputs.jsonl` and `frames.jsonl` into `dir`, then
/// invoke `write_action_camera_json` and read back the resulting array.
async fn run_writer(
    dir: &Path,
    inputs_jsonl: &str,
    frames_jsonl: &str,
    screen_w: u32,
    screen_h: u32,
) -> serde_json::Value {
    std::fs::write(
        dir.join(constants::filename::recording::INPUTS),
        inputs_jsonl,
    )
    .expect("write inputs.jsonl");
    std::fs::write(
        dir.join(constants::filename::recording::FRAMES_JSONL),
        frames_jsonl,
    )
    .expect("write frames.jsonl");

    write_action_camera_json(dir, screen_w, screen_h)
        .await
        .expect("write_action_camera_json");

    let out = dir.join(constants::filename::recording::ACTION_CAMERA_JSON);
    let raw = std::fs::read_to_string(&out).expect("read action_camera.json");
    serde_json::from_str(&raw).expect("parse action_camera.json")
}

#[tokio::test]
async fn cursor_accumulates_pixel_deltas_across_frames() {
    // Two frames spaced ~33 ms apart. Between them, two MOUSE_MOVE events
    // sum to (+12, +7) pixels. The cursor at frame 1 must reflect the
    // accumulated position; mouse_dx / mouse_dy must be the *per-frame*
    // pixel delta (NOT the total since session start).
    let dir = tempfile::tempdir().unwrap();
    let inputs = "\
{\"timestamp\":1000.000,\"event_type\":\"START\",\"event_args\":[]}
{\"timestamp\":1000.010,\"event_type\":\"MOUSE_MOVE\",\"event_args\":[10,5]}
{\"timestamp\":1000.020,\"event_type\":\"MOUSE_MOVE\",\"event_args\":[2,2]}
";
    let frames = "\
{\"idx\":0,\"t_ns\":0}
{\"idx\":1,\"t_ns\":33333333}
";
    let json = run_writer(dir.path(), inputs, frames, 1920, 1080).await;
    let arr = json.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    // Frame 0: pre-move. Cursor at center, zero delta.
    assert!((arr[0]["mouse_x"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert!((arr[0]["mouse_y"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert_eq!(arr[0]["mouse_dx"].as_f64().unwrap(), 0.0);
    assert_eq!(arr[0]["mouse_dy"].as_f64().unwrap(), 0.0);
    // Frame 1: cursor moved +12, +7 in pixels; mouse_dx == 12, mouse_dy == 7.
    let expect_x = (1920.0 / 2.0 + 12.0) / 1920.0;
    let expect_y = (1080.0 / 2.0 + 7.0) / 1080.0;
    assert!((arr[1]["mouse_x"].as_f64().unwrap() - expect_x).abs() < 1e-9);
    assert!((arr[1]["mouse_y"].as_f64().unwrap() - expect_y).abs() < 1e-9);
    assert!((arr[1]["mouse_dx"].as_f64().unwrap() - 12.0).abs() < 1e-9);
    assert!((arr[1]["mouse_dy"].as_f64().unwrap() - 7.0).abs() < 1e-9);
}

#[tokio::test]
async fn keyboard_held_set_is_sorted_ascending() {
    // Press A, W, D in arbitrary order. The output `keyCode` must always be
    // sorted ascending — buyer plugin requirement.
    let dir = tempfile::tempdir().unwrap();
    let inputs = "\
{\"timestamp\":1000.000,\"event_type\":\"START\",\"event_args\":[]}
{\"timestamp\":1000.001,\"event_type\":\"KEYBOARD\",\"event_args\":[87,true]}
{\"timestamp\":1000.002,\"event_type\":\"KEYBOARD\",\"event_args\":[65,true]}
{\"timestamp\":1000.003,\"event_type\":\"KEYBOARD\",\"event_args\":[68,true]}
";
    let frames = "{\"idx\":0,\"t_ns\":10000000}\n";
    let json = run_writer(dir.path(), inputs, frames, 1920, 1080).await;
    let key_codes: Vec<u64> = json[0]["keyCode"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert_eq!(key_codes, vec![65, 68, 87]);
}

#[tokio::test]
async fn camera_fields_are_always_null_at_recorder_layer() {
    // The recorder has no engine-state access; both camera_position and
    // camera_rotation_quaternion MUST serialize as JSON null (not omitted,
    // not [], not {}).
    let dir = tempfile::tempdir().unwrap();
    let inputs = "{\"timestamp\":1000.0,\"event_type\":\"START\",\"event_args\":[]}\n";
    let frames = "{\"idx\":0,\"t_ns\":0}\n";
    let json = run_writer(dir.path(), inputs, frames, 1920, 1080).await;
    let rec = &json[0];
    assert!(
        rec.get("camera_position")
            .map(|v| v.is_null())
            .unwrap_or(false),
        "camera_position must serialize as null, got {:?}",
        rec.get("camera_position")
    );
    assert!(
        rec.get("camera_rotation_quaternion")
            .map(|v| v.is_null())
            .unwrap_or(false),
        "camera_rotation_quaternion must serialize as null, got {:?}",
        rec.get("camera_rotation_quaternion")
    );
}

#[tokio::test]
async fn empty_frames_yields_top_level_empty_array() {
    // Zero-frame recording (e.g. user stopped before first frame) must produce
    // `[]`, not `null`, not a malformed file. This is what the buyer's plugin
    // expects for an empty recording.
    let dir = tempfile::tempdir().unwrap();
    let inputs = "{\"timestamp\":1000.0,\"event_type\":\"START\",\"event_args\":[]}\n";
    let frames = "";
    std::fs::write(
        dir.path().join(constants::filename::recording::INPUTS),
        inputs,
    )
    .unwrap();
    std::fs::write(
        dir.path()
            .join(constants::filename::recording::FRAMES_JSONL),
        frames,
    )
    .unwrap();
    write_action_camera_json(dir.path(), 1920, 1080)
        .await
        .unwrap();
    let raw = std::fs::read_to_string(
        dir.path()
            .join(constants::filename::recording::ACTION_CAMERA_JSON),
    )
    .unwrap();
    assert_eq!(raw, "[]");
}

#[tokio::test]
async fn missing_inputs_jsonl_surfaces_io_error() {
    // If `inputs.jsonl` doesn't exist, the writer must NOT silently produce
    // a partial / misleading file — it must surface the error so the caller
    // can log and skip.
    let dir = tempfile::tempdir().unwrap();
    // Only frames.jsonl present.
    std::fs::write(
        dir.path()
            .join(constants::filename::recording::FRAMES_JSONL),
        "{\"idx\":0,\"t_ns\":0}\n",
    )
    .unwrap();
    let result = write_action_camera_json(dir.path(), 1920, 1080).await;
    assert!(
        result.is_err(),
        "expected error when inputs.jsonl is absent"
    );
    // Also: the output file must NOT have been partially written.
    let out = dir
        .path()
        .join(constants::filename::recording::ACTION_CAMERA_JSON);
    assert!(
        !out.exists(),
        "action_camera.json must not exist when writer errored before serialize"
    );
}

#[tokio::test]
async fn output_is_valid_json_array_with_buyer_field_names() {
    // Round-trip: confirm the on-disk file is a top-level JSON array (not
    // JSON-Lines, not an envelope object) and each record has the buyer's
    // exact field names — mouseX, mouseY, keyCode (camelCase), and
    // mouse_dx, mouse_dy, camera_*, frame_index, timestamp (snake_case).
    let dir = tempfile::tempdir().unwrap();
    let inputs = "\
{\"timestamp\":1000.000,\"event_type\":\"START\",\"event_args\":[]}
{\"timestamp\":1000.001,\"event_type\":\"KEYBOARD\",\"event_args\":[87,true]}
";
    let frames = "{\"idx\":42,\"t_ns\":1000000}\n";
    let json = run_writer(dir.path(), inputs, frames, 1920, 1080).await;
    let arr = json.as_array().expect("top-level must be JSON array");
    assert_eq!(arr.len(), 1);
    let rec = arr[0].as_object().expect("record is object");
    for required in [
        "frame_index",
        "timestamp",
        "mouse_x",
        "mouse_y",
        "mouse_dx",
        "mouse_dy",
        "keyCode",
        "camera_position",
        "camera_rotation_quaternion",
    ] {
        assert!(
            rec.contains_key(required),
            "record missing required field `{required}`: {rec:?}"
        );
    }
    assert_eq!(rec["frame_index"].as_u64(), Some(42));
}

#[tokio::test]
async fn malformed_jsonl_lines_are_silently_skipped() {
    // Mid-recording disk hiccups can leave partially-written lines. The
    // writer must tolerate these the same way the Python adapter does:
    // skip the broken line, keep going, never panic.
    let dir = tempfile::tempdir().unwrap();
    let inputs = "\
{\"timestamp\":1000.000,\"event_type\":\"START\",\"event_args\":[]}
not actually json
{\"timestamp\":1000.001,\"event_type\":\"MOUSE_MOVE\",\"event_args\":[5,5]}

# comment line
{partial line that breaks
{\"timestamp\":1000.002,\"event_type\":\"KEYBOARD\",\"event_args\":[87,true]}
";
    let frames = "\
{\"idx\":0,\"t_ns\":10000000}
nonsense line
{\"idx\":1,\"t_ns\":20000000}
";
    let json = run_writer(dir.path(), inputs, frames, 1920, 1080).await;
    let arr = json.as_array().unwrap();
    // 2 valid frame rows -> 2 records.
    assert_eq!(arr.len(), 2);
    // Both valid input events (mouse +5,+5 and W down) were applied by
    // frame 1's t=20ms, so cursor is shifted and W is held.
    let key_codes: Vec<u64> = arr[1]["keyCode"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert_eq!(key_codes, vec![87]);
    let expect_x = (1920.0 / 2.0 + 5.0) / 1920.0;
    assert!((arr[1]["mouse_x"].as_f64().unwrap() - expect_x).abs() < 1e-9);
}

// -----------------------------------------------------------------------
// durable_write coverage — integration tests on tmpfs.
// These are separate from the in-file `#[cfg(test)]` unit tests in
// durable_write.rs and exercise the public API on a real session-style dir.
// -----------------------------------------------------------------------

#[test]
fn durable_write_atomic_round_trips_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("session.json");
    durable_write::write_atomic(&p, br#"{"k":"v"}"#).unwrap();
    let read = std::fs::read_to_string(&p).unwrap();
    assert_eq!(read, r#"{"k":"v"}"#);
}

#[test]
fn durable_write_atomic_overwrites_existing() {
    // Existing file at the destination must be replaced atomically — either
    // old or new visible at any point, never empty / torn.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("metadata.json");
    std::fs::write(&p, b"old").unwrap();
    durable_write::write_atomic(&p, b"new").unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
}

#[test]
fn durable_write_leaves_no_tmp_residue_on_success() {
    // After a successful rename the `.tmp` sibling must not exist — otherwise
    // we'd accumulate junk in every session dir over time.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("frames.jsonl");
    durable_write::write_atomic(&p, b"data").unwrap();
    let tmp_sibling = dir.path().join("frames.jsonl.tmp");
    assert!(
        !tmp_sibling.exists(),
        "leftover .tmp after successful write"
    );
}

#[tokio::test]
async fn durable_write_async_works_from_tokio_context() {
    // The async wrapper delegates to spawn_blocking. Confirm it actually
    // does the write and returns success when called from a tokio test.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("async-out.json");
    durable_write::write_atomic_async(&p, b"async-data".to_vec())
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "async-data");
}

// -----------------------------------------------------------------------
// ActionCameraRecord direct-construction sanity check.
// -----------------------------------------------------------------------

#[test]
fn action_camera_record_serializes_with_correct_field_names() {
    // Construct a record directly (bypassing the replay logic) and verify
    // serde produces exactly the buyer's wire contract field names.
    //
    // The struct now carries the full PRD schema (frame_number alias,
    // timestamp_ns, intrinsics, rotation_oula, rotation_quaternion,
    // Follow_Offset, player_*, metric_scale) — all of which serialize
    // even when the camera/player pose data isn't available (the pose
    // fields go to null, the scalars to 0.0 / 1.0).
    use action_camera_tests::action_camera_writer::{InputModality, Intrinsics};

    let rec = ActionCameraRecord {
        frame_index: 7,
        frame_number: 7,
        timestamp: 0.123,
        timestamp_ns: 123_000_000,
        // rc17.2 / Stream BD — PRD §4.1 default route classification.
        // Required by Stream BC lint_v3_prd_grounded.py criterion 11.
        route_type: 1,
        input_modality: InputModality::KeyboardMouse,
        mouse_x: Some(0.5),
        mouse_y: Some(0.5),
        mouse_dx: Some(1.5),
        mouse_dy: Some(-2.0),
        key_code: Some(vec![16, 87]),
        gamepad_left_stick_x: None,
        gamepad_left_stick_y: None,
        gamepad_right_stick_x: None,
        gamepad_right_stick_y: None,
        gamepad_left_trigger: None,
        gamepad_right_trigger: None,
        gamepad_buttons: None,
        camera_position: None,
        rotation_oula: None,
        rotation_quaternion: None,
        camera_rotation_quaternion: None,
        follow_offset: None,
        intrinsics: Intrinsics {
            fx: 1543.0,
            fy: 1543.0,
            cx: 960.0,
            cy: 540.0,
        },
        speed: 0.0,
        player_position: None,
        player_rotation_quaternion: None,
        player_speed: 0.0,
        metric_scale: 1.0,
    };
    let v = serde_json::to_value(&rec).unwrap();
    let obj = v.as_object().unwrap();
    assert_eq!(obj["frame_index"].as_u64(), Some(7));
    assert_eq!(obj["frame_number"].as_u64(), Some(7));
    assert!((obj["timestamp"].as_f64().unwrap() - 0.123).abs() < 1e-12);
    assert_eq!(obj["timestamp_ns"].as_u64(), Some(123_000_000));
    assert_eq!(obj["mouse_x"].as_f64(), Some(0.5));
    assert_eq!(obj["mouse_y"].as_f64(), Some(0.5));
    assert_eq!(obj["mouse_dx"].as_f64(), Some(1.5));
    assert_eq!(obj["mouse_dy"].as_f64(), Some(-2.0));
    assert!(obj["camera_position"].is_null());
    assert!(obj["camera_rotation_quaternion"].is_null());
    assert!(obj["camera_rotation_oula"].is_null());
    assert!(obj["rotation_quaternion"].is_null());
    assert!(obj["camera_Follow Offset"].is_null());
    assert!(obj["player_position"].is_null());
    // camera_intrinsics is always populated (depends only on FOV + resolution).
    // PRD wire name is `camera_intrinsics` (Rust field name `intrinsics` is
    // renamed at serde time so Stream BC lint criterion 12 can read it).
    assert!(obj["camera_intrinsics"].is_object());
    // route_type renders as the bare integer.
    assert_eq!(obj["route_type"].as_u64(), Some(1));
    assert_eq!(obj["metric_scale"].as_f64(), Some(1.0));
    let codes: Vec<u64> = obj["keyCode"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect();
    assert_eq!(codes, vec![16, 87]);
}

#[tokio::test]
async fn camera_position_resolves_from_sibling_session_dir() {
    // Reproduces the Stream BH-narrow bug: the MC Fabric mod and the Rust
    // recorder predict different session_ids (the mod at JVM-launch, the
    // recorder at user-click-Start), so `game_state.jsonl` lands in a
    // sibling session_dir rather than `session_dir/`. Without the
    // sibling-dir fallback, every `camera_position` serializes as `null`
    // even though the mc-mod IS writing per-tick ticks to disk one folder
    // over.
    //
    // This test sets up: recordings_root/{mod_session, recorder_session},
    // with `game_state.jsonl` only in `mod_session`, and `inputs.jsonl` +
    // `frames.jsonl` only in `recorder_session`. After
    // `write_action_camera_json(recorder_session, ...)`, the resulting
    // record's `camera_position` must be a 3-vector of real MC coords,
    // NOT null.
    let root = tempfile::tempdir().unwrap();
    let mod_session = root.path().join("session_20260511_230500_aaaaaaaa");
    let recorder_session = root.path().join("session_20260511_230537_bbbbbbbb");
    std::fs::create_dir_all(&mod_session).unwrap();
    std::fs::create_dir_all(&recorder_session).unwrap();

    // Wall-clock anchor: input first event at t=1_000_000.000s (in seconds,
    // same convention as elsewhere in this file). Game_state tick at the
    // SAME wall-clock instant so the join lands at frame 0.
    let anchor_sec: f64 = 1_000_000.000;
    let anchor_ms: i64 = (anchor_sec * 1_000.0) as i64;

    let inputs =
        format!("{{\"timestamp\":{anchor_sec:.3},\"event_type\":\"START\",\"event_args\":[]}}\n");
    let frames = "{\"idx\":0,\"t_ns\":0}\n";
    let game_state = format!(
        "{{\"tick\":0,\"timestamp_ms\":{anchor_ms},\"x\":65.5,\"y\":64.0,\"z\":-102.5,\
         \"yaw_deg\":12.5,\"pitch_deg\":-3.25,\
         \"velocity_x\":0.0,\"velocity_y\":0.0,\"velocity_z\":0.0}}\n"
    );

    // Recorder dir has inputs + frames; mod dir has game_state. This is
    // the on-disk shape of the bug.
    std::fs::write(
        recorder_session.join(constants::filename::recording::INPUTS),
        &inputs,
    )
    .unwrap();
    std::fs::write(
        recorder_session.join(constants::filename::recording::FRAMES_JSONL),
        frames,
    )
    .unwrap();
    std::fs::write(
        mod_session.join(constants::filename::recording::GAME_STATE_JSONL),
        &game_state,
    )
    .unwrap();

    // The recency window between session dirs is small (both just-created)
    // so the sibling-dir fallback must accept the mod_session candidate.
    write_action_camera_json(&recorder_session, 1920, 1080)
        .await
        .expect("write_action_camera_json");

    let out_path = recorder_session.join(constants::filename::recording::ACTION_CAMERA_JSON);
    let raw = std::fs::read_to_string(&out_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let arr = json.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected 1 frame in action_camera.json");

    let cam = &arr[0]["camera_position"];
    assert!(
        !cam.is_null(),
        "camera_position must be non-null after sibling-dir fallback, got: {cam:?}"
    );
    let cam_obj = cam
        .as_object()
        .or_else(|| None)
        .or_else(|| arr[0]["camera_position"].as_object());
    if let Some(obj) = cam_obj {
        // Serialized as {x, y, z} object form.
        let x = obj["x"].as_f64().expect("camera_position.x");
        let y = obj["y"].as_f64().expect("camera_position.y");
        let z = obj["z"].as_f64().expect("camera_position.z");
        assert!((x - 65.5).abs() < 1e-9, "x got {x}");
        assert!((y - 64.0).abs() < 1e-9, "y got {y}");
        // MC z=-102.5 → customer z=102.5 (the writer flips MC z to
        // customer left-handed frame; see `mc_pos_to_customer`).
        assert!((z - 102.5).abs() < 1e-9, "z got {z}");
    } else if let Some(arr3) = cam.as_array() {
        // Array form [x, y, z]. Either is acceptable per schema doc.
        let x = arr3[0].as_f64().unwrap();
        let y = arr3[1].as_f64().unwrap();
        let z = arr3[2].as_f64().unwrap();
        assert!((x - 65.5).abs() < 1e-9);
        assert!((y - 64.0).abs() < 1e-9);
        assert!((z - 102.5).abs() < 1e-9);
    } else {
        panic!("camera_position must be object {{x,y,z}} or array [x,y,z], got: {cam:?}");
    }
}
