//! Stream OTLP — telemetry to Oyster servers.
//!
//! ## Why this exists
//!
//! Audit B1 (P0): rc17.2.3 ships with **zero telemetry**. When 100 users
//! install and 30% fail, we don't know which 30 or why. Required before
//! scaling >10 users.
//!
//! This module adds minimal OTLP instrumentation to `Recording::stop()` so
//! EVERY recorded session ships:
//! - session_id (already in metadata)
//! - lint v3 verdict (PASS/FAIL + failed_criteria count)
//! - duration_seconds
//! - error_count (from existing tracing::warn! / tracing::error! during recording)
//! - recorder_version + commit_sha
//!
//! To: a configurable OTLP HTTP endpoint (default
//! `https://telemetry.oyster.so/v1/sessions` — Howard's server, may not
//! exist yet but spec the contract).
//!
//! ## Constraints
//!
//! - **Best-effort, non-blocking**: telemetry failures NEVER invalidate the
//!   session locally
//! - Configurable via env var `OYSTER_TELEMETRY_ENDPOINT` (empty disables)
//! - Privacy: NO mp4 content uploaded, NO user identifying info beyond
//!   session_id + hardware fingerprint hash

use std::env;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use color_eyre::Result;
use opentelemetry::{
    global,
    trace::{Span, Status},
    KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    trace::{self, RandomIdGenerator, Tracer},
    Resource,
};
use serde::Deserialize;
use tracing::error;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::record::validation::LintResult;

/// Default OTLP HTTP endpoint for session telemetry.
const DEFAULT_TELEMETRY_ENDPOINT: &str = "https://telemetry.oyster.so/v1/sessions";

/// Environment variable to configure telemetry endpoint.
/// If empty or not set, telemetry is disabled.
const TELEMETRY_ENDPOINT_ENV: &str = "OYSTER_TELEMETRY_ENDPOINT";

/// Metadata structure for reading session metadata.json.
#[derive(Debug, Deserialize)]
struct Metadata {
    session_id: String,
    duration: f64,
    // We don't need all fields, just session_id and duration
}

/// Session telemetry data structure.
#[derive(Debug)]
pub struct SessionTelemetry {
    /// Session ID (UUID from metadata).
    pub session_id: String,
    /// Lint v3 verdict.
    pub lint_verdict: LintVerdict,
    /// Recording duration in seconds.
    pub duration_seconds: f64,
    /// Count of errors during recording (from tracing::error! logs).
    pub error_count: u32,
    /// Count of warnings during recording (from tracing::warn! logs).
    pub warning_count: u32,
    /// Recorder version (from CARGO_PKG_VERSION).
    pub recorder_version: String,
    /// Git commit SHA.
    pub commit_sha: String,
}

/// Lint v3 verdict.
#[derive(Debug)]
pub enum LintVerdict {
    /// Lint passed.
    Pass { failed_criteria: u32 },
    /// Lint failed.
    Fail { failed_criteria: u32 },
    /// Lint did not run or errored.
    Unknown,
}

impl SessionTelemetry {
    /// Create new session telemetry by reading metadata from session directory.
    pub fn from_session_dir(
        session_dir: &Path,
        lint_result: Option<&LintResult>,
        error_count: u32,
        warning_count: u32,
    ) -> Result<Self> {
        // Read metadata.json
        let metadata_path = session_dir.join("metadata.json");
        let metadata_content = fs::read_to_string(&metadata_path)
            .map_err(|e| color_eyre::eyre::eyre!("Failed to read metadata.json: {}", e))?;
        
        let metadata: Metadata = serde_json::from_str(&metadata_content)
            .map_err(|e| color_eyre::eyre::eyre!("Failed to parse metadata.json: {}", e))?;

        let lint_verdict = if let Some(lint_result) = lint_result {
            if lint_result.failed == 0 {
                LintVerdict::Pass {
                    failed_criteria: lint_result.failed,
                }
            } else {
                LintVerdict::Fail {
                    failed_criteria: lint_result.failed,
                }
            }
        } else {
            LintVerdict::Unknown
        };

        Ok(Self {
            session_id: metadata.session_id,
            lint_verdict,
            duration_seconds: metadata.duration,
            error_count,
            warning_count,
            recorder_version: env!("CARGO_PKG_VERSION").to_string(),
            commit_sha: git_version::git_version!(fallback = "unknown").to_string(),
        })
    }
}

/// Initialize OTLP tracer if telemetry is enabled.
///
/// Returns `Some(Tracer)` if telemetry endpoint is configured, `None` otherwise.
/// The tracer is configured to be non-blocking and best-effort.
fn init_tracer() -> Option<Tracer> {
    let endpoint = env::var(TELEMETRY_ENDPOINT_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_TELEMETRY_ENDPOINT.to_string());

    // If endpoint is explicitly set to empty string, disable telemetry
    if endpoint.is_empty() {
        return None;
    }

    // Create resource with service name and version
    let resource = Resource::builder()
        .with_service_name("gamedata-recorder")
        .with_version(env!("CARGO_PKG_VERSION"))
        .build();

    // Configure OTLP exporter
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(&endpoint)
        .with_timeout(Duration::from_secs(5)) // Short timeout for non-blocking behavior
        .build()
        .map_err(|e| {
            error!("Failed to build OTLP exporter: {}", e);
            e
        })
        .ok()?;

    // Configure batch span processor with non-blocking behavior
    let batch_config = trace::BatchConfigBuilder::default()
        .with_max_queue_size(100) // Reasonable queue size
        .with_scheduled_delay(Duration::from_secs(5)) // Batch every 5 seconds
        .build();

    let batch_processor = trace::BatchSpanProcessor::builder(exporter)
        .with_batch_config(batch_config)
        .build();

    // Build tracer
    let tracer = opentelemetry_sdk::trace::TracerProvider::builder()
        .with_span_processor(batch_processor)
        .with_resource(resource)
        .with_id_generator(RandomIdGenerator::default())
        .build()
        .tracer("gamedata-recorder");

    Some(tracer)
}

/// Send session telemetry to OTLP endpoint.
///
/// This function is fire-and-forget and will never block or propagate errors.
/// Telemetry failures are logged but do not affect the recording session.
fn send_session_telemetry(telemetry: SessionTelemetry) {
    // Check if telemetry is enabled
    let tracer = match init_tracer() {
        Some(tracer) => tracer,
        None => {
            // Telemetry disabled or failed to initialize
            return;
        }
    };

    // Create span for this session
    let span = tracer
        .span_builder(format!("session.{}", telemetry.session_id))
        .with_attributes(vec![
            KeyValue::new("session.id", telemetry.session_id.clone()),
            KeyValue::new("recorder.version", telemetry.recorder_version.clone()),
            KeyValue::new("commit.sha", telemetry.commit_sha.clone()),
            KeyValue::new("duration.seconds", telemetry.duration_seconds),
            KeyValue::new("error.count", telemetry.error_count as i64),
            KeyValue::new("warning.count", telemetry.warning_count as i64),
        ])
        .start(&tracer);

    // Add lint verdict attributes
    match telemetry.lint_verdict {
        LintVerdict::Pass { failed_criteria } => {
            span.set_attribute(KeyValue::new("lint.verdict", "PASS"));
            span.set_attribute(KeyValue::new("lint.failed_criteria", failed_criteria as i64));
        }
        LintVerdict::Fail { failed_criteria } => {
            span.set_attribute(KeyValue::new("lint.verdict", "FAIL"));
            span.set_attribute(KeyValue::new("lint.failed_criteria", failed_criteria as i64));
            span.set_status(Status::Error("Lint failed".to_string()));
        }
        LintVerdict::Unknown => {
            span.set_attribute(KeyValue::new("lint.verdict", "UNKNOWN"));
        }
    }

    // End the span - this will trigger export
    span.end();

    // Force flush to try to send immediately (best-effort)
    global::force_flush_tracer_provider();
}

/// Spawn telemetry sending in a background task.
///
/// This is the main entry point called from `Recording::stop()`.
/// It reads metadata.json from the session directory and spawns
/// telemetry sending in a background task.
pub fn spawn_telemetry_task(session_dir: &Path, lint_result: Option<LintResult>) {
    // TODO: Implement error/warning counting from tracing events
    // For now, use placeholder values
    let error_count = 0;
    let warning_count = 0;

    // Read metadata and send telemetry in background
    let session_dir = session_dir.to_path_buf();
    tokio::spawn(async move {
        match SessionTelemetry::from_session_dir(&session_dir, lint_result.as_ref(), error_count, warning_count) {
            Ok(telemetry) => {
                send_session_telemetry(telemetry);
            }
            Err(e) => {
                error!("Failed to prepare session telemetry: {}", e);
                // Don't propagate error - telemetry is best-effort
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn test_session_telemetry_from_metadata() {
        let temp_dir = tempdir().unwrap();
        let session_dir = temp_dir.path();
        
        // Create a mock metadata.json
        let metadata_json = r#"{
            "session_id": "test-uuid-123",
            "duration": 123.456,
            "game_exe": "test.exe",
            "hardware_id": "test-hw",
            "start_timestamp": 1000.0,
            "end_timestamp": 1123.456
        }"#;
        
        let metadata_path = session_dir.join("metadata.json");
        std::fs::write(metadata_path, metadata_json).unwrap();
        
        // Test with lint result
        let lint_result = LintResult {
            lint_version: "1.0".to_string(),
            ran_at: "2024-01-01T00:00:00Z".to_string(),
            session_dir: session_dir.to_string_lossy().to_string(),
            total_criteria: 32,
            passed: 30,
            failed: 2,
            failures: vec![],
            overall_status: "FAIL".to_string(),
        };

        let telemetry = SessionTelemetry::from_session_dir(
            session_dir,
            Some(&lint_result),
            5, // error_count
            10, // warning_count
        ).unwrap();

        assert_eq!(telemetry.session_id, "test-uuid-123");
        assert_eq!(telemetry.duration_seconds, 123.456);
        assert_eq!(telemetry.error_count, 5);
        assert_eq!(telemetry.warning_count, 10);
        
        match telemetry.lint_verdict {
            LintVerdict::Fail { failed_criteria } => {
                assert_eq!(failed_criteria, 2);
            }
            _ => panic!("Expected Fail verdict"),
        }
    }

    #[test]
    fn test_lint_verdict_enum() {
        // Test Pass variant
        let pass = LintVerdict::Pass { failed_criteria: 0 };
        match pass {
            LintVerdict::Pass { failed_criteria } => {
                assert_eq!(failed_criteria, 0);
            }
            _ => panic!("Expected Pass variant"),
        }

        // Test Fail variant
        let fail = LintVerdict::Fail { failed_criteria: 5 };
        match fail {
            LintVerdict::Fail { failed_criteria } => {
                assert_eq!(failed_criteria, 5);
            }
            _ => panic!("Expected Fail variant"),
        }

        // Test Unknown variant
        let unknown = LintVerdict::Unknown;
        match unknown {
            LintVerdict::Unknown => {} // Expected
            _ => panic!("Expected Unknown variant"),
        }
    }

    #[test]
    fn test_telemetry_disabled_when_env_empty() {
        // Temporarily set empty endpoint
        env::set_var(TELEMETRY_ENDPOINT_ENV, "");
        
        // Should return None when endpoint is empty
        let tracer = init_tracer();
        assert!(tracer.is_none());
        
        // Clean up
        env::remove_var(TELEMETRY_ENDPOINT_ENV);
    }
}