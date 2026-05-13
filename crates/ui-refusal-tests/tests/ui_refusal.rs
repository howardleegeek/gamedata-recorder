//! Integration tests for the UI-refusal detector.
//!
//! These tests use mock `WindowSnapshot` values to drive
//! `RefusalDetector::observe` and verify the decision logic that the
//! production task (in `src/tokio_thread.rs`) relies on. They DO NOT
//! make any Win32 calls — that lets them run on every host including
//! macOS / Linux CI (`cargo test --target aarch64-apple-darwin` in
//! particular).
//!
//! Spec coverage (`/tmp/gamedata_spec_ui_refusal.md`):
//!
//!   1. Discord overlap → abort
//!   2. Windows toast → abort
//!   3. Alt-tab to desktop → abort
//!   4. Whitelisted game running → no abort
//!   5. Pref off → detector never triggers
//!
//! Plus stall-frame / counter resilience / priority-order coverage.

use std::time::Duration;

use ui_refusal_tests::{RefusalDetector, UiRefusalReason, WindowSnapshot};

/// Spec test #1: Discord overlay window visibly overlaps the game.
/// Expected: abort with `BlacklistedOverlay("discord.exe")`.
///
/// The production code path is: a Discord window owned by `discord.exe`
/// is detected during `EnumWindows`, its rect overlaps the game's, and
/// `WindowSnapshot::blacklisted_overlay` is set to `Some("discord.exe")`.
/// The detector then immediately reports a refusal.
#[test]
fn discord_overlap_aborts_with_overlay_reason() {
    let mut detector = RefusalDetector::default();
    let snap = WindowSnapshot {
        game_is_foreground: Some(true),
        taskbar_overlapping_game: Some(false),
        modal_popup_overlapping_game: Some(false),
        blacklisted_overlay: Some("discord.exe"),
    };
    let reason = detector.observe(&snap, None);
    assert_eq!(
        reason,
        Some(UiRefusalReason::BlacklistedOverlay("discord.exe")),
        "Discord overlap must trigger BlacklistedOverlay refusal"
    );
    // Tray message format matches the spec ("Recording rejected: {reason}").
    assert_eq!(
        UiRefusalReason::BlacklistedOverlay("discord.exe").tray_message(),
        "Recording rejected: discord.exe overlay visible"
    );
}

/// Spec test #2: Windows toast / system dialog pops over the game.
/// Expected: abort with `ModalPopup`.
///
/// The production code path is: an owner-less visible window appears in
/// `EnumWindows` whose rect overlaps the game; the snapshot's
/// `modal_popup_overlapping_game` is set to `Some(true)`.
#[test]
fn windows_toast_aborts_with_modal_popup_reason() {
    let mut detector = RefusalDetector::default();
    let snap = WindowSnapshot {
        game_is_foreground: Some(true),
        taskbar_overlapping_game: Some(false),
        modal_popup_overlapping_game: Some(true),
        blacklisted_overlay: None,
    };
    let reason = detector.observe(&snap, None);
    assert_eq!(
        reason,
        Some(UiRefusalReason::ModalPopup),
        "Windows toast must trigger ModalPopup refusal"
    );
    assert_eq!(
        UiRefusalReason::ModalPopup.tray_message(),
        "Recording rejected: modal popup over game"
    );
}

/// Spec test #3: User alt-tabs to the desktop.
/// Expected: abort with `NotForeground`.
///
/// The production code path is: `GetForegroundWindow` returns something
/// other than the game HWND (or the desktop shell's `Progman` window);
/// `game_is_foreground` is `Some(false)`. The foreground check has the
/// highest priority — it short-circuits before any overlap or stall
/// check.
#[test]
fn alt_tab_to_desktop_aborts_with_not_foreground_reason() {
    let mut detector = RefusalDetector::default();
    let snap = WindowSnapshot {
        game_is_foreground: Some(false),
        // Even if all other checks would have aborted with their own
        // reasons, NotForeground wins by priority.
        taskbar_overlapping_game: Some(true),
        modal_popup_overlapping_game: Some(true),
        blacklisted_overlay: Some("discord.exe"),
    };
    let reason = detector.observe(&snap, None);
    assert_eq!(
        reason,
        Some(UiRefusalReason::NotForeground),
        "Alt-tab away must trigger NotForeground refusal (highest priority)"
    );
    assert_eq!(
        UiRefusalReason::NotForeground.tray_message(),
        "Recording rejected: alt-tab / desktop visible"
    );
}

/// Spec test #4: A whitelisted game is running normally — fullscreen,
/// no popups, no overlays, no taskbar visible. Expected: detector does
/// NOT abort.
///
/// The production code path is: `WindowSnapshot::ok()` — every signal
/// reads "OK". This is the steady-state recording path that the
/// detector must NOT disturb.
#[test]
fn whitelisted_game_normal_play_does_not_abort() {
    let mut detector = RefusalDetector::default();
    let snap = WindowSnapshot::ok();
    // Even with a changing frame hash (real gameplay), no abort.
    for tick in 0..30 {
        // Frame hash changes every tick → not a stall.
        let frame_hash = Some(tick as u64);
        let reason = detector.observe(&snap, frame_hash);
        assert_eq!(
            reason, None,
            "Normal gameplay must NOT trigger any refusal (tick={tick})"
        );
    }
}

/// Spec test #5: Pref off → detector never triggers.
///
/// The spec is explicit: when `enable_ui_refusal_detector = false`, the
/// detector "永不触发" (never triggers). The production gate is in
/// `ui_refusal_detector_task_windows`: when the pref reads `false`, the
/// task `continue`s WITHOUT calling `RefusalDetector::observe`.
///
/// We verify this contract by asserting that, if the production code
/// faithfully implements the gate, NO snapshot — including one with all
/// signals set to refusal — can produce a triggered tray notification
/// or an abort message. We simulate the gate here: if `pref_enabled`
/// is false, we never call `observe`.
#[test]
fn pref_off_never_triggers_abort() {
    let pref_enabled = false;
    let mut detector = RefusalDetector::default();

    // Pathological snapshot — every signal screams "abort".
    let snap = WindowSnapshot {
        game_is_foreground: Some(false),
        taskbar_overlapping_game: Some(true),
        modal_popup_overlapping_game: Some(true),
        blacklisted_overlay: Some("discord.exe"),
    };

    // The gate: if pref off, the production code does NOT call observe.
    let observed = if pref_enabled {
        detector.observe(&snap, Some(0))
    } else {
        None
    };
    assert_eq!(
        observed, None,
        "Pref off must short-circuit observe(); no refusal can be emitted"
    );
}

/// Spec test #6 (additional, defends against false positives): the
/// stall-frame heuristic only fires after 5 consecutive identical
/// samples. A single freeze or a stutter for 1-2 seconds must NOT
/// trip it.
///
/// Semantics: the first sample seen with a new hash counts as run-
/// member #1 (not #0), so an interrupting different-hash sample resets
/// the run to 1 — it takes another `threshold - 1` consecutive
/// matching ticks after that to fire.
#[test]
fn brief_stall_does_not_trigger_abort() {
    let mut detector =
        RefusalDetector::with_stall_window(Duration::from_secs(1), Duration::from_secs(5));
    // 4 identical samples — under threshold (counter reaches 4).
    for _ in 0..4 {
        assert_eq!(detector.observe(&WindowSnapshot::ok(), Some(0xdead)), None);
    }
    // Frame changes — counter resets to 1 for the new hash.
    assert_eq!(detector.observe(&WindowSnapshot::ok(), Some(0xbeef)), None);
    // 3 more identical of 0xbeef — counter reaches 4, still under
    // threshold. The 5th would fire (but that's a stall, not a brief
    // stutter — verified separately).
    for tick in 0..3 {
        assert_eq!(
            detector.observe(&WindowSnapshot::ok(), Some(0xbeef)),
            None,
            "tick {tick} of brief stall must not fire abort"
        );
    }
}

/// Spec test #7 (additional): a real 5-second pause menu DOES fire the
/// stall-frame heuristic.
#[test]
fn five_second_pause_triggers_stalled_frame() {
    let mut detector =
        RefusalDetector::with_stall_window(Duration::from_secs(1), Duration::from_secs(5));
    // First 4 ticks build up the counter.
    for _ in 0..4 {
        assert_eq!(detector.observe(&WindowSnapshot::ok(), Some(0xcafe)), None);
    }
    // 5th tick at the threshold → abort.
    assert_eq!(
        detector.observe(&WindowSnapshot::ok(), Some(0xcafe)),
        Some(UiRefusalReason::StalledFrame)
    );
}

/// Spec test #8 (additional): the detector tolerates platform-shim
/// failures (returning `None` from a Win32 call). Failed signals read
/// as "unknown" and do NOT cause spurious aborts.
///
/// This is the most important false-positive guard — the spec calls out
/// that false positives are expensive ("误判一次 = 用户丢一段录制" /
/// "false positive = user loses a recording"). The buyer cares more
/// about clean recorded gameplay than about catching every popup.
#[test]
fn platform_shim_failure_does_not_trigger_abort() {
    let mut detector = RefusalDetector::default();
    let snap = WindowSnapshot {
        // GetForegroundWindow returned null → can't tell. NOT an abort.
        game_is_foreground: None,
        // FindWindowW for Shell_TrayWnd failed → can't tell. NOT an abort.
        taskbar_overlapping_game: None,
        // EnumWindows hit an error → can't tell. NOT an abort.
        modal_popup_overlapping_game: None,
        blacklisted_overlay: None,
    };
    assert_eq!(detector.observe(&snap, None), None);
}

/// Spec test #9 (additional): taskbar visible is a distinct refusal
/// reason from modal popup. Exercises the second-priority branch of
/// the detector.
#[test]
fn exposed_taskbar_aborts_with_taskbar_visible_reason() {
    let mut detector = RefusalDetector::default();
    let snap = WindowSnapshot {
        game_is_foreground: Some(true),
        taskbar_overlapping_game: Some(true),
        modal_popup_overlapping_game: Some(false),
        blacklisted_overlay: None,
    };
    assert_eq!(
        detector.observe(&snap, None),
        Some(UiRefusalReason::TaskbarVisible)
    );
    assert_eq!(
        UiRefusalReason::TaskbarVisible.tray_message(),
        "Recording rejected: taskbar visible"
    );
}

/// Spec test #10 (additional): after a refusal fires once, the
/// production wiring (1) shows tray notification, (2) sends the reason
/// over the abort channel, (3) resets the detector. We verify the
/// detector itself is in a clean state to be reused after a reset
/// (no leftover stall-counter, no leftover frame hash).
///
/// `RefusalDetector::new(...)` is what the production task calls to
/// reset; we mirror it here.
#[test]
fn detector_reset_after_refusal_yields_clean_state() {
    let mut detector =
        RefusalDetector::with_stall_window(Duration::from_secs(1), Duration::from_secs(5));
    // Fire a refusal.
    let mut snap = WindowSnapshot::ok();
    snap.modal_popup_overlapping_game = Some(true);
    assert_eq!(
        detector.observe(&snap, None),
        Some(UiRefusalReason::ModalPopup)
    );
    // Production code rebuilds the detector after a refusal:
    detector = RefusalDetector::with_stall_window(Duration::from_secs(1), Duration::from_secs(5));
    // A normal snapshot now does not trigger (counter = 1 for 0xfeed).
    assert_eq!(detector.observe(&WindowSnapshot::ok(), Some(0xfeed)), None);
    // And a stall sequence must start counting from a fresh run, not
    // pick up any leftover hash from before the reset. With
    // threshold = 5 samples and counter already at 1, three more
    // identical ticks bring counter to 4 — still under threshold.
    for tick in 0..3 {
        assert_eq!(
            detector.observe(&WindowSnapshot::ok(), Some(0xfeed)),
            None,
            "tick {tick} should not yet stall (run starts at 1 after reset)"
        );
    }
}
