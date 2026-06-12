use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use color_eyre::{Result, eyre};
use egui_wgpu::wgpu;
use serde::{Deserialize, Serialize};

use crate::{
    api::{ApiClient, ApiError, CompleteMultipartUploadChunk},
    output_types::Metadata,
    system::{hardware_id, hardware_specs},
    util::durable_write,
};

/// Pinhole camera intrinsics for the recorded frame.
///
/// Derived analytically from the encoded resolution
/// (`constants::RECORDING_WIDTH`/`HEIGHT`) and the game's vertical FOV — NOT
/// measured from the running game. The `source` field states this honestly so
/// downstream AI-training consumers never mistake an assumed default for a
/// per-session measurement (the same data-integrity rule the FOV-in-LEM and
/// fps_effective fixes followed: emit "unknown"/"assumed" rather than a
/// plausible-looking lie).
///
/// Conventions:
///   - `fx`/`fy` are focal lengths in pixels. For square pixels (the case for
///     a standard 16:9 game render) `fx == fy`. `fy` is set from the vertical
///     FOV and `fx` is set equal to it.
///   - `cx`/`cy` are the principal point, assumed to be the image center.
///   - This struct is serialized as the nested `camera_intrinsics` object that
///     is injected into `metadata.json` (see `inject_extra_metadata_fields`).
///     It is intentionally defined here rather than on the shared `Metadata`
///     struct so the wire field can be added without disturbing that type.
// `Serialize` only — the `&'static str` fields (`model`, `source`) make this
// write-only. `#[derive(Deserialize)]` would not compile for borrowed-static
// string fields, and nothing reads `camera_intrinsics` back into this struct
// (downstream consumers parse the JSON object directly).
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct CameraIntrinsics {
    /// Camera model. Always `"pinhole"` for this analytic derivation.
    pub model: &'static str,
    /// Image width in pixels (matches the encoded video width).
    pub width: u32,
    /// Image height in pixels (matches the encoded video height).
    pub height: u32,
    /// Horizontal focal length in pixels.
    pub fx: f64,
    /// Vertical focal length in pixels.
    pub fy: f64,
    /// Principal point x (image center) in pixels.
    pub cx: f64,
    /// Principal point y (image center) in pixels.
    pub cy: f64,
    /// Vertical field of view in degrees used to derive `fy`.
    pub vfov_deg: f64,
    /// Provenance of these intrinsics. `"assumed_mc_default"` marks the values
    /// as derived from the assumed default FOV, NOT measured from the game.
    pub source: &'static str,
}

impl CameraIntrinsics {
    /// Compute pinhole intrinsics from a pixel resolution and a vertical FOV.
    ///
    /// Formula (standard pinhole, principal point at center, square pixels):
    /// ```text
    ///   vfov_rad = vfov_deg * PI / 180
    ///   fy       = (height / 2) / tan(vfov_rad / 2)
    ///   fx       = fy                       (square pixels)
    ///   cx       = width  / 2
    ///   cy       = height / 2
    /// ```
    ///
    /// `source` is caller-supplied so the provenance label stays honest — the
    /// recorder passes `"assumed_mc_default"` because the FOV is assumed, not
    /// measured.
    fn from_resolution_and_vfov(
        width: u32,
        height: u32,
        vfov_deg: f64,
        source: &'static str,
    ) -> Self {
        let vfov_rad = vfov_deg * std::f64::consts::PI / 180.0;
        let fy = (height as f64 / 2.0) / (vfov_rad / 2.0).tan();
        let fx = fy; // square pixels
        let cx = width as f64 / 2.0;
        let cy = height as f64 / 2.0;
        Self {
            model: "pinhole",
            width,
            height,
            fx,
            fy,
            cx,
            cy,
            vfov_deg,
            source,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UploadProgressState {
    pub upload_id: String,
    pub game_control_id: String,
    pub tar_path: PathBuf,
    pub chunk_etags: Vec<CompleteMultipartUploadChunk>,
    pub total_chunks: u64,
    pub chunk_size_bytes: u64,
    /// Unix timestamp when the upload session expires
    pub expires_at: u64,
}

impl UploadProgressState {
    /// Create a new upload progress state from a fresh upload session
    pub fn new(
        upload_id: String,
        game_control_id: String,
        tar_path: PathBuf,
        total_chunks: u64,
        chunk_size_bytes: u64,
        expires_at: u64,
    ) -> Self {
        Self {
            upload_id,
            game_control_id,
            tar_path,
            chunk_etags: vec![],
            total_chunks,
            chunk_size_bytes,
            expires_at,
        }
    }

    /// Check if the upload session has expired
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now >= self.expires_at
    }

    /// Get the number of seconds until expiration
    pub fn seconds_until_expiration(&self) -> i64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.expires_at as i64 - now as i64
    }

    /// Load progress state from a file
    pub fn load_from_file(path: &Path) -> eyre::Result<Self> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let mut stream =
            serde_json::Deserializer::from_reader(reader).into_iter::<serde_json::Value>();

        // Read the first object which should be the UploadProgressState
        let first_value = stream
            .next()
            .ok_or_else(|| eyre::eyre!("Empty progress file"))??;
        let mut state: Self = serde_json::from_value(first_value)?;

        // If the state was saved in the old format (single JSON object with populated etags),
        // we're done (the etags are already in state.chunk_etags).
        // If it was saved in the new format (header + log lines), state.chunk_etags might be empty,
        // and we need to read the rest of the file.

        // Read subsequent objects as CompleteMultipartUploadChunk
        for value in stream {
            let chunk: CompleteMultipartUploadChunk = serde_json::from_value(value?)?;
            // Avoid duplicates if we're migrating or recovering from a weird state
            if !state
                .chunk_etags
                .iter()
                .any(|c| c.chunk_number == chunk.chunk_number)
            {
                state.chunk_etags.push(chunk);
            }
        }

        Ok(state)
    }

    /// Save progress state to a file (Snapshot + Log format)
    pub fn save_to_file(&self, path: &Path) -> eyre::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = std::io::BufWriter::new(file);

        // 1. Write the base state with EMPTY chunk_etags to the first line.
        // We clone to clear the vector without modifying self.
        let mut header_state = self.clone();
        header_state.chunk_etags.clear();
        serde_json::to_writer(&mut writer, &header_state)?;
        use std::io::Write;
        writeln!(&mut writer)?;

        // 2. Write all existing etags as subsequent lines
        for chunk in &self.chunk_etags {
            serde_json::to_writer(&mut writer, chunk)?;
            writeln!(&mut writer)?;
        }

        writer.flush()?;
        // Ensure data reaches disk before returning — protects against
        // data loss on power failure mid-upload.
        writer.get_ref().sync_all()?;
        Ok(())
    }

    /// Get the next chunk number to upload (after the last completed chunk)
    pub fn next_chunk_number(&self) -> u64 {
        self.chunk_etags
            .iter()
            .map(|c| c.chunk_number)
            .max()
            .map(|n| n + 1)
            .unwrap_or(1)
    }

    /// Get the total number of bytes uploaded so far
    pub fn uploaded_bytes(&self) -> u64 {
        self.chunk_etags.len() as u64 * self.chunk_size_bytes
    }

    /// Cleans up the tar file associated with this upload progress.
    pub fn cleanup_tar_file(&self) {
        std::fs::remove_file(&self.tar_path).ok();
    }
}

#[derive(Debug, Clone)]
pub struct LocalRecordingInfo {
    pub folder_name: String,
    pub folder_path: PathBuf,
    pub folder_size: u64,
    pub timestamp: Option<std::time::SystemTime>,
}

/// Parse the timestamp out of a session folder name.
///
/// Supports three historical formats:
/// 1. `session_YYYYMMDD_HHMMSS_<suffix>` — current (post bug-2 fix)
/// 2. `session_YYYYMMDD_HHMMSS`          — pre-suffix
/// 3. bare unix seconds (stringified `u64`) — very old
///
/// Returns `None` for folders that don't match any known format.
fn parse_session_timestamp(folder_name: &str) -> Option<std::time::SystemTime> {
    // Format 1 & 2: strip optional `_<suffix>` tail, then parse
    // `session_YYYYMMDD_HHMMSS`.
    if let Some(rest) = folder_name.strip_prefix("session_") {
        // rest = "YYYYMMDD_HHMMSS" or "YYYYMMDD_HHMMSS_suffix"
        // Take only the first two underscore-separated segments (date, time)
        // so any trailing suffix is ignored.
        let mut parts = rest.splitn(3, '_');
        let date_part = parts.next()?;
        let time_part = parts.next()?;
        let combined = format!("{date_part}{time_part}");
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&combined, "%Y%m%d%H%M%S") {
            // Interpret as local time since `generate_session_dir_name` uses Local.
            let local: chrono::DateTime<chrono::Local> =
                chrono::TimeZone::from_local_datetime(&chrono::Local, &naive).single()?;
            let secs = local.timestamp();
            if secs < 0 {
                return None;
            }
            return Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64));
        }
    }
    // Format 3 (legacy): bare u64 seconds
    folder_name
        .parse::<u64>()
        .ok()
        .map(|secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
}

/// Convert a `SystemTime` to Unix-epoch-UTC seconds as `f64`.
///
/// This is the single, audited source of truth for every "utc"/"wall_clock"
/// value this module emits. `SystemTime` is a clock-absolute instant, and
/// `duration_since(UNIX_EPOCH)` measures its offset from the epoch in UTC — so
/// the result is genuine epoch-UTC, NOT a local-time value formatted as UTC
/// (the mislabeling behind the SS5 future/timezone-shifted-timestamp bug).
///
/// Returns `None` for the impossible-but-defensive case of a time strictly
/// before the Unix epoch (e.g. a corrupted/zeroed clock), letting the caller
/// decide on a fallback rather than silently emitting a negative timestamp.
fn utc_secs_f64(t: SystemTime) -> Option<f64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs_f64())
}

/// A detected wall-clock anomaly, recorded additively in `metadata.json` so a
/// downstream consumer can quarantine the session. Serialized as the nested
/// `clock_anomaly` object; only present when an anomaly was actually detected.
///
/// `Serialize` only — this is a write-only diagnostic; nothing reads it back
/// into this struct.
#[derive(Debug, Clone, Serialize, PartialEq)]
struct ClockAnomaly {
    /// Machine-readable anomaly kind: `"start_after_stop"` (wall clock moved
    /// backward between start and stop) or `"wall_gap_disagrees_with_monotonic"`
    /// (the wall-clock start→stop gap diverges from the monotonic recording
    /// duration, indicating the wall clock was stepped mid-recording).
    kind: &'static str,
    /// The recorded UTC start timestamp (epoch seconds) that triggered the flag.
    start_utc_secs: f64,
    /// The recorded UTC stop timestamp (epoch seconds).
    end_utc_secs: f64,
    /// Monotonic recording duration (seconds) from `Instant` — unaffected by the
    /// wall-clock skew, so it stays authoritative for the true recording length.
    monotonic_duration_secs: f64,
}

/// Detect a wall-clock anomaly between the recorded start/stop epoch-UTC
/// timestamps, cross-checked against the monotonic recording duration.
///
/// Pure function (no clock reads) so the policy is unit-testable. Flags:
///   1. `start_after_stop` — `start > end`. The wall clock went backward
///      between capturing `start_time` and `SystemTime::now()` at stop, so the
///      recording appears to end before it began. This is the direct form of
///      the SS5 "future-dated start" hazard.
///   2. `wall_gap_disagrees_with_monotonic` — the wall-clock gap
///      (`end - start`) differs from the monotonic recording duration by more
///      than a generous tolerance. The two should agree to within a second; a
///      large disagreement means the wall clock was stepped (NTP correction, VM
///      catch-up, manual change) DURING the recording, so the UTC start/stop —
///      and any future-dated value derived from them — are untrustworthy even
///      though `start <= end` still holds.
///
/// Returns `None` when the timestamps are self-consistent (the common case).
fn detect_clock_anomaly(
    start_utc_secs: f64,
    end_utc_secs: f64,
    monotonic_duration_secs: f64,
) -> Option<ClockAnomaly> {
    // Non-finite inputs can't be reasoned about; don't fabricate an anomaly.
    if !start_utc_secs.is_finite() || !end_utc_secs.is_finite() {
        return None;
    }

    // Slack absorbs benign sub-second ordering between the start capture and the
    // stop `SystemTime::now()` (they are read at slightly different points).
    const ORDERING_SLACK_SECS: f64 = 1.0;

    // (1) Start is after stop: the wall clock moved backward between start and
    // stop, so the recording "ends before it began" — the SS5 hazard directly.
    if start_utc_secs > end_utc_secs + ORDERING_SLACK_SECS {
        return Some(ClockAnomaly {
            kind: "start_after_stop",
            start_utc_secs,
            end_utc_secs,
            monotonic_duration_secs,
        });
    }

    // (2) Wall-clock gap should ≈ monotonic duration. A large disagreement means
    // the wall clock was stepped mid-recording, so the UTC stamps are suspect.
    // Only meaningful when we actually have a monotonic duration to compare to.
    if monotonic_duration_secs.is_finite() && monotonic_duration_secs > 0.0 {
        let wall_gap = end_utc_secs - start_utc_secs;
        // Tolerance: the larger of a fixed floor and a small fraction of the
        // duration, so long recordings aren't flagged for normal scheduling
        // jitter while short ones still catch big absolute steps.
        let tolerance = ORDERING_SLACK_SECS.max(monotonic_duration_secs * 0.10);
        if (wall_gap - monotonic_duration_secs).abs() > tolerance {
            return Some(ClockAnomaly {
                kind: "wall_gap_disagrees_with_monotonic",
                start_utc_secs,
                end_utc_secs,
                monotonic_duration_secs,
            });
        }
    }

    None
}

#[cfg(test)]
mod camera_intrinsics_tests {
    use super::CameraIntrinsics;

    #[test]
    fn pinhole_1080p_70deg_vfov_matches_expected() {
        // Standard delivery shape: 1920x1080 @ 70° vertical FOV.
        let k = CameraIntrinsics::from_resolution_and_vfov(1920, 1080, 70.0, "assumed_mc_default");

        // fy = (1080/2) / tan(70°/2) = 540 / tan(35°) ≈ 771.199924
        assert!((k.fy - 771.199_923_641).abs() < 1e-6, "fy was {}", k.fy);
        // Square pixels: fx == fy.
        assert_eq!(k.fx, k.fy, "fx must equal fy for square pixels");
        // Principal point at image center.
        assert_eq!(k.cx, 960.0);
        assert_eq!(k.cy, 540.0);
        assert_eq!(k.width, 1920);
        assert_eq!(k.height, 1080);
        assert_eq!(k.model, "pinhole");
        assert_eq!(k.vfov_deg, 70.0);
        // Provenance must stay honest.
        assert_eq!(k.source, "assumed_mc_default");
    }

    #[test]
    fn serializes_as_expected_nested_object() {
        let k = CameraIntrinsics::from_resolution_and_vfov(1920, 1080, 70.0, "assumed_mc_default");
        let v = serde_json::to_value(k).expect("intrinsics must serialize");
        assert_eq!(v["model"], "pinhole");
        assert_eq!(v["width"], 1920);
        assert_eq!(v["height"], 1080);
        assert_eq!(v["source"], "assumed_mc_default");
        // fx/fy/cx/cy present and finite (valid JSON numbers, not NaN).
        for key in ["fx", "fy", "cx", "cy", "vfov_deg"] {
            assert!(v[key].is_number(), "{key} must be a JSON number");
        }
    }

    #[test]
    fn uses_repo_recording_constants_and_mc_fov() {
        // Guards against the constants drifting out from under the metadata.
        let k = CameraIntrinsics::from_resolution_and_vfov(
            constants::RECORDING_WIDTH,
            constants::RECORDING_HEIGHT,
            constants::MC_DEFAULT_VFOV_DEG,
            "assumed_mc_default",
        );
        assert_eq!(k.width, constants::RECORDING_WIDTH);
        assert_eq!(k.height, constants::RECORDING_HEIGHT);
        assert_eq!(k.cx, constants::RECORDING_WIDTH as f64 / 2.0);
        assert_eq!(k.cy, constants::RECORDING_HEIGHT as f64 / 2.0);
    }
}

#[cfg(test)]
mod metadata_injection_tests {
    use super::LocalRecording;

    #[test]
    fn injects_camera_intrinsics_and_timezone_fields() {
        // Start from a JSON object shaped like a minimal serialized Metadata.
        let mut value = serde_json::json!({
            "game_exe": "javaw.exe",
            "session_id": "tz-inject-0001",
            "wall_clock_start": "2026-05-29T21:30:00+00:00",
        });

        LocalRecording::inject_extra_metadata_fields(&mut value);

        // camera_intrinsics is a nested object with the honest source tag.
        let ci = &value["camera_intrinsics"];
        assert!(ci.is_object(), "camera_intrinsics must be a nested object");
        assert_eq!(ci["model"], "pinhole");
        assert_eq!(ci["source"], "assumed_mc_default");
        assert_eq!(ci["width"], constants::RECORDING_WIDTH);
        assert_eq!(ci["height"], constants::RECORDING_HEIGHT);

        // Timezone disambiguation fields exist and are correctly typed.
        assert!(
            value["timezone_utc_offset_seconds"].is_i64(),
            "offset must be an integer number of seconds"
        );
        assert_eq!(value["session_dir_timezone"], "local");
    }

    #[test]
    fn non_object_value_is_a_noop_and_does_not_panic() {
        // Defensive: if metadata ever serialized to a non-object, injection
        // must not panic on the finalize path.
        let mut value = serde_json::json!("not an object");
        LocalRecording::inject_extra_metadata_fields(&mut value);
        assert_eq!(value, serde_json::json!("not an object"));
    }
}

#[cfg(test)]
mod parse_timestamp_tests {
    use super::parse_session_timestamp;

    #[test]
    fn parses_new_format_with_suffix() {
        // 2026-01-15 14:30:22 local
        let ts = parse_session_timestamp("session_20260115_143022_deadbeef");
        assert!(ts.is_some(), "should parse new format with suffix");
    }

    #[test]
    fn parses_old_format_without_suffix() {
        let ts = parse_session_timestamp("session_20260115_143022");
        assert!(ts.is_some(), "should parse old format without suffix");
    }

    #[test]
    fn parses_legacy_bare_seconds() {
        let ts = parse_session_timestamp("1737000000");
        assert!(ts.is_some(), "should parse legacy bare-seconds format");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_session_timestamp("not-a-session").is_none());
        assert!(parse_session_timestamp("session_bad_time").is_none());
        assert!(parse_session_timestamp("").is_none());
    }
}

#[cfg(test)]
mod clock_math_tests {
    use super::{ClockAnomaly, detect_clock_anomaly, utc_secs_f64};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn utc_secs_is_genuine_epoch_utc() {
        // A SystemTime built from a known epoch offset must round-trip to that
        // same offset — proving we measure UTC-from-epoch, not local time.
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let secs = utc_secs_f64(t).expect("post-epoch time must convert");
        assert!((secs - 1_700_000_000.0).abs() < 1e-6, "got {secs}");
    }

    #[test]
    fn utc_secs_epoch_is_zero() {
        assert_eq!(utc_secs_f64(UNIX_EPOCH), Some(0.0));
    }

    #[test]
    fn utc_secs_before_epoch_is_none() {
        // Defensive: a clock set before 1970 must not yield a negative timestamp.
        let before = UNIX_EPOCH - Duration::from_secs(10);
        assert_eq!(utc_secs_f64(before), None);
    }

    #[test]
    fn healthy_recording_has_no_anomaly() {
        // start at T, stop ~120s later, monotonic duration ~120s: consistent.
        let start = 1_700_000_000.0;
        let end = start + 120.3;
        assert_eq!(detect_clock_anomaly(start, end, 120.0), None);
    }

    #[test]
    fn sub_second_ordering_noise_is_tolerated() {
        // Stop stamped a hair before start due to read ordering; within slack.
        let start = 1_700_000_000.5;
        let end = 1_700_000_000.0;
        assert_eq!(detect_clock_anomaly(start, end, 0.0), None);
    }

    #[test]
    fn start_after_stop_is_flagged() {
        // Wall clock jumped backward: start is well after stop (SS5 hazard).
        let start = 1_700_000_500.0;
        let end = 1_700_000_000.0;
        let anomaly = detect_clock_anomaly(start, end, 60.0).expect("must flag");
        assert_eq!(anomaly.kind, "start_after_stop");
        // Monotonic duration is preserved verbatim for downstream triage.
        assert_eq!(anomaly.monotonic_duration_secs, 60.0);
        assert_eq!(anomaly.start_utc_secs, start);
        assert_eq!(anomaly.end_utc_secs, end);
    }

    #[test]
    fn forward_clock_step_during_recording_is_flagged() {
        // Monotonic duration says 60s, but the wall clock advanced 3600s between
        // start and stop — the clock was stepped forward mid-recording.
        let start = 1_700_000_000.0;
        let end = start + 3600.0;
        let anomaly = detect_clock_anomaly(start, end, 60.0).expect("must flag");
        assert_eq!(anomaly.kind, "wall_gap_disagrees_with_monotonic");
    }

    #[test]
    fn long_recording_tolerates_proportional_jitter() {
        // 10-minute recording; wall gap off by ~30s (<10% of 600s) is fine.
        let start = 1_700_000_000.0;
        let end = start + 600.0 + 30.0;
        assert_eq!(detect_clock_anomaly(start, end, 600.0), None);
    }

    #[test]
    fn non_finite_inputs_do_not_panic_or_fabricate() {
        assert_eq!(detect_clock_anomaly(f64::NAN, 1.0, 1.0), None);
        assert_eq!(detect_clock_anomaly(1.0, f64::INFINITY, 1.0), None);
    }

    #[test]
    fn anomaly_serializes_as_nested_object() {
        let a = ClockAnomaly {
            kind: "start_after_stop",
            start_utc_secs: 2.0,
            end_utc_secs: 1.0,
            monotonic_duration_secs: 0.5,
        };
        let v = serde_json::to_value(a).expect("must serialize");
        assert_eq!(v["kind"], "start_after_stop");
        assert!(v["start_utc_secs"].is_number());
        assert!(v["end_utc_secs"].is_number());
        assert!(v["monotonic_duration_secs"].is_number());
    }
}

#[cfg(test)]
mod encoder_used_tests {
    use super::LocalRecording;

    #[test]
    fn promotes_encoder_from_recorder_extra() {
        let mut value = serde_json::json!({
            "game_exe": "javaw.exe",
            "recorder_extra": { "encoder": "obs_nvenc_hevc_tex", "window_capture": false },
        });
        LocalRecording::inject_encoder_used(&mut value);
        assert_eq!(value["encoder_used"], "obs_nvenc_hevc_tex");
    }

    #[test]
    fn absent_when_no_recorder_extra() {
        // Socket recorder path: recorder_extra is null → no fabricated encoder.
        let mut value = serde_json::json!({
            "game_exe": "javaw.exe",
            "recorder_extra": serde_json::Value::Null,
        });
        LocalRecording::inject_encoder_used(&mut value);
        assert!(
            value.get("encoder_used").is_none(),
            "must not fabricate an encoder when none is known"
        );
    }

    #[test]
    fn absent_when_recorder_extra_missing_entirely() {
        let mut value = serde_json::json!({ "game_exe": "javaw.exe" });
        LocalRecording::inject_encoder_used(&mut value);
        assert!(value.get("encoder_used").is_none());
    }

    #[test]
    fn does_not_overwrite_existing_encoder_used() {
        let mut value = serde_json::json!({
            "encoder_used": "preset_value",
            "recorder_extra": { "encoder": "obs_x264" },
        });
        LocalRecording::inject_encoder_used(&mut value);
        assert_eq!(value["encoder_used"], "preset_value");
    }

    #[test]
    fn non_object_value_is_a_noop() {
        let mut value = serde_json::json!("not an object");
        LocalRecording::inject_encoder_used(&mut value);
        assert_eq!(value, serde_json::json!("not an object"));
    }
}

impl std::fmt::Display for LocalRecordingInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.folder_name, self.folder_path.display())
    }
}

/// A recording that has a paused upload in progress.
/// This struct guarantees that the upload state has been validated and is ready to resume.
#[derive(Debug, Clone)]
pub struct LocalRecordingPaused {
    pub info: LocalRecordingInfo,
    pub metadata: Option<Box<Metadata>>,
    upload_progress: UploadProgressState,
}

impl LocalRecordingPaused {
    pub fn new(
        info: LocalRecordingInfo,
        metadata: Option<Box<Metadata>>,
        upload_progress: UploadProgressState,
    ) -> Self {
        Self {
            info,
            metadata,
            upload_progress,
        }
    }

    /// Cleans up upload artifacts (progress file and tar file).
    pub fn cleanup_upload_artifacts(self) {
        std::fs::remove_file(self.upload_progress_path()).ok();
        self.upload_progress.cleanup_tar_file();
        tracing::info!(
            "Cleaned up upload artifacts for upload_id={}",
            self.upload_progress.upload_id
        );
    }

    /// Get a reference to the upload progress state.
    pub fn upload_progress(&self) -> &UploadProgressState {
        &self.upload_progress
    }

    /// Records a successful chunk upload: updates in-memory state and appends to the log file.
    pub fn record_chunk_completion(
        &mut self,
        chunk: CompleteMultipartUploadChunk,
    ) -> eyre::Result<()> {
        // Append to disk first (before updating in-memory state)
        // This ensures that if the process crashes, the disk state is consistent
        // and can be recovered on restart, even if in-memory state is lost.
        let path = self.upload_progress_path();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(false) // Should already exist
            .open(path)?;

        serde_json::to_writer(&mut file, &chunk)?;
        use std::io::Write;
        writeln!(&mut file)?;
        file.sync_all()?; // Ensure data is flushed to disk before updating in-memory state

        // Update in-memory state after successful disk write
        self.upload_progress.chunk_etags.push(chunk);

        Ok(())
    }

    /// Save upload progress state to .upload-progress file.
    pub fn save_upload_progress(&self) -> eyre::Result<()> {
        self.upload_progress
            .save_to_file(&self.upload_progress_path())
    }

    pub async fn abort_and_cleanup(
        self,
        api_client: &ApiClient,
        api_token: &str,
    ) -> Result<(), ApiError> {
        let response = api_client
            .abort_multipart_upload(api_token, &self.upload_progress.upload_id)
            .await;
        tracing::info!(
            "Aborted multipart upload for upload_id={}",
            self.upload_progress.upload_id
        );
        self.cleanup_upload_artifacts();
        response.map(|_| ())
    }

    /// Mark recording as uploaded, writing .uploaded marker file.
    /// Consumes self and returns Uploaded LocalRecording variant.
    pub fn mark_as_uploaded(self, game_control_id: String) -> std::io::Result<LocalRecording> {
        let info = self.info.clone();
        self.cleanup_upload_artifacts();
        // Atomic write: a crash between creating `.uploaded` and flushing its
        // single-line payload could leave us with an empty marker file, which
        // `LocalRecording::from_path` then reads as `game_control_id = ""`
        // and treats as a successful upload we can no longer correlate.
        durable_write::write_atomic(
            &info
                .folder_path
                .join(constants::filename::recording::UPLOADED),
            game_control_id.as_bytes(),
        )?;
        tracing::info!(
            "Marked recording as uploaded: game_control_id={}, folder_path={}",
            game_control_id,
            info.folder_path.display()
        );
        Ok(LocalRecording::Uploaded {
            info,
            game_control_id,
        })
    }

    /// Mark recording as server-invalid, writing .server_invalid marker.
    /// Consumes self and returns Invalid LocalRecording variant.
    pub fn mark_as_server_invalid(self, message: &str) -> std::io::Result<LocalRecording> {
        let info = self.info.clone();
        let metadata = self.metadata.clone();
        self.cleanup_upload_artifacts();
        // Atomic so the error message reaches disk as a unit — otherwise a
        // truncated SERVER_INVALID file would still flip the recording into
        // the Invalid variant but with garbled error_reasons.
        durable_write::write_atomic(
            &info
                .folder_path
                .join(constants::filename::recording::SERVER_INVALID),
            message.as_bytes(),
        )?;
        tracing::info!(
            "Marked recording as server-invalid: message={}, folder_path={}",
            message,
            info.folder_path.display()
        );
        Ok(LocalRecording::Invalid {
            info,
            metadata,
            error_reasons: message.lines().map(String::from).collect(),
            by_server: true,
        })
    }

    fn upload_progress_path(&self) -> PathBuf {
        self.info
            .folder_path
            .join(constants::filename::recording::UPLOAD_PROGRESS)
    }
}

#[derive(Debug, Clone)]
pub enum LocalRecording {
    Invalid {
        info: LocalRecordingInfo,
        metadata: Option<Box<Metadata>>,
        error_reasons: Vec<String>,
        by_server: bool,
    },
    Unuploaded {
        info: LocalRecordingInfo,
        metadata: Option<Box<Metadata>>,
    },
    Paused(LocalRecordingPaused),
    Uploaded {
        info: LocalRecordingInfo,
        #[allow(dead_code)]
        game_control_id: String,
    },
}

impl LocalRecording {
    /// Creates the recording folder at the given path if it doesn't already exist.
    /// Returns a LocalRecording::Unuploaded variant. Called at .start() of recording.
    pub fn create_at(path: &Path) -> Result<LocalRecording> {
        std::fs::create_dir_all(path)?;

        // Build info similar to from_path
        let folder_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Unknown")
            .to_string();

        let timestamp = parse_session_timestamp(&folder_name);

        let info = LocalRecordingInfo {
            folder_name,
            folder_size: 0, // New folder, no content yet
            folder_path: path.to_path_buf(),
            timestamp,
        };

        Ok(LocalRecording::Unuploaded {
            info,
            metadata: None,
        })
    }

    /// Get the common info for any recording variant
    pub fn info(&self) -> &LocalRecordingInfo {
        match self {
            LocalRecording::Invalid { info, .. } => info,
            LocalRecording::Unuploaded { info, .. } => info,
            LocalRecording::Paused(paused) => &paused.info,
            LocalRecording::Uploaded { info, .. } => info,
        }
    }

    /// Get the metadata for any recording variant
    pub fn metadata(&self) -> Option<&Metadata> {
        match self {
            LocalRecording::Invalid { metadata, .. } => metadata.as_deref(),
            LocalRecording::Unuploaded { metadata, .. } => metadata.as_deref(),
            LocalRecording::Paused(paused) => paused.metadata.as_deref(),
            LocalRecording::Uploaded { .. } => None,
        }
    }

    /// Convenience accessor for error reasons (only for Invalid variant)
    #[allow(dead_code)]
    pub fn error_reasons(&self) -> Option<&[String]> {
        match self {
            LocalRecording::Invalid { error_reasons, .. } => Some(error_reasons),
            _ => None,
        }
    }

    /// Deletes the recording folder and cleans up server state.
    /// For Paused uploads, aborts the multipart upload on the server.
    pub async fn delete(self, api_client: &ApiClient, api_token: &str) -> std::io::Result<()> {
        let folder_path = self.info().folder_path.clone();

        // For Paused variant, abort the upload on the server first
        if let LocalRecording::Paused(paused) = self {
            paused.abort_and_cleanup(api_client, api_token).await.ok();
        }

        tokio::fs::remove_dir_all(&folder_path).await
    }

    /// Deletes the recording folder synchronously. Use this only in Drop handlers
    /// where async is not available. Does NOT abort server uploads.
    pub fn delete_without_abort_sync(&self) -> std::io::Result<()> {
        std::fs::remove_dir_all(&self.info().folder_path)
    }

    /// Scans a single recording folder and returns its state
    pub fn from_path(path: &Path) -> Option<LocalRecording> {
        if !path.is_dir() {
            return None;
        }

        let invalid_file_path = path.join(constants::filename::recording::INVALID);
        let server_invalid_file_path = path.join(constants::filename::recording::SERVER_INVALID);
        let uploaded_file_path = path.join(constants::filename::recording::UPLOADED);
        let upload_progress_file_path = path.join(constants::filename::recording::UPLOAD_PROGRESS);
        let metadata_path = path.join(constants::filename::recording::METADATA);

        // Get the folder name
        let folder_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Unknown")
            .to_string();

        // Parse the timestamp from the folder name. Handles all historical
        // formats including the current `session_YYYYMMDD_HHMMSS_<suffix>`.
        let timestamp = parse_session_timestamp(&folder_name);

        let info = LocalRecordingInfo {
            folder_name,
            folder_size: folder_size(path).unwrap_or_default(),
            folder_path: path.to_path_buf(),
            timestamp,
        };

        if uploaded_file_path.is_file() {
            // Read the game_control_id from the .uploaded file
            let game_control_id = std::fs::read_to_string(&uploaded_file_path)
                .unwrap_or_else(|_| "unknown".to_string())
                .trim()
                .to_string();

            Some(LocalRecording::Uploaded {
                info,
                game_control_id,
            })
        } else {
            // Not uploaded yet (and not invalid)
            let metadata: Option<Box<Metadata>> = std::fs::read_to_string(metadata_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .map(Box::new);

            if invalid_file_path.is_file() {
                // Read the error reasons from the [`constants::filename::recording::INVALID`] file
                let error_reasons = std::fs::read_to_string(&invalid_file_path)
                    .unwrap_or_else(|_| "Unknown error".to_string())
                    .lines()
                    .map(|s| s.to_string())
                    .collect();

                Some(LocalRecording::Invalid {
                    info,
                    metadata,
                    error_reasons,
                    by_server: false,
                })
            } else if server_invalid_file_path.is_file() {
                // Read the error reasons from the [`constants::filename::recording::SERVER_INVALID`] file
                let error_reasons = std::fs::read_to_string(&server_invalid_file_path)
                    .unwrap_or_else(|_| "Unknown error".to_string())
                    .lines()
                    .map(|s| s.to_string())
                    .collect();

                Some(LocalRecording::Invalid {
                    info,
                    metadata,
                    error_reasons,
                    by_server: true,
                })
            } else if upload_progress_file_path.is_file() {
                // Upload was paused - there's a .upload-progress file
                match UploadProgressState::load_from_file(&upload_progress_file_path) {
                    Ok(upload_progress) => Some(LocalRecording::Paused(LocalRecordingPaused {
                        info,
                        metadata,
                        upload_progress,
                    })),
                    Err(e) => {
                        // Corrupted progress file - treat as unuploaded so fresh upload can be attempted
                        tracing::warn!(
                            "Failed to load upload progress for {}, treating as unuploaded: {:?}",
                            info.folder_name,
                            e
                        );
                        Some(LocalRecording::Unuploaded { info, metadata })
                    }
                }
            } else {
                Some(LocalRecording::Unuploaded { info, metadata })
            }
        }
    }

    /// Scans the recording directory for all local recordings
    pub fn scan_directory(recording_location: &Path) -> Vec<LocalRecording> {
        let mut local_recordings = Vec::new();

        let Ok(entries) = recording_location.read_dir() else {
            return local_recordings;
        };

        for entry in entries.flatten() {
            if let Some(recording) = Self::from_path(&entry.path()) {
                local_recordings.push(recording);
            }
        }

        // Sort by timestamp, most recent first
        local_recordings.sort_by(|a, b| {
            b.info()
                .timestamp
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                .cmp(
                    &a.info()
                        .timestamp
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                )
        });

        local_recordings
    }

    /// Write metadata to disk and validate the recording.
    /// Creates a [`constants::filename::recording::INVALID`] file if validation fails.
    ///
    /// `recording_bitrate_kbps` is the clamped value handed to the OBS
    /// encoder at recording start (PRD R2.10) — it ends up in the on-disk
    /// `metadata.json` so the buyer ingest can audit per-session bitrate
    /// against the 6–12 Mbps band without re-reading user preferences.
    #[allow(clippy::too_many_arguments)]
    // TODO: refactor all of these arguments into a single struct
    pub(crate) async fn write_metadata_and_validate(
        recording_location: PathBuf,
        game_exe: String,
        game_resolution: (u32, u32),
        start_instant: Instant,
        start_time: SystemTime,
        average_fps: Option<f64>,
        window_name: Option<String>,
        adapter_infos: &[wgpu::AdapterInfo],
        gamepads: HashMap<input_capture::GamepadId, input_capture::GamepadMetadata>,
        recorder_id: &str,
        recorder_extra: Option<serde_json::Value>,
        frame_count: Option<u64>,
        dropped_input_events: u64,
        route_type: Option<u8>,
        recording_bitrate_kbps: u32,
    ) -> Result<()> {
        // Resolve metadata path from recording location
        let metadata_path = recording_location.join(constants::filename::recording::METADATA);

        // Create metadata
        let duration_nanos = start_instant.elapsed().as_nanos();
        let duration = start_instant.elapsed().as_secs_f64();
        let end_system_time = SystemTime::now();

        // Both timestamps are genuine Unix-epoch-UTC seconds: `SystemTime` is a
        // clock-absolute value, and `duration_since(UNIX_EPOCH)` measures the
        // offset from the epoch in UTC. This is NOT a local-time value formatted
        // as if it were UTC — that mislabeling was the class of bug behind the
        // SS5 recovery session (100703), where a `recording_started_utc` was a
        // future / timezone-shifted value. We derive epoch-UTC directly so the
        // emitted "utc"/"wall_clock" fields can never be a local-time lie.
        let start_timestamp = utc_secs_f64(start_time).unwrap_or_else(|| {
            tracing::warn!("Start time before UNIX epoch, using 0");
            0.0
        });
        let end_timestamp = utc_secs_f64(end_system_time).unwrap_or_else(|| {
            tracing::warn!("Current time before UNIX epoch, using 0");
            0.0
        });

        // Clock-skew / future-dated-start sanity guard (mirrors the fail-soft
        // guard in `recording.rs`'s game_state windowing). The wall-clock
        // SystemTime can jump backward between `start_time` capture and
        // `end_system_time = SystemTime::now()` (NTP step, manual clock change,
        // VM clock catch-up), which yields a start that is AFTER the stop or an
        // implausibly-far-future start — exactly the SS5 hazard. We do NOT
        // rewrite the recorded timestamps (silently editing history could itself
        // emit a wrong value, and the data team forbids changing existing field
        // meanings); instead we record an additive `clock_anomaly` diagnostic so
        // a buyer can detect and quarantine the session, and we log loudly.
        // Note: the `duration` / `duration_ns` fields are derived from the
        // MONOTONIC `Instant` (`start_instant.elapsed()`), so they remain correct
        // even when the wall clock is skewed.
        let clock_anomaly = detect_clock_anomaly(start_timestamp, end_timestamp, duration);
        if let Some(ref anomaly) = clock_anomaly {
            tracing::warn!(
                "Clock anomaly in recording metadata: kind={}, start_utc={}s, end_utc={}s, \
                 monotonic_duration={}s. Wall-clock timestamps are suspect; \
                 monotonic `duration`/`duration_ns` remain authoritative.",
                anomaly.kind,
                start_timestamp,
                end_timestamp,
                duration
            );
        }

        // fps_effective / frame_count fix (ISC-DATA-FPS): the `frame_count` arg comes
        // from `fps_logger`, whose `on_frame_fps()` is driven by the ~1 Hz `update_fps`
        // poll (embedded libobs exposes no per-frame callback), so it counts SECONDS,
        // not frames. Dividing it by duration yielded a bogus ~1 fps even though the
        // encoded mp4 is a real 30 fps — and `average_fps` (derived from
        // obs_output_get_total_frames) proves it. A buyer / PRD-audit reading the old
        // `fps_effective` would misclassify real 30 fps footage as a frozen 1 fps
        // capture. Report the TRUE OBS frame rate, and derive a real frame count from
        // it so the two fields agree with the mp4. Fall back to the heartbeat count
        // only when OBS reported no fps at all.
        let fps_effective = average_fps.filter(|_| duration > 0.0);
        let frame_count = match average_fps {
            Some(f) if duration > 0.0 => Some((f * duration).round() as u64),
            _ => frame_count,
        };

        // Wall-clock strings in RFC 3339 for human-friendly audit trails.
        let wall_clock_start = chrono::DateTime::<chrono::Utc>::from(start_time).to_rfc3339();
        let wall_clock_end = chrono::DateTime::<chrono::Utc>::from(end_system_time).to_rfc3339();

        // Capture resolution is what we encoded — currently fixed by constants::RECORDING_*.
        // Exposed as a field so downstream tools don't have to hard-code the constant.
        let capture_resolution = (constants::RECORDING_WIDTH, constants::RECORDING_HEIGHT);

        let hardware_id = hardware_id::get()?;

        let hardware_specs = match hardware_specs::get_hardware_specs(
            adapter_infos
                .iter()
                .map(|a| hardware_specs::GpuSpecs::from_name(&a.name))
                .collect(),
        ) {
            // R5.2: enrich GPU entries with driver_version. On non-Windows
            // hosts this is a no-op; on Windows it walks the DISPLAY_DEVICE
            // chain and substring-matches against the friendly name. We do
            // this AFTER `get_hardware_specs` so any failure in the
            // enrichment doesn't lose the rest of the data.
            Ok(mut specs) => {
                hardware_specs::enrich_gpu_specs_with_driver_version(&mut specs.gpus);
                Some(specs)
            }
            Err(e) => {
                tracing::warn!("Failed to get hardware specs: {}", e);
                None
            }
        };

        let metadata = Metadata {
            game_exe,
            game_resolution: Some(game_resolution),
            recorder_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            recorder_commit: Some(
                git_version::git_version!(
                    args = ["--abbrev=40", "--always", "--dirty=-modified"],
                    fallback = "unknown"
                )
                .to_string(),
            ),
            session_id: uuid::Uuid::new_v4().to_string(),
            hardware_id,
            hardware_specs,
            gamepads: gamepads
                .into_iter()
                .map(|(id, metadata)| (id, metadata.into()))
                .collect(),
            start_timestamp,
            end_timestamp,
            duration,
            input_stats: None,
            dropped_input_events: if dropped_input_events > 0 {
                Some(dropped_input_events)
            } else {
                None
            },
            recorder: Some(recorder_id.to_string()),
            recorder_extra,
            window_name,
            average_fps,
            platform: Some("Windows".to_string()),
            fps_effective,
            frame_count,
            duration_ns: Some(duration_nanos as u64),
            capture_resolution: Some(capture_resolution),
            wall_clock_start: Some(wall_clock_start),
            wall_clock_end: Some(wall_clock_end),
            // Only persist a tag the operator actually set (1..=3). Defensive
            // filter against malformed input (e.g. a future code path that
            // writes 4 / 255) — buyer's schema only accepts {1,2,3}, so we
            // would rather omit the field than ship a poison value the
            // pipeline can't reject without a separate validation pass.
            route_type: route_type.filter(|n| (1..=3).contains(n)),
            // R2.10: stamp the effective (clamped) encoder bitrate so the
            // buyer ingest can audit per-session bitrate against the
            // 6–12 Mbps band.
            encoder_bitrate_kbps: Some(recording_bitrate_kbps),
        };

        // Write metadata to disk using atomic + fsync'd write.
        //
        // The old implementation used `tokio::fs::write` + `tokio::fs::rename`,
        // which is atomic at the directory-entry level BUT skipped the
        // file-data fsync between write and rename. On power loss between
        // those two steps, the new inode's name would commit while its data
        // blocks sat in the page cache — leaving a 0-byte metadata.json
        // referencing whatever MP4 was next to it. `write_atomic_async`
        // inserts the missing `sync_all` call on the temp file and syncs
        // the parent directory on POSIX so the rename itself is durable.
        //
        // Note: this runs via spawn_blocking inside write_atomic_async, so the
        // tokio reactor isn't pinned while the fsync stalls (fsync on a busy
        // NVMe can take tens of ms; on a networked drive, seconds).
        //
        // Two metadata fields can't live on the shared `output_types::Metadata`
        // struct from here (it's owned by another module), so we serialize to a
        // JSON object first and inject them as top-level keys:
        //   1. `camera_intrinsics` — pinhole intrinsics derived from the encoded
        //      resolution + MC's assumed default vertical FOV. Honestly tagged
        //      `source: "assumed_mc_default"` so downstream never mistakes it
        //      for a measured value.
        //   2. timezone disambiguation — `wall_clock_start`/`wall_clock_end`
        //      are UTC (+00:00) while the SESSION DIRECTORY name is LOCAL time
        //      (`generate_session_dir_name` uses `Local::now()`). Emitting the
        //      local UTC offset + an explicit `session_dir_timezone` label lets
        //      a reader reconcile the two without guessing.
        let mut metadata_value = serde_json::to_value(&metadata)?;
        Self::inject_extra_metadata_fields(&mut metadata_value);

        // SELF-DIAGNOSTIC (ISC-KS-VIS): measure the ACTUALLY-encoded mp4 and
        // inject a `_video_actual` block + warn loudly on mismatch.
        //
        // WHY: every field above (`capture_resolution`, `frame_count`,
        // `average_fps`) is sourced from OBS's CONFIGURED values — what we asked
        // OBS to produce, not what the muxer actually wrote. Real sessions have
        // shipped a metadata.json claiming 1920x1080 @ ~30fps while the decoded
        // mp4 was 960x544 @ 24fps. This block measures the finished file (which
        // was already fsync'd in `recording.rs` before we got here) so the
        // session carries the measured TRUTH next to the claim. It is PURELY
        // ADDITIVE and DIAGNOSTIC: it never touches capture/encode/muxer/fps/
        // resolution, and a measurement failure simply omits the block (the
        // recording is still finalized normally).
        let claimed_fps = constants::FPS as f64;
        Self::inject_video_actual(
            &mut metadata_value,
            &recording_location,
            capture_resolution.0,
            capture_resolution.1,
            claimed_fps,
        )
        .await;

        // Surface the encoder that ACTUALLY encoded this recording as a
        // top-level convenience field so the session is self-describing for the
        // buyer (PRD: "encoder actually used") without parsing the nested
        // `recorder_extra` blob. The embedded OBS recorder already records the
        // actually-constructed encoder id (including any runtime fallback) under
        // `recorder_extra.encoder`; we promote it verbatim. This is purely
        // additive and only set when genuinely known — we never fabricate it
        // (the socket recorder returns no settings, so the field is simply
        // absent there rather than guessed).
        Self::inject_encoder_used(&mut metadata_value);

        // Additive clock-anomaly diagnostic (see guard above). Only emitted when
        // an anomaly was detected, so healthy recordings are unchanged.
        if let Some(anomaly) = clock_anomaly {
            if let Some(obj) = metadata_value.as_object_mut() {
                match serde_json::to_value(&anomaly) {
                    Ok(v) => {
                        obj.insert("clock_anomaly".to_string(), v);
                    }
                    Err(e) => tracing::warn!("Failed to serialize clock_anomaly: {e}"),
                }
            }
        }

        let metadata_json = serde_json::to_string_pretty(&metadata_value)?;
        durable_write::write_atomic_async(&metadata_path, metadata_json.into_bytes()).await?;

        // Validate the recording immediately after stopping to create [`constants::filename::recording::INVALID`] file if needed
        tracing::info!("Validating recording at {}", recording_location.display());
        tokio::task::spawn_blocking(move || {
            if let Err(e) = crate::validation::validate_folder(&recording_location) {
                tracing::error!("Error validating recording on stop: {e}");
            }
        })
        .await
        .ok();

        Ok(())
    }

    /// Inject metadata fields that can't be expressed on the shared
    /// `output_types::Metadata` struct (owned by another module) directly into
    /// the serialized JSON object as top-level keys.
    ///
    /// Adds:
    ///   - `camera_intrinsics`: nested pinhole-intrinsics object derived from
    ///     `constants::RECORDING_WIDTH`/`HEIGHT` and `MC_DEFAULT_VFOV_DEG`. Its
    ///     `source` is `"assumed_mc_default"` — the FOV is assumed, not
    ///     measured, and the field says so to keep the data honest.
    ///   - `timezone_utc_offset_seconds`: the machine's current local UTC
    ///     offset in seconds (e.g. `-25200` for UTC-7). This is the offset that
    ///     applies to the LOCAL-time session directory name; the `wall_clock_*`
    ///     fields remain UTC. A reader adds this offset to the UTC wall-clock to
    ///     recover the local time embedded in the directory name.
    ///   - `session_dir_timezone`: `"local"`, stating explicitly that the
    ///     `session_YYYYMMDD_HHMMSS_*` directory name is in local time, NOT UTC.
    ///
    /// If the serialized metadata is somehow not a JSON object (it always is —
    /// `Metadata` is a struct), this is a no-op so we never panic on the
    /// finalize path.
    fn inject_extra_metadata_fields(metadata_value: &mut serde_json::Value) {
        let Some(obj) = metadata_value.as_object_mut() else {
            tracing::warn!("Metadata did not serialize to a JSON object; skipping extra fields");
            return;
        };

        // Camera intrinsics from the encoded resolution + assumed MC FOV.
        let intrinsics = CameraIntrinsics::from_resolution_and_vfov(
            constants::RECORDING_WIDTH,
            constants::RECORDING_HEIGHT,
            constants::MC_DEFAULT_VFOV_DEG,
            "assumed_mc_default",
        );
        match serde_json::to_value(intrinsics) {
            Ok(v) => {
                obj.insert("camera_intrinsics".to_string(), v);
            }
            Err(e) => {
                // Shouldn't happen for a plain struct of f64/u32/&str, but stay
                // non-fatal: a missing intrinsics block is better than failing
                // to write metadata at all.
                tracing::warn!("Failed to serialize camera_intrinsics: {e}");
            }
        }

        // Timezone disambiguation. `Local::now().offset()` yields the machine's
        // current `FixedOffset`; `local_minus_utc()` is the offset in seconds
        // (local = utc + offset). Captured at finalize time — close enough to
        // the recording window that DST transitions mid-session are a
        // negligible edge case for this disambiguation aid.
        let utc_offset_seconds = chrono::Local::now().offset().local_minus_utc();
        obj.insert(
            "timezone_utc_offset_seconds".to_string(),
            serde_json::Value::from(utc_offset_seconds),
        );
        obj.insert(
            "session_dir_timezone".to_string(),
            serde_json::Value::from("local"),
        );
    }

    /// Promote the encoder that ACTUALLY encoded this recording to a top-level
    /// `encoder_used` string, copied verbatim from `recorder_extra.encoder` when
    /// present.
    ///
    /// WHY a top-level mirror: the buyer PRD wants each session self-describing
    /// ("encoder actually used") without reaching into the recorder-specific
    /// `recorder_extra` blob, whose shape varies per backend. The embedded OBS
    /// recorder writes the actually-constructed encoder id there (including any
    /// runtime fallback to a different encoder), so this is the truthful value.
    ///
    /// HONESTY: we ONLY set the field when `recorder_extra.encoder` is a real
    /// string. The socket recorder returns no settings (`recorder_extra` is
    /// `null`), so the field is simply ABSENT for those sessions rather than a
    /// fabricated guess — same data-integrity rule as `camera_intrinsics`'
    /// `source` tag. Purely additive: never overwrites an existing key.
    fn inject_encoder_used(metadata_value: &mut serde_json::Value) {
        let Some(obj) = metadata_value.as_object_mut() else {
            return;
        };
        if obj.contains_key("encoder_used") {
            return;
        }
        if let Some(encoder) = obj
            .get("recorder_extra")
            .and_then(|extra| extra.get("encoder"))
            .and_then(|enc| enc.as_str())
            .map(str::to_owned)
        {
            obj.insert("encoder_used".to_string(), serde_json::Value::from(encoder));
        }
    }

    /// Measure the encoded `recording.mp4` and inject a `_video_actual` block
    /// describing the REAL file, warning loudly when it disagrees with the
    /// claimed values.
    ///
    /// This is the cure for the recurring "metadata.json lies about the video"
    /// class of bug (see `mp4_probe.rs` for the full rationale). It is purely
    /// additive and read-only with respect to the recording: it measures the
    /// already-finalized mp4 (resolution / frame_count / duration / fps) and
    /// writes the measured truth into a NEW top-level key, leaving every claimed
    /// field untouched.
    ///
    /// The `_video_actual` object matches `mp4_probe::VideoActual`:
    /// `{ width, height, fps, frame_count, duration_s, source, matches_claim }`
    /// where `source` is `"ffprobe"` or `"mp4_moov_parse"` and `matches_claim`
    /// is true iff width/height match EXACTLY and fps is within ±1.0.
    ///
    /// Failure handling: if the mp4 is missing or unmeasurable we log at warn
    /// and SKIP the block — diagnostics must never block finalizing a recording.
    /// The measurement is blocking file I/O, so it runs on `spawn_blocking` to
    /// keep the tokio reactor free (mirrors the fsync/validate calls in
    /// `recording.rs`).
    async fn inject_video_actual(
        metadata_value: &mut serde_json::Value,
        recording_location: &Path,
        claimed_width: u32,
        claimed_height: u32,
        claimed_fps: f64,
    ) {
        let Some(obj) = metadata_value.as_object_mut() else {
            return;
        };

        let mp4_path = recording_location.join(constants::filename::recording::VIDEO);
        if !mp4_path.exists() {
            // No mp4 to measure (rare encoder-failure path). The validator will
            // flag the missing video; we simply emit no `_video_actual`.
            tracing::warn!(
                "Cannot self-measure encoded video: {} does not exist; \
                 omitting _video_actual block",
                mp4_path.display()
            );
            return;
        }

        let measured = tokio::task::spawn_blocking(move || {
            crate::record::mp4_probe::measure_encoded_video(
                &mp4_path,
                claimed_width,
                claimed_height,
                claimed_fps,
            )
        })
        .await;

        let actual = match measured {
            Ok(Ok(actual)) => actual,
            Ok(Err(e)) => {
                tracing::warn!(
                    "Failed to self-measure encoded video, omitting _video_actual block: {e}"
                );
                return;
            }
            Err(join_err) => {
                tracing::warn!(
                    "Self-measure task panicked, omitting _video_actual block: {join_err}"
                );
                return;
            }
        };

        if !actual.matches_claim {
            // LOUD mismatch warning — the whole point of this instrumentation.
            // A future debug log reads this line to know, definitively, that the
            // metadata's claimed resolution/fps are NOT what was encoded.
            tracing::warn!(
                "ENCODED VIDEO MISMATCH: claimed {cw}x{ch}@{cfps:.1}, actual {aw}x{ah}@{afps:.1} \
                 ({frames} frames) — metadata claimed values are NOT what was encoded \
                 (measured via {source})",
                cw = claimed_width,
                ch = claimed_height,
                cfps = claimed_fps,
                aw = actual.width,
                ah = actual.height,
                afps = actual.fps,
                frames = actual.frame_count,
                source = actual.source.as_str(),
            );
        } else {
            tracing::info!(
                "Encoded video self-check OK: {w}x{h}@{fps:.2} ({frames} frames, {dur:.2}s) \
                 matches claim (measured via {source})",
                w = actual.width,
                h = actual.height,
                fps = actual.fps,
                frames = actual.frame_count,
                dur = actual.duration_s,
                source = actual.source.as_str(),
            );
        }

        match serde_json::to_value(&actual) {
            Ok(v) => {
                obj.insert("_video_actual".to_string(), v);
            }
            Err(e) => {
                tracing::warn!("Failed to serialize _video_actual: {e}");
            }
        }
    }
}

/// Calculate the total size of all files in a folder (recursively).
/// Excludes .tar files as they are temporary upload artifacts.
fn folder_size(path: &Path) -> Result<u64, std::io::Error> {
    let mut size = 0;
    let mut dirs_to_visit = vec![path.to_path_buf()];
    while let Some(dir) = dirs_to_visit.pop() {
        let entries = match dir.read_dir() {
            Ok(e) => e,
            Err(_) => continue, // Skip unreadable directories
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                dirs_to_visit.push(entry_path);
            } else if entry_path.is_file() && entry_path.extension().unwrap_or_default() != "tar" {
                size += entry_path.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    Ok(size)
}

#[cfg(test)]
mod durability_tests {
    //! Durability tests for the session-metadata finalize path.
    //!
    //! We can't easily exercise the full
    //! [`LocalRecording::write_metadata_and_validate`] from a unit test —
    //! it pulls in OBS adapter info, input-capture gamepads, and git
    //! version strings that aren't available in a `cargo test` context.
    //! What we CAN test is the underlying invariant the bugfix relies on:
    //! when we commit a metadata.json via the same `durable_write` helper
    //! the finalize path now uses, the on-disk result satisfies:
    //!   (a) the final file exists,
    //!   (b) no `.tmp` sibling remains,
    //!   (c) the content is valid JSON matching what we wrote.
    //!
    //! This is the pre-bugfix failure mode we were seeing: a crash between
    //! `tokio::fs::write(tmp)` and `tokio::fs::rename(tmp, final)` could
    //! leave either (a) missing or (b) present with a truncated payload.

    use crate::util::durable_write;
    use tempfile::TempDir;

    #[test]
    fn finalize_metadata_write_produces_valid_json_without_tmp_leftover() {
        let session_dir = TempDir::new().expect("tempdir for fake session");
        let metadata_path = session_dir
            .path()
            .join(constants::filename::recording::METADATA);

        // Build a fake metadata blob shaped like the real
        // `output_types::Metadata`. We don't import the full struct here —
        // the point of this test is the file-system invariants, not the
        // schema. Any valid JSON is sufficient.
        let fake_metadata = serde_json::json!({
            "game_exe": "eldenring.exe",
            "session_id": "durability-test-0001",
            "duration": 123.456,
            "frame_count": 7400,
        });
        let json = serde_json::to_string_pretty(&fake_metadata).expect("serialize");

        // Commit via the same helper the finalize path uses.
        durable_write::write_atomic(&metadata_path, json.as_bytes())
            .expect("atomic write should succeed on a healthy tempdir");

        // (a) Final file exists.
        assert!(
            metadata_path.exists(),
            "metadata.json must exist after finalize"
        );

        // (b) No `.tmp` sibling was left behind. Post-R5.6 the tempfile
        // name carries a random suffix (`metadata.json.tmp.<rand>`), so
        // the legacy `<path>.tmp` path is never even created — but we
        // also scan the directory for ANY file whose name starts with the
        // tempfile prefix to catch a partial cleanup.
        let tmp_sibling = session_dir
            .path()
            .join(format!("{}.tmp", constants::filename::recording::METADATA));
        assert!(
            !tmp_sibling.exists(),
            "metadata.json.tmp must not remain after successful rename"
        );
        let orphan_tmps: Vec<_> = std::fs::read_dir(session_dir.path())
            .expect("read session dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            orphan_tmps.is_empty(),
            "R5.6: no `<path>.tmp.*` tempfiles must leak from a successful write, got: {orphan_tmps:?}"
        );

        // (c) Content round-trips through JSON.
        let read_back = std::fs::read_to_string(&metadata_path).expect("read metadata.json");
        let parsed: serde_json::Value =
            serde_json::from_str(&read_back).expect("metadata.json must parse as JSON");
        assert_eq!(parsed["game_exe"], "eldenring.exe");
        assert_eq!(parsed["frame_count"], 7400);
    }

    #[test]
    fn atomic_overwrite_does_not_merge_with_previous_contents() {
        // Regression guard for the torn-write hazard the fix is designed
        // to defeat: after atomic write, the reader sees EITHER the old
        // or the NEW complete contents — never a mix.
        let session_dir = TempDir::new().unwrap();
        let p = session_dir
            .path()
            .join(constants::filename::recording::METADATA);
        std::fs::write(
            &p,
            r#"{"schema":"v1","note":"this is the OLD, intentionally longer"}"#,
        )
        .unwrap();

        let new = r#"{"schema":"v2"}"#;
        durable_write::write_atomic(&p, new.as_bytes()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            new,
            "atomic write must fully replace the file, not merge"
        );
    }
}
