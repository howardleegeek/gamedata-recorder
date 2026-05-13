//! Integration tests for `depth-hook::capture::CaptureSession`.
//!
//! On non-Windows platforms `CaptureSession` is a no-op shim — it accepts
//! a profile, returns `Ok`, and `take_frames()` always returns empty.
//! That's still a non-trivial code path with `Drop` cleanup and error
//! conversions. These tests exercise the public surface end-to-end.

use std::sync::Arc;

use depth_hook::profiles::cyberpunk2077::Cyberpunk2077;
use depth_hook::{CaptureError, CaptureSession};

#[test]
fn session_starts_for_known_profile_on_non_windows() {
    // On Mac/Linux, `CaptureSession::start` returns Ok and the resulting
    // session is a no-op. Production behaviour: the recorder treats this
    // as "depth capture disabled" without erroring out the whole pipeline.
    let profile: Arc<dyn depth_hook::DepthHookProfile> = Arc::new(Cyberpunk2077);
    let session = CaptureSession::start(profile);
    assert!(
        session.is_ok(),
        "non-Windows start must succeed (no-op session)"
    );
}

#[test]
fn session_take_frames_returns_empty_on_non_windows() {
    // The no-op session's `take_frames()` always returns Vec::new(). The
    // recorder's frame-loop calls this at 30 Hz; an `Err` here would
    // log-spam the user. Lock the empty-Vec contract.
    let profile: Arc<dyn depth_hook::DepthHookProfile> = Arc::new(Cyberpunk2077);
    let mut session = CaptureSession::start(profile).expect("non-Windows start");
    for _ in 0..10 {
        let frames = session.take_frames();
        assert!(frames.is_empty(), "non-Windows take_frames must be empty");
    }
}

#[test]
fn session_profile_name_matches_active_profile() {
    // The profile name is surfaced in logs and telemetry. Lock the
    // round-trip from constructor argument to accessor.
    let profile: Arc<dyn depth_hook::DepthHookProfile> = Arc::new(Cyberpunk2077);
    let session = CaptureSession::start(profile).expect("start");
    assert_eq!(session.profile_name(), "Cyberpunk 2077 (REDengine 4, DX12)");
}

#[test]
fn session_drop_does_not_panic() {
    // Drop logs and (on Windows) uninstalls the hook. On Mac it's just
    // the tracing line. Explicit drop in a test pin both paths.
    let profile: Arc<dyn depth_hook::DepthHookProfile> = Arc::new(Cyberpunk2077);
    {
        let _session = CaptureSession::start(profile).expect("start");
        // Implicit drop at scope end exercises the Drop impl.
    }
    // If we got here without panicking, the Drop path is clean.
}

#[test]
fn session_lifecycle_round_trip() {
    // Combined: start -> drain frames a few times -> drop. Mirrors the
    // recorder's 30 Hz polling loop semantics so any future refactor
    // that breaks the lifecycle ordering fails here first.
    let profile: Arc<dyn depth_hook::DepthHookProfile> = Arc::new(Cyberpunk2077);
    let mut session = CaptureSession::start(profile).expect("start");
    let _ = session.take_frames();
    let _ = session.take_frames();
    assert_eq!(session.profile_name(), "Cyberpunk 2077 (REDengine 4, DX12)");
    drop(session);
}

// ---------------------------------------------------------------------------
// CaptureError: Display + std::error::Error impls
// ---------------------------------------------------------------------------

#[test]
fn capture_error_hook_install_failed_display() {
    let err = CaptureError::HookInstallFailed("vtable offset mismatch".to_string());
    let s = format!("{err}");
    assert!(s.contains("DX12 hook install failed"));
    assert!(s.contains("vtable offset mismatch"));
}

#[test]
fn capture_error_no_depth_buffer_display() {
    let err = CaptureError::NoDepthBufferFound;
    let s = format!("{err}");
    assert!(s.contains("profile heuristic did not match"));
}

#[test]
fn capture_error_implements_std_error() {
    // The recorder wraps CaptureError in color_eyre::eyre::Report,
    // which needs std::error::Error.
    let err = CaptureError::NoDepthBufferFound;
    let _boxed: Box<dyn std::error::Error> = Box::new(err);
    let err2 = CaptureError::HookInstallFailed("x".into());
    let _boxed2: Box<dyn std::error::Error> = Box::new(err2);
}

#[test]
fn capture_error_debug_does_not_panic() {
    // Sanity: Debug impl on both variants is reachable.
    let err = CaptureError::HookInstallFailed("test".into());
    let _ = format!("{err:?}");
    let err = CaptureError::NoDepthBufferFound;
    let _ = format!("{err:?}");
}
