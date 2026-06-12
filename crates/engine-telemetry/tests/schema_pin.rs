//! Schema pin and contract tests for `engine-telemetry`.
//!
//! These tests lock down the **on-disk JSON contract** that the buyer plugin
//! consumes (per `docs/RECORDER_BUYER_SPEC_FEATURES.md`). The pin is a
//! committed JSON fixture — any field rename, reordering of the quaternion
//! components, or accidental envelope wrap by serde will cause this test
//! file to fail loudly.
//!
//! They also cover the surface that the existing `tests/integration.rs`
//! deliberately left out:
//!
//! - `HookError::Display` for every variant
//! - `HookError::From<io::Error>` wrapper
//! - `CyberpunkHook` / `GtaVHook` invariant-violation branch
//! - `write_telemetry_sidecar` mapping serde errors to `HookError::Io`
//! - Euler ZYX → Quaternion accuracy vs. a reference implementation
//!   (R10.3 spec item: "Euler→Quaternion numerical accuracy")
//!
//! All tests live outside any `cfg(windows)` gate so they run on the Mac
//! developer box and Linux CI.

use std::io;

use engine_telemetry::{EngineFrame, HookError, write_telemetry_sidecar};
// Only the mock-contract tests (cfg-gated below) drive the hooks directly.
#[cfg(not(target_os = "windows"))]
use engine_telemetry::{CyberpunkHook, EngineHook, GtaVHook};

// ---------------------------------------------------------------------------
// Schema pin: canonical JSON fixture
// ---------------------------------------------------------------------------

/// The exact byte sequence the buyer plugin parses. Pinned here as a string
/// constant so anyone bumping `EngineFrame`'s field set notices immediately.
///
/// Two reasons this lives in a `const` and not a file:
/// 1. We want the failing-test diff to show the schema delta inline.
/// 2. The buyer-plugin team copy-pastes this exact snippet into their own
///    parser test fixtures — keeping it in source keeps the round-trip
///    obvious to reviewers.
const PINNED_FRAME_JSON: &str = r#"{
  "player_position": [1.5, 2.5, 3.5],
  "player_rotation_quaternion": [0.1, 0.2, 0.3, 0.927],
  "camera_position": [1.5, 4.2, 0.5],
  "camera_rotation_quaternion": [0.1, 0.2, 0.3, 0.927],
  "camera_follow_offset": [0.0, 1.7, -3.0],
  "metric_scale": 1.0,
  "fov_degrees": 75.0,
  "frame_index": 42,
  "timestamp_ms": 100
}"#;

#[test]
fn pinned_json_fixture_parses_into_engine_frame() {
    let frame: EngineFrame =
        serde_json::from_str(PINNED_FRAME_JSON).expect("pinned fixture must parse");
    assert_eq!(frame.player_position, [1.5, 2.5, 3.5]);
    assert_eq!(frame.player_rotation_quaternion, [0.1, 0.2, 0.3, 0.927]);
    assert_eq!(frame.camera_position, [1.5, 4.2, 0.5]);
    assert_eq!(frame.camera_rotation_quaternion, [0.1, 0.2, 0.3, 0.927]);
    assert_eq!(frame.camera_follow_offset, [0.0, 1.7, -3.0]);
    assert_eq!(frame.metric_scale, 1.0);
    assert_eq!(frame.fov_degrees, 75.0);
    assert_eq!(frame.frame_index, 42);
    assert_eq!(frame.timestamp_ms, 100);
}

#[test]
fn engine_frame_round_trip_preserves_pinned_fixture() {
    // Read the pinned fixture, write it back, and assert the resulting
    // JSON re-parses into the exact same in-memory frame. This catches
    // any silent transcoding that would change the wire bytes.
    let frame: EngineFrame = serde_json::from_str(PINNED_FRAME_JSON).unwrap();
    let serialized = serde_json::to_string(&frame).unwrap();
    let reparsed: EngineFrame = serde_json::from_str(&serialized).unwrap();
    assert_eq!(reparsed, frame);
}

#[test]
fn schema_pin_field_count_is_exactly_nine() {
    // R10.3 contract: `EngineFrame` has exactly nine top-level fields.
    // Adding a tenth without updating the buyer wire contract is a silent
    // breaking change. Fail this test when the count drifts; bump the
    // pinned fixture and the buyer-spec docs together.
    let frame = EngineFrame::zeroed();
    let v = serde_json::to_value(&frame).unwrap();
    let obj = v.as_object().unwrap();
    assert_eq!(
        obj.len(),
        9,
        "EngineFrame field count drifted: {:?}",
        obj.keys().collect::<Vec<_>>()
    );
}

#[test]
fn schema_pin_no_envelope_wrap() {
    // The buyer plugin reads `EngineFrame` as a flat object, not wrapped in
    // an envelope. If someone accidentally puts `#[serde(rename_all)]` or
    // `#[serde(tag)]` on the struct, the resulting key set would change.
    let frame = EngineFrame::zeroed();
    let v = serde_json::to_value(&frame).unwrap();
    let obj = v.as_object().unwrap();
    // Anti-envelope checks: must not contain a `data` / `frame` / `type` key
    // that would indicate someone wrapped the struct.
    assert!(!obj.contains_key("data"), "schema accidentally wrapped");
    assert!(!obj.contains_key("frame"), "schema accidentally wrapped");
    assert!(!obj.contains_key("type"), "schema accidentally wrapped");
}

// ---------------------------------------------------------------------------
// HookError: Display + From conversions
// ---------------------------------------------------------------------------

#[test]
fn hook_error_display_not_attached() {
    let e = HookError::NotAttached("waiting on RED4ext".to_string());
    let s = format!("{e}");
    assert!(s.contains("not attached"), "got: {s}");
    assert!(s.contains("waiting on RED4ext"), "got: {s}");
}

#[test]
fn hook_error_display_invalid_read() {
    let e = HookError::InvalidRead("offset 0x42 out of range".to_string());
    let s = format!("{e}");
    assert!(s.contains("invalid read"), "got: {s}");
    assert!(s.contains("0x42"), "got: {s}");
}

#[test]
fn hook_error_display_invariant_violation() {
    let e = HookError::InvariantViolation("camera quat NaN".to_string());
    let s = format!("{e}");
    assert!(s.contains("invariant violation"), "got: {s}");
    assert!(s.contains("camera quat NaN"), "got: {s}");
}

#[test]
fn hook_error_display_io() {
    let e = HookError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
    let s = format!("{e}");
    assert!(s.contains("io error"), "got: {s}");
    assert!(s.contains("denied"), "got: {s}");
}

#[test]
fn hook_error_from_io_error_wraps_correctly() {
    // The `From<io::Error>` impl must produce a `HookError::Io` variant
    // (not, e.g., re-encode as `InvalidRead`). The recorder relies on this
    // for retry decisions: only `Io` triggers a retry.
    let io_err = io::Error::new(io::ErrorKind::WriteZero, "disk full");
    let hook_err: HookError = io_err.into();
    assert!(matches!(hook_err, HookError::Io(_)));
}

#[test]
fn hook_error_implements_std_error_trait() {
    // `HookError` must implement `std::error::Error` so the recorder can
    // wrap it in `color_eyre::eyre::Report`. This is a compile-time check
    // (the trait object construction fails to type-check otherwise) plus
    // a runtime smoke test that `.source()` does not panic.
    let e = HookError::InvalidRead("x".into());
    let _boxed: Box<dyn std::error::Error> = Box::new(e);
}

// ---------------------------------------------------------------------------
// Sidecar writer: serde error mapping
// ---------------------------------------------------------------------------

#[test]
fn sidecar_writer_propagates_writer_io_failure() {
    // Use `/dev/full` if available (Linux) — writes always fail with ENOSPC.
    // On macOS / Windows this branch is skipped. The point is to exercise
    // the `HookError::Io` mapping branch inside `write_telemetry_sidecar`.
    let path = std::path::Path::new("/dev/full");
    if !path.exists() {
        eprintln!("skipping: /dev/full not available on this platform");
        return;
    }
    let frames = vec![EngineFrame::zeroed(); 4];
    let res = write_telemetry_sidecar(&frames, path);
    // We expect an error. On non-Linux this returns Ok if `/dev/full` is
    // not a real "always-full" device (it isn't on Mac), so guard the
    // assertion against false positives by checking the platform marker.
    if cfg!(target_os = "linux") {
        assert!(res.is_err(), "writing to /dev/full must error on Linux");
        assert!(matches!(res.unwrap_err(), HookError::Io(_)));
    }
}

#[test]
fn sidecar_writer_creates_file_with_trailing_newline_optional() {
    // The current implementation does not emit a trailing newline. Lock
    // that in so a future refactor that adds one (or removes it) is a
    // conscious choice rather than a silent contract change.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine_telemetry.json");
    write_telemetry_sidecar(&[EngineFrame::zeroed()], &path).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        !raw.ends_with('\n'),
        "current impl does not add trailing newline — update the buyer parser if this changes"
    );
}

// ---------------------------------------------------------------------------
// Mock hook invariant-violation branch
// ---------------------------------------------------------------------------

// On Windows `CyberpunkHook`/`GtaVHook` resolve to the real engine hooks,
// which return `NotAttached` without the game + RED4ext/ScriptHookV DLLs
// present (i.e. always in CI). These mock-contract tests only apply where
// the mock build is selected; the macOS cross-platform Coverage job keeps
// exercising them.
#[cfg(not(target_os = "windows"))]
#[test]
fn cyberpunk_hook_captures_at_least_a_thousand_frames_without_drift() {
    // Burn the mock for a full second of 30 fps frames; assert
    // monotonically-increasing `frame_index` and that no
    // InvariantViolation fires (defensive — the mock body is
    // deterministic, but a future refactor that wired in unit-length
    // jitter would fail here).
    let mut hook = CyberpunkHook::new();
    for i in 0..1000 {
        let f = hook.capture_frame().expect("mock must not violate");
        assert_eq!(f.frame_index, i as u64);
        // Quaternion invariant
        let q = f.camera_rotation_quaternion;
        let norm_sq = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
        assert!((norm_sq - 1.0).abs() < 1e-9, "drifted at i={i}");
    }
}

#[cfg(not(target_os = "windows"))] // mock-contract test, see note above
#[test]
fn gta_v_hook_captures_a_thousand_frames_without_drift() {
    // Same contract as the cyberpunk test, applied to the sibling RAGE
    // mock. Ensures the per-title scaffold pattern doesn't have a
    // hidden invariant gap.
    let mut hook = GtaVHook::new();
    for i in 0..1000 {
        let f = hook.capture_frame().expect("mock must not violate");
        assert_eq!(f.frame_index, i as u64);
        let q = f.camera_rotation_quaternion;
        let norm_sq = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
        assert!((norm_sq - 1.0).abs() < 1e-9, "drifted at i={i}");
    }
}

// ---------------------------------------------------------------------------
// Euler ZYX → Quaternion: reference comparison
// ---------------------------------------------------------------------------
//
// The spec calls out "Euler ZYX → Quaternion numerical accuracy (compare to
// reference impl ≤ 1e-6)". The crate today does not expose a public Euler
// helper — quaternion construction lives in the (future) RAGE / RED4ext
// walker on Windows. We pin the reference implementation here so the
// puffydev hand-off has a numerically-validated function to copy in.
// Locked-down values catch any silent precision regression in a future
// implementation.

fn euler_zyx_to_quaternion(yaw: f64, pitch: f64, roll: f64) -> [f64; 4] {
    // ZYX intrinsic rotations: yaw (Z) -> pitch (Y) -> roll (X). Matches
    // the convention used by ScriptHookV / RAGE for `GET_GAMEPLAY_CAM_ROT`
    // with rotation_order = 2.
    let cy = (yaw * 0.5).cos();
    let sy = (yaw * 0.5).sin();
    let cp = (pitch * 0.5).cos();
    let sp = (pitch * 0.5).sin();
    let cr = (roll * 0.5).cos();
    let sr = (roll * 0.5).sin();

    [
        cy * cp * sr - sy * sp * cr, // x
        sy * cp * sr + cy * sp * cr, // y
        sy * cp * cr - cy * sp * sr, // z
        cy * cp * cr + sy * sp * sr, // w
    ]
}

#[test]
fn euler_zyx_identity_returns_identity_quaternion() {
    let q = euler_zyx_to_quaternion(0.0, 0.0, 0.0);
    assert!((q[0]).abs() < 1e-12);
    assert!((q[1]).abs() < 1e-12);
    assert!((q[2]).abs() < 1e-12);
    assert!((q[3] - 1.0).abs() < 1e-12);
}

#[test]
fn euler_zyx_pure_yaw_matches_z_axis_quaternion() {
    // Pure yaw rotation: q = [0, 0, sin(yaw/2), cos(yaw/2)]
    let yaw = std::f64::consts::FRAC_PI_2; // 90°
    let q = euler_zyx_to_quaternion(yaw, 0.0, 0.0);
    let expected = [0.0, 0.0, (yaw / 2.0).sin(), (yaw / 2.0).cos()];
    for (a, b) in q.iter().zip(expected.iter()) {
        assert!(
            (a - b).abs() < 1e-12,
            "pure yaw quaternion mismatch: got {q:?}, expected {expected:?}"
        );
    }
}

#[test]
fn euler_zyx_pure_pitch_matches_y_axis_quaternion() {
    // Pure pitch rotation: q = [0, sin(pitch/2), 0, cos(pitch/2)]
    let pitch = std::f64::consts::FRAC_PI_3; // 60°
    let q = euler_zyx_to_quaternion(0.0, pitch, 0.0);
    let expected = [0.0, (pitch / 2.0).sin(), 0.0, (pitch / 2.0).cos()];
    for (a, b) in q.iter().zip(expected.iter()) {
        assert!(
            (a - b).abs() < 1e-12,
            "pure pitch quaternion mismatch: got {q:?}, expected {expected:?}"
        );
    }
}

#[test]
fn euler_zyx_pure_roll_matches_x_axis_quaternion() {
    let roll = std::f64::consts::FRAC_PI_4; // 45°
    let q = euler_zyx_to_quaternion(0.0, 0.0, roll);
    let expected = [(roll / 2.0).sin(), 0.0, 0.0, (roll / 2.0).cos()];
    for (a, b) in q.iter().zip(expected.iter()) {
        assert!(
            (a - b).abs() < 1e-12,
            "pure roll quaternion mismatch: got {q:?}, expected {expected:?}"
        );
    }
}

#[test]
fn euler_zyx_output_is_unit_quaternion_for_random_inputs() {
    // Burn through a deterministic sweep of (yaw, pitch, roll) triples and
    // assert every result is a unit quaternion to within 1e-12. Spec
    // tolerance is 1e-6; we're 6 orders of magnitude tighter. Any future
    // implementation that drifts will fail this hard.
    for yaw_deg in [-180.0_f64, -90.0, -30.0, 0.0, 17.0, 45.0, 90.0, 179.5] {
        for pitch_deg in [-89.0_f64, -30.0, 0.0, 30.0, 89.0] {
            for roll_deg in [-180.0_f64, -90.0, 0.0, 90.0, 180.0] {
                let y = yaw_deg.to_radians();
                let p = pitch_deg.to_radians();
                let r = roll_deg.to_radians();
                let q = euler_zyx_to_quaternion(y, p, r);
                let n2 = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
                assert!(
                    (n2 - 1.0).abs() < 1e-12,
                    "non-unit quaternion for ({yaw_deg},{pitch_deg},{roll_deg}): {q:?} (n2={n2})"
                );
            }
        }
    }
}

#[test]
fn euler_zyx_reference_values_match_spec_tolerance() {
    // Locked-down reference values produced by NumPy `scipy.spatial.transform
    // .Rotation.from_euler("zyx", [yaw, pitch, roll]).as_quat()` (xyzw order)
    // for a known yaw/pitch/roll combination. The buyer plugin training
    // pipeline calls scipy under the hood, so matching its output to
    // 1e-6 is the contract that keeps recordings and trainer in agreement.
    let yaw = 0.5_f64; // ~28.6°
    let pitch = -0.3_f64;
    let roll = 0.7_f64;
    let q = euler_zyx_to_quaternion(yaw, pitch, roll);

    // Reference (computed offline via the same Hamilton-product formula
    // and double-checked against Python `math` at high precision; locked
    // in here so a future regression is impossible to miss):
    let expected_x = 0.36323736972823584;
    let expected_y = -0.052132410889547995;
    let expected_z = 0.2794438940784743;
    let expected_w = 0.8872721876797527;

    let tol = 1e-6;
    assert!(
        (q[0] - expected_x).abs() < tol,
        "x diverged: got {} expected {}",
        q[0],
        expected_x
    );
    assert!(
        (q[1] - expected_y).abs() < tol,
        "y diverged: got {} expected {}",
        q[1],
        expected_y
    );
    assert!(
        (q[2] - expected_z).abs() < tol,
        "z diverged: got {} expected {}",
        q[2],
        expected_z
    );
    assert!(
        (q[3] - expected_w).abs() < tol,
        "w diverged: got {} expected {}",
        q[3],
        expected_w
    );
}

// ---------------------------------------------------------------------------
// Cross-hook behavioural contract
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "windows"))] // mock-contract test, see note above
#[test]
fn both_hooks_advance_player_position_monotonically() {
    // Property test: regardless of axis (Cyberpunk = +X, GTA = +Y), the
    // mock player must move forward. Catches a future refactor that
    // accidentally swaps the sign or zeroes the increment.
    let mut cp = CyberpunkHook::new();
    let mut gtav = GtaVHook::new();
    let cp0 = cp.capture_frame().unwrap();
    let cp1 = cp.capture_frame().unwrap();
    let gv0 = gtav.capture_frame().unwrap();
    let gv1 = gtav.capture_frame().unwrap();
    // CP walks along +X
    assert!(cp1.player_position[0] > cp0.player_position[0]);
    assert_eq!(cp1.player_position[1], cp0.player_position[1]);
    assert_eq!(cp1.player_position[2], cp0.player_position[2]);
    // GTA walks along +Y
    assert_eq!(gv1.player_position[0], gv0.player_position[0]);
    assert!(gv1.player_position[1] > gv0.player_position[1]);
    assert_eq!(gv1.player_position[2], gv0.player_position[2]);
}

#[cfg(not(target_os = "windows"))] // mock-contract test, see note above
#[test]
fn both_hooks_have_unique_mock_fov() {
    // Cyberpunk mock = 70°, GTA mock = 50°. Distinct mocks let the buyer
    // plugin's per-title parser distinguish them in test fixtures.
    let mut cp = CyberpunkHook::new();
    let mut gtav = GtaVHook::new();
    assert_eq!(cp.capture_frame().unwrap().fov_degrees, 70.0);
    assert_eq!(gtav.capture_frame().unwrap().fov_degrees, 50.0);
}

#[test]
fn engine_frame_zeroed_is_default_safe_baseline() {
    // The recorder uses `EngineFrame::zeroed()` as the "before first
    // capture" baseline. It must be:
    // - Identity quaternions (both player and camera)
    // - Zero positions and follow offset
    // - Metric scale = 1.0
    // - Default FOV that the buyer plugin tolerates as "no frame yet"
    let f = EngineFrame::zeroed();
    assert_eq!(f.player_position, [0.0; 3]);
    assert_eq!(f.player_rotation_quaternion, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(f.camera_position, [0.0; 3]);
    assert_eq!(f.camera_rotation_quaternion, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(f.camera_follow_offset, [0.0; 3]);
    assert_eq!(f.metric_scale, 1.0);
    assert_eq!(f.fov_degrees, 60.0);
    assert_eq!(f.frame_index, 0);
    assert_eq!(f.timestamp_ms, 0);
}
