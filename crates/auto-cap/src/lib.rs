//! 5-min auto-cap timer policy.
//!
//! Pure logic kernel for the buyer-spec acceptance bar (every clip
//! `5 ≤ duration ≤ 6 min`, see `docs/RECORDER_BUYER_SPEC_FEATURES.md` §3).
//! This crate intentionally has **zero** OS / IO / framework dependencies
//! so it can be unit-tested on every host the team develops on (macOS,
//! Linux, Windows), while the Windows-only `gamedata-recorder` main crate
//! drives the actual stop transition.
//!
//! # Wiring (see `src/tokio_thread.rs` in the main crate)
//!
//! - **On `Idle/Paused -> Recording`**: caller writes
//!   `app_state.recording_start_time = Some(Instant::now())`.
//! - **On `Recording -> Recording` (restart)**: caller overwrites with a
//!   fresh `Some(Instant::now())`. The timer resets.
//! - **On `Recording -> Idle/Paused`**: caller writes
//!   `app_state.recording_start_time = None`. This is the path the F9
//!   hotkey takes; once cleared the timer can no longer fire, so a
//!   manual stop before the cap silently cancels it.
//! - **Each `perform_checks` tick (1 Hz)**: caller passes the current
//!   start timestamp, the now-instant, and the user-configured
//!   `enable_auto_cap_5min` / `auto_cap_duration_sec` preferences into
//!   [`evaluate`]. If it returns [`ShouldStop::Yes`] the caller fires the
//!   same `RecordingState::Idle` transition F9 would.
//!
//! # Default-off invariant (tested)
//!
//! `enable_auto_cap_5min == false` → [`evaluate`] always returns
//! [`ShouldStop::No`], regardless of elapsed time. This is the
//! property the integration tests pin down so a future refactor cannot
//! accidentally arm the timer for users who never enabled it.

use std::time::{Duration, Instant};

/// Default cap duration: 5:30 (330 seconds).
///
/// Picked as the median of the buyer-spec window `5 ≤ duration ≤ 6 min`:
/// 30 s of head-room above the lower bound (so a graceful stop that takes
/// a second to flush the trailing input events still lands ≥ 5 min) and
/// 30 s under the ceiling (so even if the stop path stalls on a slow
/// disk we have margin before crossing the 6 min hard line).
pub const DEFAULT_AUTO_CAP_DURATION_SEC: u32 = 330;

/// The configuration preferences the policy consults.
///
/// These are a 1:1 mirror of the `Preferences` fields added in
/// `src/config.rs`. We re-declare them here as a plain `Copy` struct so
/// this crate stays decoupled from the (large, Windows-only)
/// `Preferences` type. The main crate is expected to project its
/// preferences into [`Config`] right before calling [`evaluate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// User-facing master switch. **Default `false`.** When `false` the
    /// policy MUST always return [`ShouldStop::No`].
    pub enabled: bool,
    /// Cap duration in seconds. The main crate stores `u32`; we keep
    /// the same width here so projection is a straight copy. Zero is
    /// treated identically to `enabled == false` (operator opt-out).
    pub duration_sec: u32,
}

impl Config {
    /// Build a [`Config`] with the project-wide default cap duration and
    /// the master switch off — i.e. exactly what a fresh
    /// `Preferences::default()` projects to. Useful in tests and for
    /// constructing the disabled sentinel.
    pub const fn disabled_default() -> Self {
        Self {
            enabled: false,
            duration_sec: DEFAULT_AUTO_CAP_DURATION_SEC,
        }
    }

    /// True iff the policy is in its "armed" state — i.e. enabled by the
    /// user AND configured with a non-zero duration. Exposed so the main
    /// crate can short-circuit the tick handler when the policy is
    /// definitely-off, without re-stating the (enabled && duration > 0)
    /// invariant in two places.
    #[inline]
    pub const fn is_armed(&self) -> bool {
        self.enabled && self.duration_sec > 0
    }

    /// Project the [`Config::duration_sec`] field into a [`Duration`].
    /// Returns `None` iff the policy is not armed — callers can treat
    /// `None` as "do not even consult the timer this tick".
    #[inline]
    pub fn duration(&self) -> Option<Duration> {
        if self.is_armed() {
            Some(Duration::from_secs(self.duration_sec as u64))
        } else {
            None
        }
    }
}

/// The decision returned by [`evaluate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShouldStop {
    /// Continue recording — either the cap is disabled, no recording is
    /// active, or the elapsed time has not yet reached the configured
    /// duration.
    No,
    /// Stop the recording. The cap has fired. Caller must drive the
    /// graceful stop path (same one F9 takes) and clear the recording
    /// start timestamp so the next tick can't double-fire.
    Yes,
}

/// Evaluate the auto-cap policy for the current tick.
///
/// # Arguments
///
/// - `cfg`: projection of the user's preferences.
/// - `start`: when the active recording began, or `None` if no
///   recording is live. The main crate sources this from
///   `AppState.recording_start_time`.
/// - `now`: the moment the tick is being evaluated. Passed in (rather
///   than read from `Instant::now()` internally) so unit tests can
///   drive arbitrary elapsed times deterministically without sleeping.
///
/// # Behaviour
///
/// Returns [`ShouldStop::Yes`] iff **all** of the following hold:
///
/// 1. `cfg.enabled == true`
/// 2. `cfg.duration_sec > 0`
/// 3. `start.is_some()` — i.e. there is an active recording.
/// 4. `now.saturating_duration_since(start) >= Duration::from_secs(cfg.duration_sec as u64)`
///
/// In every other case the policy returns [`ShouldStop::No`]. In
/// particular:
///
/// - If the user manually stops the recording with F9 between ticks,
///   the main crate writes `None` into `recording_start_time` and this
///   function returns [`ShouldStop::No`] forever — the cap is cleanly
///   cancelled (no race, no double-fire).
/// - If the user starts a second recording, the new `Some(Instant)` is
///   strictly more recent than the previous one and the elapsed-time
///   check restarts from zero.
/// - If `now < start` (clock skew between ticks, or a test that hands
///   in inverted instants) we treat elapsed as zero and return
///   [`ShouldStop::No`] — `Instant`'s monotonic guarantee makes this
///   the safe default rather than panicking.
pub fn evaluate(cfg: Config, start: Option<Instant>, now: Instant) -> ShouldStop {
    // Default-off guard. Both `enabled == false` and `duration_sec == 0`
    // must short-circuit BEFORE any time arithmetic — this is the
    // property the "pref off → no stop" integration test pins.
    let Some(duration) = cfg.duration() else {
        return ShouldStop::No;
    };

    let Some(start) = start else {
        // No recording active. Nothing to cap.
        return ShouldStop::No;
    };

    // `saturating_duration_since` returns Duration::ZERO if `now < start`,
    // which is safer than `now - start` (which would panic on the same
    // input) and lets us fold the clock-skew case into the common path.
    let elapsed = now.saturating_duration_since(start);
    if elapsed >= duration {
        tracing::info!(
            elapsed_secs = elapsed.as_secs(),
            cap_secs = cfg.duration_sec,
            "Auto-cap timer fired; signalling graceful stop"
        );
        ShouldStop::Yes
    } else {
        ShouldStop::No
    }
}
