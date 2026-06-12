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
    assert!((arr[0]["mouseX"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert!((arr[0]["mouseY"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert_eq!(arr[0]["mouse_dx"].as_f64().unwrap(), 0.0);
    assert_eq!(arr[0]["mouse_dy"].as_f64().unwrap(), 0.0);
    // Frame 1: cursor moved +12, +7 in pixels; mouse_dx == 12, mouse_dy == 7.
    let expect_x = (1920.0 / 2.0 + 12.0) / 1920.0;
    let expect_y = (1080.0 / 2.0 + 7.0) / 1080.0;
    assert!((arr[1]["mouseX"].as_f64().unwrap() - expect_x).abs() < 1e-9);
    assert!((arr[1]["mouseY"].as_f64().unwrap() - expect_y).abs() < 1e-9);
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
        "mouseX",
        "mouseY",
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
    assert!((arr[1]["mouseX"].as_f64().unwrap() - expect_x).abs() < 1e-9);
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
    let rec = ActionCameraRecord {
        frame_index: 7,
        timestamp: 0.123,
        mouse_x: 0.5,
        mouse_y: 0.5,
        mouse_dx: 1.5,
        mouse_dy: -2.0,
        key_code: vec![16, 87],
        camera_position: None,
        camera_rotation_quaternion: None,
    };
    let v = serde_json::to_value(&rec).unwrap();
    let obj = v.as_object().unwrap();
    assert_eq!(obj["frame_index"].as_u64(), Some(7));
    assert!((obj["timestamp"].as_f64().unwrap() - 0.123).abs() < 1e-12);
    assert_eq!(obj["mouseX"].as_f64(), Some(0.5));
    assert_eq!(obj["mouseY"].as_f64(), Some(0.5));
    assert_eq!(obj["mouse_dx"].as_f64(), Some(1.5));
    assert_eq!(obj["mouse_dy"].as_f64(), Some(-2.0));
    assert!(obj["camera_position"].is_null());
    assert!(obj["camera_rotation_quaternion"].is_null());
    let codes: Vec<u64> = obj["keyCode"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect();
    assert_eq!(codes, vec![16, 87]);
}
