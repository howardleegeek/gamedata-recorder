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
