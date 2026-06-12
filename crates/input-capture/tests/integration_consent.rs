//! Integration tests for `input_capture::ConsentGuard` and `ConsentStatus`.
//!
//! R46 contract: the recorder must not install any global input hook
//! (RegisterRawInputDevices, gamepad polling) before user consent is
//! recorded for the *current* build version. `ConsentGuard` enforces
//! this gate.

#![cfg(target_os = "windows")]

use input_capture::{ConsentGuard, ConsentStatus};

// ---------------------------------------------------------------------------
// ConsentGuard constructors
// ---------------------------------------------------------------------------

#[test]
fn consent_guard_granted_reports_granted_status() {
    let g = ConsentGuard::granted();
    assert_eq!(g.status(), ConsentStatus::Granted);
    assert!(g.is_granted());
}

#[test]
fn consent_guard_not_granted_reports_not_granted_status() {
    let g = ConsentGuard::not_granted();
    assert_eq!(g.status(), ConsentStatus::NotGranted);
    assert!(!g.is_granted());
}

#[test]
fn consent_guard_new_with_version_mismatch() {
    let g = ConsentGuard::new(ConsentStatus::VersionMismatch);
    assert_eq!(g.status(), ConsentStatus::VersionMismatch);
    assert!(!g.is_granted(), "version mismatch must not be 'granted'");
}

// ---------------------------------------------------------------------------
// require_granted() — the recording-entry gate
// ---------------------------------------------------------------------------

#[test]
fn require_granted_passes_for_granted_status() {
    let g = ConsentGuard::granted();
    assert!(g.require_granted().is_ok(), "Granted must allow recording");
}

#[test]
fn require_granted_blocks_for_not_granted_status() {
    let g = ConsentGuard::not_granted();
    let res = g.require_granted();
    assert!(res.is_err(), "NotGranted must block recording");
    let err = res.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("consent") || msg.contains("disclosure"),
        "error message must reference consent: got `{msg}`"
    );
}

#[test]
fn require_granted_blocks_for_version_mismatch() {
    let g = ConsentGuard::new(ConsentStatus::VersionMismatch);
    let res = g.require_granted();
    assert!(res.is_err(), "VersionMismatch must block recording");
    let err = res.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("prior version") || msg.contains("re-accept") || msg.contains("updated"),
        "version mismatch error must reference re-accept: got `{msg}`"
    );
}

// ---------------------------------------------------------------------------
// ConsentGuard is Clone
// ---------------------------------------------------------------------------

#[test]
fn consent_guard_is_clone_and_carries_status_through() {
    // The host passes a single guard down to multiple subsystems
    // (input-capture, OBS recorder) — clone must preserve the status
    // exactly so each subsystem evaluates the same gate.
    let g1 = ConsentGuard::granted();
    let g2 = g1.clone();
    assert_eq!(g2.status(), ConsentStatus::Granted);

    let g3 = ConsentGuard::not_granted();
    let g4 = g3.clone();
    assert_eq!(g4.status(), ConsentStatus::NotGranted);

    let g5 = ConsentGuard::new(ConsentStatus::VersionMismatch);
    let g6 = g5.clone();
    assert_eq!(g6.status(), ConsentStatus::VersionMismatch);
}

// ---------------------------------------------------------------------------
// ConsentStatus equality
// ---------------------------------------------------------------------------

#[test]
fn consent_status_is_partial_eq_and_eq() {
    // ConsentStatus is used as a HashMap key in the host crate — Eq + Hash
    // must hold. Smoke test PartialEq here (Hash is verified by the host
    // crate's own test suite).
    assert_eq!(ConsentStatus::Granted, ConsentStatus::Granted);
    assert_eq!(ConsentStatus::NotGranted, ConsentStatus::NotGranted);
    assert_eq!(
        ConsentStatus::VersionMismatch,
        ConsentStatus::VersionMismatch
    );
    assert_ne!(ConsentStatus::Granted, ConsentStatus::NotGranted);
    assert_ne!(ConsentStatus::Granted, ConsentStatus::VersionMismatch);
    assert_ne!(ConsentStatus::NotGranted, ConsentStatus::VersionMismatch);
}

#[test]
fn consent_status_is_copy() {
    // The host can pass ConsentStatus by value freely.
    let s = ConsentStatus::Granted;
    let copied = s;
    assert_eq!(s, copied);
}

#[test]
fn consent_status_is_debug() {
    // For log triage we need a working Debug impl. Smoke test that
    // it does not panic for any variant.
    let _ = format!("{:?}", ConsentStatus::Granted);
    let _ = format!("{:?}", ConsentStatus::NotGranted);
    let _ = format!("{:?}", ConsentStatus::VersionMismatch);
}
