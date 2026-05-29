//! Income notification subsystem.
//!
//! Runs a background tokio task that polls `GET /api/v1/income/today` at
//! 20:00 local time each day. On success, shows a native tray bubble
//! notification via `notify-rust` (Windows toast / macOS NSUserNotification).
//!
//! If income > $0 and this is the user's first payout (onboard signal),
//! a special onboarding notification is shown.

pub mod income_poller;

pub use income_poller::IncomePoller;

/// Compatibility hook for the half-wired S63v2 startup path.
///
/// The income poller needs real backend/auth wiring before startup can own it.
/// Until then, keep the recorder launch path unchanged.
pub fn spawn_income_poller(_local_time: &str) {
    tracing::debug!("S63v2 income poller startup hook disabled");
}
