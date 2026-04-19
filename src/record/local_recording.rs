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
    ) -> Result<()> {
        // Resolve metadata path from recording location
        let metadata_path = recording_location.join(constants::filename::recording::METADATA);

        // Create metadata
        let duration_nanos = start_instant.elapsed().as_nanos();
        let duration = start_instant.elapsed().as_secs_f64();
        let end_system_time = SystemTime::now();

        let start_timestamp = start_time
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or_else(|_| {
                tracing::warn!("Start time before UNIX epoch, using 0");
                0.0
            });
        let end_timestamp = end_system_time
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or_else(|_| {
                tracing::warn!("Current time before UNIX epoch, using 0");
                0.0
            });

        // Effective FPS from frame count and duration — mirrors competitor semantics
        // and will differ from `average_fps` when frames were dropped at the edges.
        let fps_effective = frame_count.and_then(|n| {
            if duration > 0.0 {
                Some(n as f64 / duration)
            } else {
                None
            }
        });

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
            Ok(specs) => Some(specs),
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
        let metadata_json = serde_json::to_string_pretty(&metadata)?;
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

        // (b) No `.tmp` sibling was left behind.
        let tmp_sibling = session_dir
            .path()
            .join(format!("{}.tmp", constants::filename::recording::METADATA));
        assert!(
            !tmp_sibling.exists(),
            "metadata.json.tmp must not remain after successful rename"
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
