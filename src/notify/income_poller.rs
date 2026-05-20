//! Background income poller — runs as an async tokio task.
//!
//! * Schedules itself for 20:00 local time each day.
//! * Checks network availability before fetching.
//! * Uses exponential backoff (max 3 retries) on transient errors.
//! * Shows native notification via `notify-rust` (or mock in tests).

use std::time::Duration;

use chrono::{Local, Timelike};
use serde::Deserialize;
use tokio::time::{sleep, Instant, MissedTickBehavior};

use crate::app_state::AppState;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Handle returned by [`IncomePoller::spawn`] so the caller can shut it down.
pub struct IncomePoller {
    /// Signal to stop the poller.
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
}

impl IncomePoller {
    /// Spawn the income poller as a background tokio task.
    ///
    /// The task will sleep until the next 20:00 local time, then poll the
    /// backend and show a notification. It repeats every 24 hours.
    ///
    /// Returns a handle that can be used to shut the poller down.
    pub fn spawn(
        app_state: std::sync::Arc<AppState>,
        api_base_url: String,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            run_poller(app_state, api_base_url, shutdown_rx).await;
        });

        Self { shutdown_tx }
    }

    /// Signal the poller to stop. Best-effort — the task may already be
    /// sleeping until the next scheduled poll.
    pub fn stop(self) {
        let _ = self.shutdown_tx.send(());
    }
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

/// Response from `GET /api/v1/income/today`.
#[derive(Debug, Deserialize)]
pub struct IncomeResponse {
    /// Total income earned today in USD.
    pub income_usd: f64,
    /// Number of sessions uploaded today.
    pub sessions_uploaded: u64,
    /// Whether this is the user's first-ever payout (onboarding signal).
    #[serde(default)]
    pub first_payout: bool,
}

/// Trait abstracting the notification backend so we can mock it in tests.
#[async_trait::async_trait]
pub trait Notifier: Send + Sync + 'static {
    /// Show a notification with the given title and body.
    /// The notification should auto-dismiss after ~3 seconds.
    async fn show(&self, title: &str, body: &str);
}

/// Real notifier backed by `notify-rust`.
pub struct NativeNotifier;

#[async_trait::async_trait]
impl Notifier for NativeNotifier {
    async fn show(&self, title: &str, body: &str) {
        show_native_notification(title, body);
    }
}

// ---------------------------------------------------------------------------
// Core poller loop
// ---------------------------------------------------------------------------

async fn run_poller(
    app_state: std::sync::Arc<AppState>,
    api_base_url: String,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    tracing::info!("Income poller started — will poll daily at 20:00 local time");

    loop {
        // Calculate duration until next 20:00 local time.
        let now = Local::now();
        let target = if now.hour() >= 20 {
            // Already past 20:00 today → schedule for tomorrow.
            now.date_naive()
                .succ_opt()
                .unwrap_or(now.date_naive())
                .and_hms_opt(20, 0, 0)
                .unwrap()
                .and_local_timezone(Local)
                .single()
                .unwrap()
        } else {
            // Today's 20:00 is still ahead.
            now.date_naive()
                .and_hms_opt(20, 0, 0)
                .unwrap()
                .and_local_timezone(Local)
                .single()
                .unwrap()
        };

        let wait = target.signed_duration_since(now).to_std().unwrap_or(Duration::ZERO);
        tracing::info!(
            wait_secs = wait.as_secs(),
            "Income poller sleeping until next 20:00"
        );

        // Sleep with shutdown awareness.
        tokio::select! {
            _ = sleep(wait) => {}
            _ = &mut shutdown_rx => {
                tracing::info!("Income poller shutting down (received stop signal)");
                return;
            }
        }

        // Perform the poll.
        match poll_once(&app_state, &api_base_url).await {
            Ok(Some(income)) => {
                let notifier = NativeNotifier;
                show_income_notification(&notifier, &income).await;
            }
            Ok(None) => {
                // No income data yet (e.g. user not logged in) — skip silently.
                tracing::debug!("Income poll: no data to report");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Income poll failed — will retry at next scheduled time");
            }
        }
    }
}

/// Single poll attempt with exponential backoff (max 3 retries).
async fn poll_once(
    app_state: &AppState,
    api_base_url: &str,
) -> Result<Option<IncomeResponse>, String> {
    // Check offline mode first.
    if app_state.offline.mode.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("offline mode active — skipping income poll".to_string());
    }

    // Check network availability via a lightweight probe.
    if !is_network_available().await {
        return Err("network unavailable — skipping income poll".to_string());
    }

    // Get the API key (Bearer token).
    let api_key = {
        let config_guard = app_state.config.read().map_err(|e| format!("config lock poisoned: {e}"))?;
        config_guard.credentials.api_key.clone()
    };

    if api_key.is_empty() {
        return Ok(None); // User not logged in — nothing to poll.
    }

    // Build the request URL.
    let url = format!("{api_base_url}/api/v1/income/today");

    // Exponential backoff: max 3 attempts.
    let mut backoff_ms: u64 = 1_000;
    let max_retries: u32 = 3;

    for attempt in 0..=max_retries {
        tracing::debug!(attempt, "Income poll attempt {}/{}", attempt + 1, max_retries + 1);

        match fetch_income(&url, &api_key).await {
            Ok(income) => return Ok(Some(income)),
            Err(e) => {
                if attempt < max_retries {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        backoff_ms,
                        "Income poll attempt failed — backing off"
                    );
                    sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms = backoff_ms.saturating_mul(2); // exponential
                } else {
                    tracing::error!(
                        error = %e,
                        "Income poll exhausted all retries"
                    );
                    return Err(format!("all retries exhausted: {e}"));
                }
            }
        }
    }

    unreachable!()
}

/// Check if the network is available by probing a well-known endpoint.
async fn is_network_available() -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;

    // Probe a lightweight, globally-available endpoint.
    match client
        .get("https://www.google.com/generate_204")
        .send()
        .await
    {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// Fetch income from the backend.
async fn fetch_income(url: &str, api_key: &str) -> Result<IncomeResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let resp = client
        .get(url)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "backend returned HTTP {}",
            resp.status()
        ));
    }

    let income: IncomeResponse = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse income response: {e}"))?;

    Ok(income)
}

// ---------------------------------------------------------------------------
// Notification display
// ---------------------------------------------------------------------------

async fn show_income_notification<N: Notifier>(notifier: &N, income: &IncomeResponse) {
    if income.first_payout && income.income_usd > 0.0 {
        // Onboarding notification for first payout.
        notifier
            .show(
                "💰 第一笔到账！",
                &format!(
                    "今天 ${:.2}，已上传 {} session — 点击查看 dashboard",
                    income.income_usd, income.sessions_uploaded
                ),
            )
            .await;
    } else {
        // Regular daily income notification.
        notifier
            .show(
                "今日收入",
                &format!(
                    "今天 ${:.2}，已上传 {} session",
                    income.income_usd, income.sessions_uploaded
                ),
            )
            .await;
    }
}

/// Show a native OS notification (Windows toast / macOS NSUserNotification).
/// Auto-dismisses after ~3 seconds.
fn show_native_notification(title: &str, body: &str) {
    #[cfg(not(feature = "mock-notify"))]
    {
        use notify_rust::Notification;

        let result = Notification::new()
            .summary(title)
            .body(body)
            .appname("GameData Recorder")
            .timeout_ms(3000) // 3 seconds
            .show();

        match result {
            Ok(handle) => {
                tracing::debug!(
                    title,
                    body,
                    notification_id = ?handle.id(),
                    "Native notification shown"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to show native notification");
            }
        }
    }

    #[cfg(feature = "mock-notify")]
    {
        // In mock mode, log the notification instead of showing it.
        tracing::info!(
            title,
            body,
            "[MOCK-NOTIFY] Notification would be shown"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock notifier that records calls.
    struct MockNotifier {
        calls: AtomicUsize,
        last_title: std::sync::Mutex<String>,
        last_body: std::sync::Mutex<String>,
    }

    impl MockNotifier {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                last_title: std::sync::Mutex::new(String::new()),
                last_body: std::sync::Mutex::new(String::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn last_title(&self) -> String {
            self.last_title.lock().unwrap().clone()
        }

        fn last_body(&self) -> String {
            self.last_body.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Notifier for MockNotifier {
        async fn show(&self, title: &str, body: &str) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_title.lock().unwrap() = title.to_string();
            *self.last_body.lock().unwrap() = body.to_string();
        }
    }

    #[tokio::test]
    async fn test_regular_income_notification() {
        let notifier = MockNotifier::new();
        let income = IncomeResponse {
            income_usd: 42.50,
            sessions_uploaded: 7,
            first_payout: false,
        };

        show_income_notification(&notifier, &income).await;

        assert_eq!(notifier.call_count(), 1);
        assert_eq!(notifier.last_title(), "今日收入");
        assert!(notifier.last_body().contains("$42.50"));
        assert!(notifier.last_body().contains("7"));
    }

    #[tokio::test]
    async fn test_first_payout_onboard_notification() {
        let notifier = MockNotifier::new();
        let income = IncomeResponse {
            income_usd: 5.00,
            sessions_uploaded: 1,
            first_payout: true,
        };

        show_income_notification(&notifier, &income).await;

        assert_eq!(notifier.call_count(), 1);
        assert!(notifier.last_title().contains("第一笔到账"));
        assert!(notifier.last_body().contains("$5.00"));
        assert!(notifier.last_body().contains("dashboard"));
    }

    #[tokio::test]
    async fn test_zero_income_no_onboard() {
        // Even if first_payout is true, $0 should not trigger onboarding.
        let notifier = MockNotifier::new();
        let income = IncomeResponse {
            income_usd: 0.0,
            sessions_uploaded: 0,
            first_payout: true,
        };

        show_income_notification(&notifier, &income).await;

        assert_eq!(notifier.call_count(), 1);
        // Should be regular notification, not onboarding.
        assert_eq!(notifier.last_title(), "今日收入");
    }

    #[test]
    fn test_income_response_deserialize() {
        let json = r#"{"incomeUsd": 12.34, "sessionsUploaded": 5, "firstPayout": true}"#;
        let income: IncomeResponse = serde_json::from_str(json).unwrap();
        assert!((income.income_usd - 12.34).abs() < 0.001);
        assert_eq!(income.sessions_uploaded, 5);
        assert!(income.first_payout);
    }

    #[test]
    fn test_income_response_deserialize_defaults() {
        let json = r#"{"incomeUsd": 0.0, "sessionsUploaded": 0}"#;
        let income: IncomeResponse = serde_json::from_str(json).unwrap();
        assert!((income.income_usd - 0.0).abs() < 0.001);
        assert_eq!(income.sessions_uploaded, 0);
        assert!(!income.first_payout); // default
    }
}
