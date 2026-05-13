//! Integration tests for `engine-telemetry`.
//!
//! These exercise the public surface (`EngineFrame`, `EngineHook`,
//! `CyberpunkHook`, `GtaVHook`, `write_telemetry_sidecar`) end-to-end
//! on a real tmpdir. They deliberately live outside any `cfg(windows)`
//! gate so they run on the Mac developer box and Linux CI — the whole
//! point of splitting `engine-telemetry` from the eventual Windows-only
//! RTTI / ScriptHookV walker is so the JSON contract and the in-memory
//! plumbing are validated independently of the hook itself.

// `0.7071` (≈ sqrt(2)/2) shows up in this file as a deliberate wire-
// format test fixture — it's the conventional human-readable quaternion
// component for a 90° rotation, and the buyer's sample data files cite
// the same literal. Substituting `FRAC_1_SQRT_2` would obscure the wire
// contract; the value is load-bearing exactly as written. Suppress
// `clippy::approx_constant` at the file level so the spec's
// `-D warnings` clippy gate stays clean without changing test
// assertions. Pattern mirrored from `feat/cyberpunk-hook-cluster`.
#![allow(clippy::approx_constant)]

use engine_telemetry::{
    CyberpunkHook, EngineFrame, EngineHook, GtaVHook, HookError, write_telemetry_sidecar,
};

// ---------------------------------------------------------------------------
// EngineFrame: serde round-trip
// ---------------------------------------------------------------------------

#[test]
fn engine_frame_round_trips_through_serde_json() {
    // Construct a frame with non-trivial values in every field — round-tripping
    // through serde must reproduce them exactly. This is the wire contract
    // test: if this fails, the buyer plugin's parser breaks.
    let original = EngineFrame {
        player_position: [12.5, -3.25, 1024.0],
        player_rotation_quaternion: [0.0, 0.7071, 0.0, 0.7071],
        camera_position: [12.5, -1.55, 1021.0],
        camera_rotation_quaternion: [0.0, 0.7071, 0.0, 0.7071],
        camera_follow_offset: [0.0, 1.7, -3.0],
        metric_scale: 1.0,
        fov_degrees: 90.0,
        frame_index: 12345,
        timestamp_ms: 67890,
    };
    let s = serde_json::to_string(&original).expect("serialize");
    let parsed: EngineFrame = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(parsed, original);
}

#[test]
fn engine_frame_serializes_with_snake_case_field_names() {
    // Buyer wire contract: the plugin pattern-matches on snake_case keys
    // (player_position, camera_follow_offset, metric_scale, frame_index,
    // timestamp_ms). If serde renames these — even by accident, e.g. via a
    // renamed struct field — the plugin silently sees `null` and emits no
    // training samples. Lock the names down by string match.
    let f = EngineFrame::zeroed();
    let v = serde_json::to_value(&f).unwrap();
    let obj = v.as_object().expect("frame must serialize as JSON object");
    for required in [
        "player_position",
        "player_rotation_quaternion",
        "camera_position",
        "camera_rotation_quaternion",
        "camera_follow_offset",
        "metric_scale",
        "fov_degrees",
        "frame_index",
        "timestamp_ms",
    ] {
        assert!(
            obj.contains_key(required),
            "engine_frame missing required field `{required}`: {obj:?}"
        );
    }
}

#[test]
fn engine_frame_quaternion_array_order_is_xyzw() {
    // Order matters: `[x, y, z, w]` with `w` last. Decart's Oasis pipeline
    // (and the existing action_camera writer's `camera_rotation_quaternion`
    // field) both consume w-last. An accidental w-first ordering would feed
    // the trainer rotated-by-90° garbage with no error message.
    let f = EngineFrame {
        player_rotation_quaternion: [1.0, 2.0, 3.0, 4.0],
        ..EngineFrame::zeroed()
    };
    let v = serde_json::to_value(&f).unwrap();
    let arr = v["player_rotation_quaternion"]
        .as_array()
        .expect("quaternion must be JSON array");
    assert_eq!(arr.len(), 4);
    assert_eq!(arr[0].as_f64(), Some(1.0)); // x
    assert_eq!(arr[1].as_f64(), Some(2.0)); // y
    assert_eq!(arr[2].as_f64(), Some(3.0)); // z
    assert_eq!(arr[3].as_f64(), Some(4.0)); // w
}

// ---------------------------------------------------------------------------
// Sidecar writer: round-trip on disk
// ---------------------------------------------------------------------------

#[test]
fn sidecar_writer_round_trips_a_full_recording() {
    // End-to-end: write 5 frames -> read back -> deep-equals the inputs.
    // Any divergence here is a bug in the on-disk JSON contract.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine_telemetry.json");

    let mut hook = CyberpunkHook::new();
    let frames: Vec<EngineFrame> = (0..5)
        .map(|_| hook.capture_frame().expect("mock capture"))
        .collect();

    write_telemetry_sidecar(&frames, &path).expect("write sidecar");

    let raw = std::fs::read_to_string(&path).expect("read sidecar");
    let parsed: Vec<EngineFrame> = serde_json::from_str(&raw).expect("parse sidecar");
    assert_eq!(parsed, frames, "sidecar round-trip mismatch");
}

#[test]
fn sidecar_writer_with_empty_input_writes_empty_array() {
    // Per buyer contract: zero-frame recording must produce literally `[]`,
    // not `null`, not an empty file, not an envelope object. Mirror the
    // same invariant the action_camera writer enforces.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine_telemetry.json");

    write_telemetry_sidecar(&[], &path).expect("write empty");
    let raw = std::fs::read_to_string(&path).expect("read");
    assert_eq!(raw, "[]");
}

#[test]
fn sidecar_writer_top_level_is_json_array() {
    // The format spec is "top-level array of objects" — confirm at the
    // serde_json::Value level so a future refactor that accidentally
    // wraps in an envelope (e.g. `{"frames": [...]}`) fails this test.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine_telemetry.json");

    let frames = vec![EngineFrame::zeroed()];
    write_telemetry_sidecar(&frames, &path).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(v.is_array(), "expected top-level JSON array, got {v:?}");
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert!(arr[0].is_object());
}

#[test]
fn sidecar_writer_overwrites_existing_file() {
    // A second call must replace the file's contents — the recorder may
    // overwrite a partially-written sidecar after a crash recovery.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine_telemetry.json");

    write_telemetry_sidecar(&[EngineFrame::zeroed()], &path).unwrap();
    let first = std::fs::read_to_string(&path).unwrap();
    assert!(first.contains("\"frame_index\""));

    write_telemetry_sidecar(&[], &path).unwrap();
    let second = std::fs::read_to_string(&path).unwrap();
    assert_eq!(second, "[]");
    assert_ne!(first, second);
}

#[test]
fn sidecar_writer_errors_on_unwritable_path() {
    // Writing to a path under a directory that does not exist must surface
    // an error rather than silently producing nothing. The recorder needs
    // this signal to log and skip the sidecar without corrupting the rest
    // of the recording.
    let dir = tempfile::tempdir().unwrap();
    let bogus = dir.path().join("does/not/exist/engine_telemetry.json");
    let res = write_telemetry_sidecar(&[EngineFrame::zeroed()], &bogus);
    assert!(res.is_err(), "expected I/O error for missing parent dir");
    assert!(matches!(res.unwrap_err(), HookError::Io(_)));
}

// ---------------------------------------------------------------------------
// Mock CyberpunkHook produces valid frames
// ---------------------------------------------------------------------------

#[test]
fn mock_hook_produces_unit_quaternions() {
    // Even though the values are deterministic, validate they actually
    // satisfy the unit-quaternion invariant. The real RTTI walker should
    // hit `HookError::InvariantViolation` if it ever sees a degenerate
    // quaternion, so this test serves as a contract check on the mock too.
    let mut hook = CyberpunkHook::new();
    for _ in 0..10 {
        let f = hook.capture_frame().expect("mock capture");
        for q in [&f.player_rotation_quaternion, &f.camera_rotation_quaternion] {
            let n2 = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
            assert!(
                (n2 - 1.0).abs() < 1e-9,
                "quaternion not unit-length: {q:?} (norm^2 = {n2})"
            );
        }
    }
}

#[test]
fn mock_hook_frame_index_matches_iteration_order() {
    // `frame_index` is the buyer plugin's join key against `frames.jsonl`.
    // The mock must produce a strictly increasing sequence starting at 0
    // so integration tests downstream can assert positional alignment.
    let mut hook = CyberpunkHook::new();
    let frames: Vec<EngineFrame> = (0..32).map(|_| hook.capture_frame().unwrap()).collect();
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(f.frame_index, i as u64, "frame_index gap at i={i}");
    }
}

#[test]
fn mock_hook_metric_scale_matches_redengine_convention() {
    // REDengine units ARE meters. The mock and the trait both report `1.0`.
    // Locking this in catches a future refactor that accidentally lets a
    // non-1.0 scale leak through and double-scales positions.
    let mut hook = CyberpunkHook::new();
    let trait_scale = hook.metric_scale();
    let frame_scale = hook.capture_frame().unwrap().metric_scale;
    assert_eq!(trait_scale, 1.0);
    assert_eq!(frame_scale, 1.0);
    assert_eq!(trait_scale, frame_scale);
}

#[test]
fn mock_hook_camera_offset_matches_third_person_convention() {
    // Mock simulates a third-person follow camera at +1.7m up, -3m back.
    // This validates the documented `[right, up, back]` convention is the
    // shape we're producing — a regression here means the contract docs
    // and the impl drifted.
    let mut hook = CyberpunkHook::new();
    let f = hook.capture_frame().unwrap();
    assert_eq!(f.camera_follow_offset[0], 0.0); // right
    assert!(f.camera_follow_offset[1] > 0.0); // up
    assert!(f.camera_follow_offset[2] < 0.0); // back (negative -> behind)
}

#[test]
fn mock_hook_player_walks_along_x_axis() {
    // Sanity: the deterministic mock advances player_position[0] per frame
    // and leaves y/z at zero. Useful for downstream tests in other crates
    // that want a reproducible "moving avatar" fixture.
    let mut hook = CyberpunkHook::new();
    let f0 = hook.capture_frame().unwrap();
    let f1 = hook.capture_frame().unwrap();
    assert!(f1.player_position[0] > f0.player_position[0]);
    assert_eq!(f0.player_position[1], 0.0);
    assert_eq!(f0.player_position[2], 0.0);
    assert_eq!(f1.player_position[1], 0.0);
    assert_eq!(f1.player_position[2], 0.0);
}

#[test]
fn default_cyberpunk_hook_equals_new() {
    // Default impl should be observationally identical to `new()`. Catches
    // a future refactor that adds non-default state to one but not the other.
    let mut a = CyberpunkHook::default();
    let mut b = CyberpunkHook::new();
    let fa = a.capture_frame().unwrap();
    let fb = b.capture_frame().unwrap();
    // timestamp_ms can differ by sub-ms so compare everything else.
    assert_eq!(fa.frame_index, fb.frame_index);
    assert_eq!(fa.player_position, fb.player_position);
    assert_eq!(fa.camera_follow_offset, fb.camera_follow_offset);
    assert_eq!(fa.metric_scale, fb.metric_scale);
}

// ---------------------------------------------------------------------------
// GtaVHook: sibling-of-CyberpunkHook contract checks
// ---------------------------------------------------------------------------

#[test]
fn gta_v_hook_is_constructible() {
    // Smoke test: the per-title scaffold pattern generalises past
    // CyberpunkHook. A new title should require no more than `Hook::new()`
    // + `impl EngineHook` to plug into the rest of the recorder.
    // Runs on every platform — even on Windows the construction path
    // is documented to be `LoadLibrary`-free (lazy attach on first
    // `capture_frame`).
    let _hook = GtaVHook::new();
    let _default_hook = GtaVHook::default();
}

// The mock-specific assertions below (walking-along-+Y, FOV=50°, etc.)
// only hold for the cross-platform `gtav_mock` body. The Windows
// `gtav_windows` build returns `NotAttached` from `capture_frame`
// when no real ScriptHookV.dll is loaded into the test process —
// which is the documented production behaviour, but means the mock
// assertions don't apply. Equivalent Windows coverage lives in
// `src/gtav_windows.rs#tests::happy_path_returns_unit_quaternion_frame`
// against an injected MockRegistry. Mirrors the gating in
// `crates/engine-telemetry/tests/integration.rs` Cyberpunk section
// (`mock_hook_*` cfg-gates), per `feat/cyberpunk-hook-cluster`.

#[cfg(not(target_os = "windows"))]
#[test]
fn gta_v_hook_captures_default_frame() {
    // The mock implementation must produce a fully-populated EngineFrame
    // (no defaulted fields, no NaNs, RAGE-flavoured mock values). This
    // doubles as a regression test that the buyer wire contract is
    // satisfied for GTA V telemetry the same way it is for Cyberpunk.
    let mut hook = GtaVHook::new();
    let f0 = hook.capture_frame().expect("first mock capture");
    assert_eq!(f0.frame_index, 0);
    // RAGE mock walks along +Y (north), distinguishing it from the
    // Cyberpunk mock which walks along +X. See the GtaVHook docstring.
    let f1 = hook.capture_frame().expect("second mock capture");
    assert!(
        f1.player_position[1] > f0.player_position[1],
        "GtaVHook mock should advance along +Y (north): f0={:?}, f1={:?}",
        f0.player_position,
        f1.player_position
    );
    assert_eq!(f0.player_position[0], 0.0);
    assert_eq!(f0.player_position[2], 0.0);
    // Default RAGE gameplay FOV is 50°, distinct from CyberpunkHook's 70°
    // mock. Locks in the documented per-title default.
    assert_eq!(f0.fov_degrees, 50.0);
    // Quaternions must be unit-length even in mock frames — the runtime
    // InvariantViolation guard is exercised by this assertion path.
    for q in [
        &f0.player_rotation_quaternion,
        &f0.camera_rotation_quaternion,
    ] {
        let n2 = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
        assert!(
            (n2 - 1.0).abs() < 1e-9,
            "GtaVHook mock quaternion not unit-length: {q:?}"
        );
    }
}

#[cfg(not(target_os = "windows"))]
#[test]
fn gta_v_hook_respects_metric_scale_one() {
    // RAGE world units are meters (validated empirically — see the
    // 100m walk test in docs/GTA_V_HOOK_RUNBOOK.md). Locking
    // metric_scale = 1.0 catches a future refactor that copies in
    // a UE5-style cm scale by accident.
    let mut hook = GtaVHook::new();
    let trait_scale = hook.metric_scale();
    let frame_scale = hook.capture_frame().unwrap().metric_scale;
    assert_eq!(trait_scale, 1.0);
    assert_eq!(frame_scale, 1.0);
    assert_eq!(GtaVHook::METRIC_SCALE, 1.0);
    assert_eq!(trait_scale, frame_scale);
}

#[cfg(not(target_os = "windows"))]
#[test]
fn gta_v_hook_frame_serde_round_trip() {
    // End-to-end serde round-trip on a captured GtaVHook frame. This is
    // the contract test: if the buyer plugin parses GTA V telemetry
    // differently from Cyberpunk telemetry, the bug shows up here.
    let mut hook = GtaVHook::new();
    let original = hook.capture_frame().expect("mock capture");
    let s = serde_json::to_string(&original).expect("serialize");
    let parsed: EngineFrame = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(parsed, original);
    // Also verify the sidecar writer accepts a vec of GtaVHook frames
    // unchanged — it should, because EngineFrame is engine-agnostic, but
    // a regression in the trait surface would surface here first.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine_telemetry.json");
    let frames: Vec<EngineFrame> = (0..4)
        .map(|_| hook.capture_frame().expect("mock capture"))
        .collect();
    write_telemetry_sidecar(&frames, &path).expect("write sidecar");
    let raw = std::fs::read_to_string(&path).expect("read sidecar");
    let reparsed: Vec<EngineFrame> = serde_json::from_str(&raw).expect("parse sidecar");
    assert_eq!(reparsed, frames);
}

// ---------------------------------------------------------------------------
// Windows-only: GtaVHook with default registry returns NotAttached on a
// system without ScriptHookV.dll loaded.
// ---------------------------------------------------------------------------
//
// This is the integration-level counterpart to the unit tests in
// `src/gtav_windows.rs#tests`. The unit tests use a `MockRegistry` to
// inject simulated native-table responses; this integration test
// exercises the production `ScriptHookVDllRegistry` path on a CI box
// that does *not* have `ScriptHookV.dll` loaded into the test process,
// validating that the failure mode is the documented
// `HookError::NotAttached` (transient, recorder skips frame, no panic).
//
// Built and run only on `target_os = "windows"`. On Mac/Linux this
// configuration is impossible by definition (the Windows-only crate
// surface isn't even compiled). Mirrors the sibling
// `windows_cyberpunk_hook_returns_not_attached_without_red4ext` test
// on `feat/cyberpunk-hook-cluster`.
#[cfg(target_os = "windows")]
#[test]
fn windows_gtav_hook_returns_not_attached_without_scripthookv() {
    use engine_telemetry::GtaVHook;

    // Construct the production hook. `GtaVHook::new()` defers all I/O —
    // no `LoadLibrary` happens here.
    let mut hook = GtaVHook::new();

    // Call capture_frame. The test process is not GTA V Enhanced and
    // does not have ScriptHookV.dll loaded, so we expect NotAttached.
    let res = hook.capture_frame();
    assert!(
        matches!(res, Err(HookError::NotAttached(_))),
        "expected NotAttached on a system without ScriptHookV, got {res:?}"
    );

    // The hook should not panic across consecutive failed calls — the
    // recorder's per-frame tick will call us 60x/second and we MUST
    // remain stable.
    for _ in 0..16 {
        let r = hook.capture_frame();
        assert!(matches!(r, Err(HookError::NotAttached(_))));
    }
}

// ---------------------------------------------------------------------------
// Windows-only: trait-injected registry produces a fully-populated frame.
// ---------------------------------------------------------------------------
//
// Exercises the `with_registry(Box<dyn ScriptHookVRegistry>)` API
// surface — the same seam unit tests use, but at the integration-test
// boundary to lock in the public API contract. Uses an in-line trivial
// implementation rather than re-importing `MockRegistry` (which is
// `pub(super)` and not exposed beyond the `gtav_windows` module).
// Mirrors `windows_cyberpunk_hook_with_injected_registry_emits_engine_frame`
// on `feat/cyberpunk-hook-cluster`.
#[cfg(target_os = "windows")]
#[test]
fn windows_gtav_hook_with_injected_registry_emits_engine_frame() {
    use engine_telemetry::{GtaVHook, RageEulerDeg, RageVector3, ScriptHookVRegistry};

    struct AlwaysOriginRegistry;

    impl ScriptHookVRegistry for AlwaysOriginRegistry {
        fn is_attached(&self) -> bool {
            true
        }
        fn attach_blocker(&self) -> Option<String> {
            None
        }
        fn player_world_position(&self) -> Result<RageVector3, HookError> {
            Ok(RageVector3 {
                x: 7.0,
                y: 8.0,
                z: 9.0,
            })
        }
        fn player_world_rotation(&self) -> Result<RageEulerDeg, HookError> {
            // Identity rotation so the follow_offset derivation
            // simplifies to (camera - player) in [right, up, back]
            // with yaw=0: local_north = dy, so back = -dy.
            Ok(RageEulerDeg {
                pitch: 0.0,
                roll: 0.0,
                yaw: 0.0,
            })
        }
        fn camera_world_position(&self) -> Result<RageVector3, HookError> {
            // 3m behind player (player at y=8, camera at y=5), 1.7m up.
            Ok(RageVector3 {
                x: 7.0,
                y: 5.0,
                z: 10.7,
            })
        }
        fn camera_world_rotation(&self) -> Result<RageEulerDeg, HookError> {
            Ok(RageEulerDeg {
                pitch: 0.0,
                roll: 0.0,
                yaw: 0.0,
            })
        }
        fn camera_fov_degrees(&self) -> Result<f64, HookError> {
            Ok(80.0)
        }
        fn world_id(&self) -> String {
            "test::story::open_world".to_string()
        }
    }

    let mut hook = GtaVHook::with_registry(Box::new(AlwaysOriginRegistry));
    let frame = hook.capture_frame().expect("trait-injected capture");

    // Player position passthrough: RAGE units = meters, X east, Y
    // north, Z up.
    assert_eq!(frame.player_position, [7.0, 8.0, 9.0]);
    // Camera position passthrough.
    assert_eq!(frame.camera_position, [7.0, 5.0, 10.7]);
    // FOV passthrough.
    assert_eq!(frame.fov_degrees, 80.0);
    // METRIC_SCALE is the const 1.0 — never read from the trait.
    assert_eq!(frame.metric_scale, 1.0);
    // Frame index started at 0 and only one frame was captured.
    assert_eq!(frame.frame_index, 0);
    // Identity rotation -> identity quaternion.
    assert_eq!(frame.player_rotation_quaternion, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(frame.camera_rotation_quaternion, [0.0, 0.0, 0.0, 1.0]);
    // Follow-offset derivation with yaw=0:
    //   dx = 0,  dy = -3,  dz = 1.7
    //   local_right = 0, local_north = -3, local_up = 1.7
    //   wire [right, up, back] = [0, 1.7, 3.0]
    let off = frame.camera_follow_offset;
    assert!(off[0].abs() < 1e-9, "right: {off:?}");
    assert!((off[1] - 1.7).abs() < 1e-9, "up: {off:?}");
    assert!((off[2] - 3.0).abs() < 1e-9, "back: {off:?}");

    // Sidecar round-trip on the trait-produced frame, mirroring the
    // mock-body integration test
    // (`sidecar_writer_round_trips_a_full_recording`) so the Windows
    // path has equivalent end-to-end JSON contract coverage.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine_telemetry.json");
    write_telemetry_sidecar(std::slice::from_ref(&frame), &path).expect("write sidecar");
    let raw = std::fs::read_to_string(&path).expect("read sidecar");
    let parsed: Vec<EngineFrame> = serde_json::from_str(&raw).expect("parse");
    assert_eq!(parsed, vec![frame]);
}
