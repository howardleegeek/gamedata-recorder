//! Integration tests for the 5-min auto-cap policy.
//!
//! These tests cover the four invariants the dispatch spec calls out
//! (pref off → never fires, pref on → fires at 5:30 ±30 s, manual F9 →
//! cap cleanly cancelled, second recording → timer resets). Time is
//! injected as `Instant` arithmetic — we never sleep, so the suite
//! runs in single-digit milliseconds and is host-independent (the
//! buyer's hard requirement is `cargo test --target aarch64-apple-darwin`
//! passes; the main crate is Windows-only so this crate is where the
//! cross-platform cargo-test green has to come from).

use std::time::{Duration, Instant};

use auto_cap::{Config, DEFAULT_AUTO_CAP_DURATION_SEC, ShouldStop, evaluate};

/// `t0` for every test below — a single anchor `Instant` so the
/// arithmetic in each scenario is unambiguous.
fn epoch() -> Instant {
    Instant::now()
}

// ───────────────────────────────────────────────────────────────────
// Pref OFF → never fires
// ───────────────────────────────────────────────────────────────────

/// **Invariant**: when the user has NOT enabled the cap, the policy must
/// return `No` even after a recording has run for the full 5 minutes
/// (and well beyond). This is the regression guard for the default-off
/// rollout — a future refactor that drops the `enabled` short-circuit
/// silently flips the behaviour for every existing user.
#[test]
fn pref_off_does_not_stop_after_five_minutes() {
    let cfg = Config {
        enabled: false,
        duration_sec: DEFAULT_AUTO_CAP_DURATION_SEC,
    };
    let t0 = epoch();
    let after_5min = t0 + Duration::from_secs(5 * 60);

    assert_eq!(
        evaluate(cfg, Some(t0), after_5min),
        ShouldStop::No,
        "pref off: cap must not fire at the buyer-spec lower bound"
    );

    // And keep verifying far past the cap — at 30 min the recording
    // should still be running by user intent.
    let after_30min = t0 + Duration::from_secs(30 * 60);
    assert_eq!(
        evaluate(cfg, Some(t0), after_30min),
        ShouldStop::No,
        "pref off: cap must still not fire at 30 minutes"
    );
}

/// **Invariant**: even with `enabled == true`, a zero `duration_sec`
/// must be treated as "off" (operator opt-out). Mirrors the
/// `disable_auto_cap_timer` knob in `docs/RECORDER_BUYER_SPEC_FEATURES.md`.
#[test]
fn zero_duration_is_treated_as_disabled() {
    let cfg = Config {
        enabled: true,
        duration_sec: 0,
    };
    let t0 = epoch();
    assert_eq!(
        evaluate(cfg, Some(t0), t0 + Duration::from_secs(10_000)),
        ShouldStop::No,
        "duration_sec == 0 is the explicit opt-out; must not fire"
    );
}

// ───────────────────────────────────────────────────────────────────
// Pref ON → fires at 5:30 ±30 s (buyer-spec window 5..=6 min)
// ───────────────────────────────────────────────────────────────────

/// **Invariant**: with the cap enabled at the default 330 s (5:30) the
/// policy must NOT fire one second early — operator dry-runs would
/// otherwise see clips under the buyer's 5-min lower bound.
#[test]
fn pref_on_does_not_stop_one_second_before_cap() {
    let cfg = Config {
        enabled: true,
        duration_sec: DEFAULT_AUTO_CAP_DURATION_SEC,
    };
    let t0 = epoch();
    let just_before = t0 + Duration::from_secs(DEFAULT_AUTO_CAP_DURATION_SEC as u64 - 1);
    assert_eq!(
        evaluate(cfg, Some(t0), just_before),
        ShouldStop::No,
        "cap must not fire 1 s before configured duration"
    );
}

/// **Invariant**: exactly at the configured 5:30 mark the policy fires.
/// This is the "fires at 5:30 ±30 s" leg from the dispatch spec — at
/// the boundary instant the predicate must trigger so subsequent ticks
/// don't push the clip past the 6 min ceiling.
#[test]
fn pref_on_stops_exactly_at_cap() {
    let cfg = Config {
        enabled: true,
        duration_sec: DEFAULT_AUTO_CAP_DURATION_SEC,
    };
    let t0 = epoch();
    let at_cap = t0 + Duration::from_secs(DEFAULT_AUTO_CAP_DURATION_SEC as u64);
    assert_eq!(
        evaluate(cfg, Some(t0), at_cap),
        ShouldStop::Yes,
        "cap must fire at the configured duration boundary"
    );
}

/// **Invariant**: anywhere in the buyer's 5..=6 min acceptance window
/// the policy continues to report `Yes` so a missed tick (the
/// `perform_checks` interval can be delayed by up to ~1 s under load)
/// still terminates the clip cleanly inside the window.
#[test]
fn pref_on_fires_throughout_buyer_window() {
    let cfg = Config {
        enabled: true,
        duration_sec: DEFAULT_AUTO_CAP_DURATION_SEC,
    };
    let t0 = epoch();

    // Probe the entire acceptance window in 30 s steps. The lower
    // boundary is `DEFAULT_AUTO_CAP_DURATION_SEC` (5:30); the upper is
    // 6:00 — the buyer's hard ceiling. Every sample inside this range
    // must report `Yes` because once the timer has fired the caller is
    // expected to drive a graceful stop on the very next tick.
    for offset_secs in [
        DEFAULT_AUTO_CAP_DURATION_SEC as u64,
        DEFAULT_AUTO_CAP_DURATION_SEC as u64 + 30,
        6 * 60,
    ] {
        let now = t0 + Duration::from_secs(offset_secs);
        assert_eq!(
            evaluate(cfg, Some(t0), now),
            ShouldStop::Yes,
            "cap must remain fired at t0+{offset_secs}s (≥{} s)",
            DEFAULT_AUTO_CAP_DURATION_SEC,
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// Manual F9 before timer → cap cancelled cleanly
// ───────────────────────────────────────────────────────────────────

/// **Invariant**: if the user presses F9 at 4:00 — i.e. before the
/// 5:30 cap — the main crate clears `recording_start_time` to `None`.
/// From that moment on the policy must return `No` for every subsequent
/// tick, regardless of what time it is, so the cap can never
/// retroactively trigger a second stop after the user-initiated one.
///
/// This is the regression check against a class of bug where the cap
/// fires after manual stop, races the next recording's start, and
/// either stops the wrong clip or generates a spurious tray
/// notification.
#[test]
fn manual_f9_before_cap_cleanly_cancels_timer() {
    let cfg = Config {
        enabled: true,
        duration_sec: DEFAULT_AUTO_CAP_DURATION_SEC,
    };
    let recording_started = epoch();

    // 4:00 in — user is still recording, cap has NOT yet fired.
    let four_min = recording_started + Duration::from_secs(4 * 60);
    assert_eq!(
        evaluate(cfg, Some(recording_started), four_min),
        ShouldStop::No,
        "cap must not fire at the 4 min mark"
    );

    // User hits F9. The main crate clears `recording_start_time`.
    let start_after_manual_stop: Option<Instant> = None;

    // Subsequent ticks must keep reporting `No` — not even at the
    // would-have-fired moment, nor anywhere after it. The user already
    // got their stop.
    for offset in [
        Duration::from_secs(4 * 60 + 1),
        Duration::from_secs(DEFAULT_AUTO_CAP_DURATION_SEC as u64),
        Duration::from_secs(DEFAULT_AUTO_CAP_DURATION_SEC as u64 + 60),
        Duration::from_secs(60 * 60),
    ] {
        let tick_at = recording_started + offset;
        assert_eq!(
            evaluate(cfg, start_after_manual_stop, tick_at),
            ShouldStop::No,
            "after manual F9 stop, cap must not fire at +{}s",
            offset.as_secs()
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// Second recording → timer resets
// ───────────────────────────────────────────────────────────────────

/// **Invariant**: after the first recording stops (manually or by cap),
/// a second recording starts with a fresh `Some(Instant)` written to
/// `recording_start_time`. The cap window must restart from that new
/// instant — i.e. the elapsed time of the *first* recording must not
/// leak into the second.
#[test]
fn second_recording_resets_timer() {
    let cfg = Config {
        enabled: true,
        duration_sec: DEFAULT_AUTO_CAP_DURATION_SEC,
    };

    // First recording ran a full 5:30 and was capped.
    let first_started = epoch();
    let first_cap_fired = first_started + Duration::from_secs(DEFAULT_AUTO_CAP_DURATION_SEC as u64);
    assert_eq!(
        evaluate(cfg, Some(first_started), first_cap_fired),
        ShouldStop::Yes,
        "sanity: first recording's cap must fire at the boundary"
    );

    // Five seconds later the operator hits F9 again to start a second
    // clip. The main crate writes a fresh `Some(Instant)` here.
    let second_started = first_cap_fired + Duration::from_secs(5);

    // 1 s into the second recording — the predicate must report `No`
    // even though we are well past `first_started + 5:30` (which would
    // be the wrong reference frame).
    let one_sec_into_second = second_started + Duration::from_secs(1);
    assert_eq!(
        evaluate(cfg, Some(second_started), one_sec_into_second),
        ShouldStop::No,
        "second recording must start its timer from zero, not inherit the first's elapsed"
    );

    // Likewise at 5:29 of the second recording.
    let just_before_second_cap =
        second_started + Duration::from_secs(DEFAULT_AUTO_CAP_DURATION_SEC as u64 - 1);
    assert_eq!(
        evaluate(cfg, Some(second_started), just_before_second_cap),
        ShouldStop::No,
        "second recording must not fire one second before its own cap"
    );

    // And at exactly 5:30 of the second recording it fires — the
    // window is shifted, not double-counted.
    let second_cap_fired =
        second_started + Duration::from_secs(DEFAULT_AUTO_CAP_DURATION_SEC as u64);
    assert_eq!(
        evaluate(cfg, Some(second_started), second_cap_fired),
        ShouldStop::Yes,
        "second recording's own cap must fire at its own boundary"
    );
}

// ───────────────────────────────────────────────────────────────────
// Hardening tests — edge cases pinned so a refactor can't drop them
// ───────────────────────────────────────────────────────────────────

/// `start.is_none()` — there is no live recording — must always be
/// `No`, even if the cap is armed. Otherwise the policy could fire on
/// a stale `enabled` flag right after app startup.
#[test]
fn no_active_recording_never_fires() {
    let cfg = Config {
        enabled: true,
        duration_sec: DEFAULT_AUTO_CAP_DURATION_SEC,
    };
    let t0 = epoch();
    assert_eq!(
        evaluate(cfg, None, t0 + Duration::from_secs(10 * 60)),
        ShouldStop::No,
        "no recording active: cap must never fire"
    );
}

/// Clock skew / inverted instants must not panic. `Instant` is
/// monotonic on every supported host, so this should never happen in
/// production — but the public API takes `Instant` from the caller, so
/// the integration test still pins down the safe fallback.
#[test]
fn inverted_now_and_start_does_not_panic_and_returns_no() {
    let cfg = Config {
        enabled: true,
        duration_sec: 60,
    };
    let later = epoch() + Duration::from_secs(10);
    let earlier = later - Duration::from_secs(5);
    // `now == earlier`, `start == later` ⇒ elapsed clamps to 0.
    assert_eq!(
        evaluate(cfg, Some(later), earlier),
        ShouldStop::No,
        "now < start: elapsed clamps to zero, policy returns No"
    );
}

/// `Config::disabled_default()` is the projection of
/// `Preferences::default()` — it must be a no-fire sentinel.
#[test]
fn disabled_default_is_inert() {
    let cfg = Config::disabled_default();
    assert!(!cfg.is_armed());
    assert_eq!(cfg.duration(), None);
    let t0 = epoch();
    assert_eq!(
        evaluate(cfg, Some(t0), t0 + Duration::from_secs(3600)),
        ShouldStop::No,
    );
}
