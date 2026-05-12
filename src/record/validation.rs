//! Stream BN (rc17.2): post-session lint v3 self-validation hook.
//!
//! ## Why this exists
//!
//! Howard's tester reported (2026-05-11): "yesterday's data had many
//! nulls". The recorder's MC mod / recorder process pair has been
//! producing sessions where `action_camera.json.camera_position`,
//! `rotation_*`, `player_*` are all `null`, `inputs.jsonl` contains
//! only markers, `gameinfo.xlsx` is missing, and the depth EXR is
//! absent. Streams BG / BH / BJ are fixing the upstream causes, but
//! meanwhile **every recorded session enters the upload pipeline
//! unvalidated** — bad data ships to the customer.
//!
//! This module closes that gap. The contract is:
//!
//!   * `run_lint_v3(session_dir)` is called from `Recording::stop()`
//!     after metadata.json + action_camera.json + frames.jsonl have
//!     been flushed (i.e. the session is complete on disk).
//!   * It shells out to `lint_v3_prd_grounded.py` (Stream BC's
//!     rewrite — 32 PRD-grounded criteria) and parses the JSON
//!     report.
//!   * It writes a compact `lint_result.json` summary into the
//!     session directory and, on FAIL, fires a Windows toast (via
//!     [`crate::ui::notification::post_session_toast`]) so the
//!     operator notices within ~1s of session stop.
//!
//! ## Failure isolation
//!
//! Lint is **purely diagnostic**. A failed lint run MUST NOT
//! invalidate the session (writing `.invalid` is the recorder's job,
//! and lint v3 is not authoritative for that — Stream BC's authors
//! deliberately separated linting from invalidation). All errors
//! inside this module are logged at `warn` or `error` and swallowed.
//! The worst case is a missing `lint_result.json`, which downstream
//! tooling treats as "lint did not run" (not "lint failed").
//!
//! ## Cross-platform stance
//!
//! The recorder is Windows-only in production, but this module
//! compiles on every host so tests can run on the dev machine
//! (macOS / Linux). The Python invocation, file IO, and JSON
//! reshape are platform-neutral; only the toast call is
//! Windows-flavoured and is delegated to `ui::notification`.
//!
//! ## Out of scope
//!
//! * **Modifying lint v3 criteria** — Stream BC owns the script. We
//!   only read its output.
//! * **CI integration / pre-upload gate** — that's Stream T's
//!   territory. This module only writes `lint_result.json` next to
//!   the session; an external uploader can read it and decide.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Lint v3 has 32 PRD-grounded criteria (Stream BC rewrite, rc17.1).
/// We expose this so callers can sanity-check the lint script version:
/// if the script ever reports `summary.total != EXPECTED_TOTAL_CRITERIA`
/// we still record the result but log a warning, because the result
/// shape may have shifted out from under us.
pub const EXPECTED_TOTAL_CRITERIA: u32 = 32;

/// File name for the per-session lint v3 summary, written by
/// [`run_lint_v3`] into the session directory.
pub const LINT_RESULT_FILENAME: &str = "lint_result.json";

/// Subprocess timeout. Lint v3 does FFmpeg probes per video and
/// quaternion math over the full per-frame array — empirically 5-15s
/// for a 5-min 1080p session. We cap at 60s so a hung ffprobe (e.g.
/// MP4 with corrupt moov) doesn't stall `Recording::stop()` forever.
const LINT_TIMEOUT: Duration = Duration::from_secs(60);

/// JSON shape produced by `lint_v3_prd_grounded.py`'s `LintReport.to_dict()`.
/// Only the fields we actually consume are listed; serde ignores the
/// rest (`#[serde(default)]` covers script-version drift).
#[derive(Debug, Deserialize)]
struct LintScriptReport {
    #[serde(default)]
    summary: LintScriptSummary,
    #[serde(default)]
    results: Vec<LintScriptResult>,
}

#[derive(Debug, Default, Deserialize)]
struct LintScriptSummary {
    #[serde(default)]
    total: u32,
    #[serde(default)]
    passed: u32,
    #[serde(default)]
    failed: u32,
}

#[derive(Debug, Deserialize)]
struct LintScriptResult {
    id: u32,
    name: String,
    passed: bool,
    #[serde(default)]
    message: String,
}

/// Per-criterion failure record persisted in `lint_result.json`.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LintFailure {
    /// Criterion number from `lint_v3_prd_grounded.py` (1..=32).
    pub criterion: u32,
    /// Short, machine-readable criterion name (e.g. `"camera_intrinsics"`).
    pub name: String,
    /// Human-readable reason text from the lint script.
    pub reason: String,
}

/// Top-level shape of `lint_result.json` (spec'd in Stream BN ticket).
///
/// Stable, additive: new fields go at the end, no field is ever
/// renamed or removed without a `lint_version` bump.
#[derive(Debug, Serialize)]
pub struct LintResult {
    /// Wire version of the lint result shape. Bump if you remove
    /// fields. Adding optional fields does not require a bump.
    pub lint_version: String,
    /// RFC 3339 UTC timestamp of when lint v3 finished.
    pub ran_at: String,
    /// Absolute path of the session directory that was linted.
    pub session_dir: String,
    /// Total number of PRD criteria checked by the script. Should be
    /// [`EXPECTED_TOTAL_CRITERIA`]; logged as a warning if not.
    pub total_criteria: u32,
    /// Count of criteria that returned `passed: true`.
    pub passed: u32,
    /// Count of criteria that returned `passed: false`.
    pub failed: u32,
    /// Per-criterion failure records, in script-emitted order. Empty
    /// when `overall_status == "PASS"`.
    pub failures: Vec<LintFailure>,
    /// `"PASS"` iff `failed == 0`, otherwise `"FAIL"`. Sentinel
    /// strings (not booleans) so downstream JSON consumers can
    /// trivially log/grep without parsing the count.
    pub overall_status: String,
    /// Optional error reason when the lint script itself could not
    /// be run (e.g. python not on PATH, script missing, timeout).
    /// When `Some`, `failures` is empty and `overall_status` is
    /// `"ERROR"`. Downstream treats this as "lint did not run" —
    /// distinct from "lint ran and failed".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl LintResult {
    /// Build a PASS / FAIL result from a parsed script report. The
    /// `ran_at` and `session_dir` arguments are injected by the caller
    /// (so unit tests can pin them deterministically).
    fn from_script(
        report: LintScriptReport,
        session_dir: &Path,
        ran_at: String,
    ) -> Self {
        let failures: Vec<LintFailure> = report
            .results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| LintFailure {
                criterion: r.id,
                name: r.name.clone(),
                reason: r.message.clone(),
            })
            .collect();

        let overall_status = if report.summary.failed == 0 && !failures.is_empty() {
            // Defensive: if summary.failed is wrong but results show
            // failures, trust the per-result list. Downstream cares
            // about the failures count.
            "FAIL".to_string()
        } else if report.summary.failed == 0 {
            "PASS".to_string()
        } else {
            "FAIL".to_string()
        };

        Self {
            lint_version: "v3".to_string(),
            ran_at,
            session_dir: session_dir.display().to_string(),
            total_criteria: report.summary.total,
            passed: report.summary.passed,
            failed: report.summary.failed.max(failures.len() as u32),
            failures,
            overall_status,
            error: None,
        }
    }

    /// Build an ERROR result for when the lint script could not be
    /// executed at all (binary missing, timeout, malformed JSON).
    fn from_error(session_dir: &Path, ran_at: String, reason: String) -> Self {
        Self {
            lint_version: "v3".to_string(),
            ran_at,
            session_dir: session_dir.display().to_string(),
            total_criteria: 0,
            passed: 0,
            failed: 0,
            failures: Vec::new(),
            overall_status: "ERROR".to_string(),
            error: Some(reason),
        }
    }
}

/// Resolve the absolute path to `lint_v3_prd_grounded.py`.
///
/// Resolution order:
///   1. `OYSTER_LINT_V3_PY` env var (operator override; absolute path).
///   2. `<exe_dir>/lint_v3_prd_grounded.py` — bundled next to the
///      recorder binary by the installer (rc17.x bundler).
///   3. `<exe_dir>/bin/lint_v3_prd_grounded.py` — dev layout for
///      `cargo run` inside the parent oyster-agent-runner workspace.
///   4. `<cwd>/bin/lint_v3_prd_grounded.py` — fallback for `cargo run`
///      from the parent repo root.
///
/// Returns `None` if no candidate exists, which the caller surfaces as
/// `LintResult::ERROR` (lint did not run).
fn resolve_lint_script() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("OYSTER_LINT_V3_PY") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Some(p);
        }
        tracing::warn!(
            path = %p.display(),
            "OYSTER_LINT_V3_PY set but file not found, falling back to discovery"
        );
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe_dir.as_ref() {
        candidates.push(dir.join("lint_v3_prd_grounded.py"));
        candidates.push(dir.join("bin").join("lint_v3_prd_grounded.py"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("bin").join("lint_v3_prd_grounded.py"));
        // Parent workspace layout: vendor/recorder/target/<profile>/<exe>
        // -> walk up to find a sibling `bin/` directory.
        let mut walk = cwd.as_path();
        for _ in 0..6 {
            let cand = walk.join("bin").join("lint_v3_prd_grounded.py");
            if cand.is_file() {
                return Some(cand);
            }
            walk = match walk.parent() {
                Some(p) => p,
                None => break,
            };
        }
    }

    candidates.into_iter().find(|p| p.is_file())
}

/// Pick the python interpreter to invoke. Prefers `OYSTER_PYTHON` env
/// var, then `python3` (POSIX dev hosts), then `python.exe` /
/// `python` (Windows).
fn resolve_python() -> String {
    if let Ok(p) = std::env::var("OYSTER_PYTHON") {
        if !p.is_empty() {
            return p;
        }
    }
    if cfg!(windows) {
        "python".to_string()
    } else {
        "python3".to_string()
    }
}

/// RFC 3339 UTC timestamp ("2026-05-12T18:42:01Z"). We deliberately
/// avoid the `chrono` crate to keep the dependency surface small — the
/// recorder already has `std::time::SystemTime`, and this is the only
/// place we need a formatted timestamp.
fn now_iso8601_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Convert epoch seconds to (Y, M, D, h, m, s) without chrono. Civil
    // calendar conversion adapted from Howard Hinnant's date algorithms,
    // which the Rust `time` crate also uses internally.
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    let (y, m, d) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hour, minute, second
    )
}

/// Convert "days since 1970-01-01" to (year, month, day).
/// Civil-from-days algorithm (Hinnant 2013), valid for any signed days.
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // 0..=146_096
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y_adjusted = if m <= 2 { y + 1 } else { y };
    (y_adjusted as i32, m as u32, d as u32)
}

/// Write the result to `<session_dir>/lint_result.json` atomically
/// (write to a temp file then rename). On failure to write, the
/// caller still gets the LintResult value back; persistence errors
/// are logged but swallowed (the toast still fires).
fn write_lint_result(session_dir: &Path, result: &LintResult) -> std::io::Result<PathBuf> {
    let target = session_dir.join(LINT_RESULT_FILENAME);
    let tmp = session_dir.join(format!("{}.tmp", LINT_RESULT_FILENAME));
    let body = serde_json::to_vec_pretty(result).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;
    std::fs::write(&tmp, &body)?;
    // Best-effort fsync of the tmp before rename so a power loss between
    // rename and reboot can't leave a torn lint_result.json.
    if let Ok(f) = std::fs::File::open(&tmp) {
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

/// Parse the JSON the lint script wrote to its `--output` file, or
/// (fallback) the stdout it printed. Returns either a `LintResult` or
/// an error reason that becomes `LintResult::ERROR`.
fn parse_lint_output(
    json_bytes: &[u8],
    session_dir: &Path,
    ran_at: String,
) -> Result<LintResult, String> {
    let report: LintScriptReport = serde_json::from_slice(json_bytes)
        .map_err(|e| format!("failed to parse lint v3 JSON: {e}"))?;
    if report.summary.total != EXPECTED_TOTAL_CRITERIA {
        tracing::warn!(
            actual = report.summary.total,
            expected = EXPECTED_TOTAL_CRITERIA,
            "lint v3 reported unexpected criteria count — script version drift?"
        );
    }
    Ok(LintResult::from_script(report, session_dir, ran_at))
}

/// Run lint v3 on the freshly-finalized session, persist the summary
/// to `<session_dir>/lint_result.json`, and on FAIL fire a Windows
/// toast pointing the operator at the session.
///
/// Returns `Ok(LintResult)` whether the lint passed, failed, or
/// errored — the caller treats this as advisory only.
pub async fn run_lint_v3(session_dir: PathBuf) -> std::io::Result<LintResult> {
    let ran_at = now_iso8601_utc();

    // Step 1: resolve script + interpreter. Either missing → ERROR
    // result (lint did not run).
    let script = match resolve_lint_script() {
        Some(p) => p,
        None => {
            let reason =
                "lint_v3_prd_grounded.py not found (set OYSTER_LINT_V3_PY to override)"
                    .to_string();
            tracing::warn!(reason = %reason, "run_lint_v3: cannot locate script");
            let result = LintResult::from_error(&session_dir, ran_at, reason);
            let _ = write_lint_result(&session_dir, &result);
            return Ok(result);
        }
    };
    let python = resolve_python();

    // Step 2: run `python lint_v3_prd_grounded.py <session_dir>
    // --output <tmp>`. We force --output (rather than stdout parsing)
    // so a stray `print(...)` in a future revision can't corrupt our
    // JSON parse. Atomic-write to a tmp inside session_dir.
    let lint_output_tmp = session_dir.join(".lint_v3_output.json.tmp");
    // Make sure any stale tmp from a crashed previous run is gone.
    let _ = std::fs::remove_file(&lint_output_tmp);

    let session_dir_arg = session_dir.clone();
    let lint_output_arg = lint_output_tmp.clone();
    let script_arg = script.clone();
    let python_arg = python.clone();

    let spawn_result = tokio::time::timeout(
        LINT_TIMEOUT,
        tokio::process::Command::new(&python_arg)
            .arg(&script_arg)
            .arg(&session_dir_arg)
            .arg("--output")
            .arg(&lint_output_arg)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await;

    let result: LintResult = match spawn_result {
        Err(_elapsed) => {
            let reason = format!(
                "lint v3 timed out after {}s ({}s budget)",
                LINT_TIMEOUT.as_secs(),
                LINT_TIMEOUT.as_secs()
            );
            tracing::warn!(reason = %reason, "run_lint_v3: timeout");
            LintResult::from_error(&session_dir, ran_at, reason)
        }
        Ok(Err(io_err)) => {
            let reason = format!("failed to spawn '{}': {io_err}", python_arg);
            tracing::warn!(reason = %reason, "run_lint_v3: spawn failed");
            LintResult::from_error(&session_dir, ran_at, reason)
        }
        Ok(Ok(output)) => {
            // Exit code 0 = all pass, 1 = some fail, 2 = lint itself errored.
            // We treat 0 and 1 as "script ran cleanly, read output file";
            // 2 is an internal lint failure (parse output if present, else
            // surface stderr as ERROR).
            let exit_code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if !lint_output_tmp.is_file() && exit_code == 2 {
                let stderr_excerpt = stderr.chars().take(512).collect::<String>();
                let reason = format!(
                    "lint v3 errored (exit=2) and produced no output: {stderr_excerpt}"
                );
                tracing::warn!(reason = %reason, "run_lint_v3: script error");
                LintResult::from_error(&session_dir, ran_at, reason)
            } else if !lint_output_tmp.is_file() {
                let reason = format!(
                    "lint v3 produced no output file (exit={}); stderr: {}",
                    exit_code,
                    stderr.chars().take(256).collect::<String>(),
                );
                tracing::warn!(reason = %reason, "run_lint_v3: missing output");
                LintResult::from_error(&session_dir, ran_at, reason)
            } else {
                let bytes = std::fs::read(&lint_output_tmp).unwrap_or_default();
                match parse_lint_output(&bytes, &session_dir, ran_at.clone()) {
                    Ok(r) => r,
                    Err(reason) => {
                        tracing::warn!(reason = %reason, "run_lint_v3: parse failed");
                        LintResult::from_error(&session_dir, ran_at, reason)
                    }
                }
            }
        }
    };

    // Clean up the tmp output file (best effort).
    let _ = std::fs::remove_file(&lint_output_tmp);

    // Step 3: persist lint_result.json. Errors are logged + swallowed.
    match write_lint_result(&session_dir, &result) {
        Ok(p) => {
            tracing::info!(
                path = %p.display(),
                status = %result.overall_status,
                passed = result.passed,
                failed = result.failed,
                "run_lint_v3: lint_result.json written"
            );
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                session_dir = %session_dir.display(),
                "run_lint_v3: failed to write lint_result.json (recording NOT invalidated)"
            );
        }
    }

    // Step 4: toast on FAIL/ERROR. PASS sessions stay silent — we
    // don't want to flash a toast on every successful recording, only
    // when a human needs to look.
    if result.overall_status != "PASS" {
        let title = format!(
            "Session FAILED lint v3 ({} criteria)",
            result.failed
        );
        let body = if let Some(err) = &result.error {
            format!(
                "Lint did not run: {}\nSession: {}",
                err,
                session_dir.display()
            )
        } else {
            let first_few: Vec<String> = result
                .failures
                .iter()
                .take(3)
                .map(|f| format!("#{} {}", f.criterion, f.name))
                .collect();
            format!(
                "{} failed criteria: {}\nSession: {}",
                result.failed,
                first_few.join(", "),
                session_dir.display()
            )
        };
        crate::ui::notification::post_session_toast(&title, &body, &session_dir);
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests — cross-platform (no Windows API, no real lint subprocess).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// The contract under test: given the JSON shape that
    /// `lint_v3_prd_grounded.py`'s `LintReport.to_dict()` produces,
    /// `parse_lint_output` must materialize the per-failure list and
    /// pick the right `overall_status`.
    fn sample_pass_json() -> Vec<u8> {
        // 32 passing criteria, summary.failed = 0.
        let results: Vec<serde_json::Value> = (1..=32)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "name": format!("criterion_{i}"),
                    "passed": true,
                    "message": "ok",
                    "details": {}
                })
            })
            .collect();
        let body = serde_json::json!({
            "data_dir": "/tmp/session",
            "summary": { "total": 32, "passed": 32, "failed": 0, "pass_rate": "100.0%" },
            "results": results,
        });
        serde_json::to_vec_pretty(&body).unwrap()
    }

    fn sample_fail_with_nulls_json() -> Vec<u8> {
        // Mirrors Howard's tester report: criterion #12 (intrinsics) and
        // #13/14 (quaternion) FAIL because action_camera.json's camera_*
        // and rotation_* fields are null. Plus #22 metadata missing
        // gameProcessName. 3 failed, 29 passed.
        let mut results: Vec<serde_json::Value> = Vec::with_capacity(32);
        for i in 1..=32 {
            let (passed, msg) = match i {
                12 => (false, "fx is null on 305/305 frames".to_string()),
                13 => (
                    false,
                    "rotation_quaternion is null on 305/305 frames".to_string(),
                ),
                22 => (
                    false,
                    "metadata.json missing required field: gameProcessName".to_string(),
                ),
                _ => (true, "ok".to_string()),
            };
            results.push(serde_json::json!({
                "id": i,
                "name": match i {
                    12 => "camera_intrinsics",
                    13 => "quaternion_shape",
                    22 => "metadata_required_fields",
                    _ => "criterion",
                }.to_string(),
                "passed": passed,
                "message": msg,
                "details": {}
            }));
        }
        let body = serde_json::json!({
            "data_dir": "/tmp/session_with_nulls",
            "summary": { "total": 32, "passed": 29, "failed": 3, "pass_rate": "90.6%" },
            "results": results,
        });
        serde_json::to_vec_pretty(&body).unwrap()
    }

    #[test]
    fn parse_lint_output_pass_yields_pass_status() {
        let session_dir = PathBuf::from("/tmp/session");
        let r =
            parse_lint_output(&sample_pass_json(), &session_dir, "2026-05-12T00:00:00Z".into())
                .expect("parse should succeed");
        assert_eq!(r.overall_status, "PASS");
        assert_eq!(r.passed, 32);
        assert_eq!(r.failed, 0);
        assert_eq!(r.total_criteria, 32);
        assert!(r.failures.is_empty());
        assert_eq!(r.lint_version, "v3");
    }

    #[test]
    fn parse_lint_output_fail_with_nulls_lists_failures() {
        // This is the regression test that matches Howard's "many nulls"
        // session. The whole point of Stream BN is that this kind of
        // session must produce a FAIL lint_result.json BEFORE it gets
        // uploaded, instead of silently shipping.
        let session_dir = PathBuf::from("/tmp/session_with_nulls");
        let r = parse_lint_output(
            &sample_fail_with_nulls_json(),
            &session_dir,
            "2026-05-12T00:00:00Z".into(),
        )
        .expect("parse should succeed");

        assert_eq!(r.overall_status, "FAIL");
        assert_eq!(r.failed, 3);
        assert_eq!(r.passed, 29);
        assert_eq!(r.failures.len(), 3);

        // Sorted by criterion id (script-emitted order). Specifically
        // we want to see #12 / #13 / #22 — the exact criteria that
        // catch the NULL-data session.
        let ids: Vec<u32> = r.failures.iter().map(|f| f.criterion).collect();
        assert_eq!(ids, vec![12, 13, 22]);

        let intrinsics = r
            .failures
            .iter()
            .find(|f| f.criterion == 12)
            .expect("intrinsics failure must be present");
        assert_eq!(intrinsics.name, "camera_intrinsics");
        assert!(
            intrinsics.reason.contains("fx is null"),
            "reason must surface the underlying null"
        );
    }

    #[test]
    fn write_lint_result_creates_atomic_file() {
        let tmp = TempDir::new().expect("tempdir");
        let session_dir = tmp.path().to_path_buf();
        let r = LintResult {
            lint_version: "v3".into(),
            ran_at: "2026-05-12T00:00:00Z".into(),
            session_dir: session_dir.display().to_string(),
            total_criteria: 32,
            passed: 29,
            failed: 3,
            failures: vec![LintFailure {
                criterion: 12,
                name: "camera_intrinsics".into(),
                reason: "fx is null".into(),
            }],
            overall_status: "FAIL".into(),
            error: None,
        };
        let written = write_lint_result(&session_dir, &r).expect("write");
        assert_eq!(written.file_name().unwrap(), LINT_RESULT_FILENAME);
        // Tmp must be gone (atomic rename).
        let tmp_path = session_dir.join(format!("{}.tmp", LINT_RESULT_FILENAME));
        assert!(!tmp_path.exists(), "tmp file should be renamed away");

        // Roundtrip: file contents parse back into the expected fields.
        let bytes = fs::read(&written).expect("read back");
        let parsed: serde_json::Value =
            serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(parsed["overall_status"], "FAIL");
        assert_eq!(parsed["failed"], 3);
        assert_eq!(parsed["lint_version"], "v3");
        assert_eq!(parsed["failures"][0]["criterion"], 12);
    }

    #[test]
    fn from_error_yields_error_status_and_empty_failures() {
        let session_dir = PathBuf::from("/tmp/x");
        let r = LintResult::from_error(
            &session_dir,
            "2026-05-12T00:00:00Z".into(),
            "python not found".into(),
        );
        assert_eq!(r.overall_status, "ERROR");
        assert!(r.failures.is_empty());
        assert_eq!(r.error.as_deref(), Some("python not found"));
    }

    /// Civil calendar sanity — make sure the no-chrono date conversion
    /// doesn't drift across known epoch fixed points.
    #[test]
    fn days_to_ymd_known_dates() {
        // 1970-01-01 → day 0
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        // 2000-01-01 → day 10957 (30 years × 365.25)
        assert_eq!(days_to_ymd(10957), (2000, 1, 1));
        // 2026-05-12 → day = (2026-1970)*365 + leap days
        // Compute: 56*365 = 20440, leaps 1972..=2024 every 4y excluding
        // 2100,etc = 14 leap days in 1972-2024. Jan 1 2026 = 20440+14 = 20454.
        // + 31 (Jan) + 28 (Feb) + 31 (Mar) + 30 (Apr) + 11 = 131 → 20585.
        assert_eq!(days_to_ymd(20585), (2026, 5, 12));
    }

    #[test]
    fn iso8601_is_well_formed() {
        let s = now_iso8601_utc();
        assert!(s.ends_with('Z'), "must be UTC: {s}");
        assert_eq!(s.len(), 20, "expected YYYY-MM-DDTHH:MM:SSZ: {s}");
        assert!(s.contains('T'), "missing T separator: {s}");
    }

    /// Integration: full `run_lint_v3` pipeline using OYSTER_LINT_V3_PY
    /// pointing at a tiny stub script. Verifies that (a) the recorder
    /// invokes the script with the session dir, (b) reads its
    /// `--output` file, (c) writes `lint_result.json` with FAIL +
    /// the expected failure entries, mirroring Howard's NULL-data
    /// session.
    ///
    /// Skipped if `python3` (POSIX) / `python` (Windows) is not on
    /// PATH; we'd rather skip than fabricate a fake pass.
    #[tokio::test]
    async fn run_lint_v3_writes_fail_result_for_null_session() {
        let python_bin = resolve_python();
        // Probe for python availability — skip silently if absent so
        // CI hosts without python don't fail this test.
        if std::process::Command::new(&python_bin)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            eprintln!("skipping: {python_bin} not available");
            return;
        }

        let tmp = TempDir::new().expect("tempdir");
        let session_dir = tmp.path().to_path_buf();
        // Place a minimal action_camera.json so the script "sees" the
        // session (the stub below doesn't read it, but a real
        // integration is more realistic).
        fs::write(
            session_dir.join("action_camera.json"),
            b"[{\"frame_index\":0}]",
        )
        .unwrap();

        // Stub lint script: emits the same JSON shape as the real one,
        // honouring `--output`. This lets us test the wire contract
        // without depending on ffprobe / imageio / numpy.
        let stub_path = tmp.path().join("stub_lint.py");
        let stub_src = r#"
import argparse, json, sys
p = argparse.ArgumentParser()
p.add_argument("data_dir")
p.add_argument("--output", "-o", required=True)
p.add_argument("--verbose", "-v", action="store_true")
p.add_argument("--strict", action="store_true")
args = p.parse_args()
results = []
for i in range(1, 33):
    if i == 12:
        results.append({"id": i, "name": "camera_intrinsics", "passed": False,
                        "message": "fx is null on 305/305 frames", "details": {}})
    elif i == 13:
        results.append({"id": i, "name": "quaternion_shape", "passed": False,
                        "message": "rotation_quaternion is null on 305/305 frames",
                        "details": {}})
    elif i == 22:
        results.append({"id": i, "name": "metadata_required_fields",
                        "passed": False,
                        "message": "metadata.json missing required field: gameProcessName",
                        "details": {}})
    else:
        results.append({"id": i, "name": f"criterion_{i}", "passed": True,
                        "message": "ok", "details": {}})
body = {"data_dir": args.data_dir,
        "summary": {"total": 32, "passed": 29, "failed": 3, "pass_rate": "90.6%"},
        "results": results}
with open(args.output, "w") as f:
    json.dump(body, f, indent=2)
sys.exit(1)
"#;
        fs::write(&stub_path, stub_src.as_bytes()).unwrap();
        // SAFETY: integration test single-threaded sets env then immediately
        // reads it inside this task. No concurrent test mutates this var.
        unsafe {
            std::env::set_var("OYSTER_LINT_V3_PY", &stub_path);
        }

        let result = run_lint_v3(session_dir.clone()).await.expect("io");

        assert_eq!(result.overall_status, "FAIL");
        assert_eq!(result.failed, 3);
        assert_eq!(result.failures.len(), 3);
        let ids: Vec<u32> = result.failures.iter().map(|f| f.criterion).collect();
        assert_eq!(ids, vec![12, 13, 22]);

        // lint_result.json is on disk in the session_dir.
        let written = session_dir.join(LINT_RESULT_FILENAME);
        assert!(written.is_file(), "lint_result.json must exist");
        let on_disk: serde_json::Value =
            serde_json::from_slice(&fs::read(&written).unwrap()).unwrap();
        assert_eq!(on_disk["overall_status"], "FAIL");
        assert_eq!(on_disk["failed"], 3);
        assert_eq!(on_disk["failures"][0]["criterion"], 12);

        unsafe {
            std::env::remove_var("OYSTER_LINT_V3_PY");
        }
    }
}
