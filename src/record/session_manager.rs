//! Session Manager for LEM Format
//!
//! Manages the session directory structure and provides utilities for
//! timestamp conversion and frame indexing.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::SystemTime,
};

use chrono::{Datelike, Timelike, Utc};
use color_eyre::{Result, eyre::eyre};

use crate::{output_types::lem_metadata::SessionMetadata, util::durable_write};

/// Manages a recording session in LEM format
pub struct SessionManager {
    session_id: String,
    session_path: PathBuf,
    start_time: SystemTime,
    start_ns: u64,
    frame_counter: Arc<AtomicU64>,
}

impl SessionManager {
    /// Create a new session with LEM directory structure
    pub async fn create(base_path: &Path, game_name: &str) -> Result<Self> {
        let session_id = generate_session_id();
        let session_path = base_path.join(&session_id);

        // Create directory structure
        Self::create_directory_structure(&session_path).await?;

        let start_time = SystemTime::now();
        let start_ns = system_time_to_ns(start_time);

        let manager = Self {
            session_id,
            session_path,
            start_time,
            start_ns,
            frame_counter: Arc::new(AtomicU64::new(0)),
        };

        // Write initial session metadata
        let metadata = SessionMetadata::new(
            manager.session_id.clone(),
            game_name.to_string(),
            "unknown".to_string(),
        );
        manager.write_session_metadata(&metadata).await?;

        tracing::info!(
            session_id = %manager.session_id,
            path = %manager.session_path.display(),
            "Created new LEM session"
        );

        Ok(manager)
    }

    /// Create all necessary directories for LEM format
    async fn create_directory_structure(session_path: &Path) -> Result<()> {
        let dirs = [
            "recordings",
            "streams",
            "extracted/rgb",
            "extracted/depth",
            "metadata",
            "checksums",
        ];

        for dir in &dirs {
            let path = session_path.join(dir);
            tokio::fs::create_dir_all(&path)
                .await
                .map_err(|e| eyre!("Failed to create directory {}: {}", path.display(), e))?;
        }

        Ok(())
    }

    /// Get the current frame index
    pub fn current_frame(&self) -> u64 {
        self.frame_counter.load(Ordering::SeqCst)
    }

    /// Increment frame counter and return the new frame index
    pub fn increment_frame(&self) -> u64 {
        self.frame_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Get the next frame index without incrementing
    pub fn next_frame(&self) -> u64 {
        self.frame_counter.load(Ordering::SeqCst)
    }

    /// Convert SystemTime to nanoseconds since Unix epoch
    pub fn system_time_to_ns(&self, time: SystemTime) -> u64 {
        system_time_to_ns(time)
    }

    /// Get current time in nanoseconds
    pub fn now_ns(&self) -> u64 {
        system_time_to_ns(SystemTime::now())
    }

    /// Get elapsed time since session start in nanoseconds
    pub fn elapsed_ns(&self) -> u64 {
        self.now_ns() - self.start_ns
    }

    /// Get session start time in nanoseconds
    pub fn start_ns(&self) -> u64 {
        self.start_ns
    }

    // Directory path getters

    pub fn session_path(&self) -> &Path {
        &self.session_path
    }

    pub fn recordings_dir(&self) -> PathBuf {
        self.session_path.join("recordings")
    }

    pub fn streams_dir(&self) -> PathBuf {
        self.session_path.join("streams")
    }

    pub fn extracted_dir(&self) -> PathBuf {
        self.session_path.join("extracted")
    }

    pub fn extracted_rgb_dir(&self) -> PathBuf {
        self.session_path.join("extracted/rgb")
    }

    pub fn extracted_depth_dir(&self) -> PathBuf {
        self.session_path.join("extracted/depth")
    }

    pub fn metadata_dir(&self) -> PathBuf {
        self.session_path.join("metadata")
    }

    pub fn checksums_dir(&self) -> PathBuf {
        self.session_path.join("checksums")
    }

    // File path getters

    pub fn main_video_path(&self) -> PathBuf {
        self.recordings_dir().join("main_record.mp4")
    }

    pub fn video_metadata_path(&self) -> PathBuf {
        self.recordings_dir().join("main_record.meta.json")
    }

    pub fn depth_video_path(&self) -> PathBuf {
        self.recordings_dir().join("depth_record.avi")
    }

    pub fn actions_path(&self) -> PathBuf {
        self.streams_dir().join("actions.jsonl")
    }

    pub fn states_path(&self) -> PathBuf {
        self.streams_dir().join("states.jsonl")
    }

    pub fn events_path(&self) -> PathBuf {
        self.streams_dir().join("events.jsonl")
    }

    pub fn timestamps_path(&self) -> PathBuf {
        self.streams_dir().join("timestamps.jsonl")
    }

    pub fn session_metadata_path(&self) -> PathBuf {
        self.metadata_dir().join("session.json")
    }

    pub fn hardware_metadata_path(&self) -> PathBuf {
        self.metadata_dir().join("hardware.json")
    }

    pub fn game_metadata_path(&self) -> PathBuf {
        self.metadata_dir().join("game.json")
    }

    pub fn recorder_metadata_path(&self) -> PathBuf {
        self.metadata_dir().join("recorder.json")
    }

    pub fn extraction_log_path(&self) -> PathBuf {
        self.extracted_dir().join("extraction_log.json")
    }

    pub fn recordings_checksum_path(&self) -> PathBuf {
        self.checksums_dir().join("recordings.sha256")
    }

    pub fn streams_checksum_path(&self) -> PathBuf {
        self.checksums_dir().join("streams.sha256")
    }

    pub fn extracted_checksum_path(&self) -> PathBuf {
        self.checksums_dir().join("extracted.sha256")
    }

    /// Write session metadata to file
    pub async fn write_session_metadata(&self, metadata: &SessionMetadata) -> Result<()> {
        let path = self.session_metadata_path();
        let json = serde_json::to_string_pretty(metadata)?;
        // Durable write: atomic rename + fsync on the temp file before rename
        // so an unclean shutdown can't leave a 0-byte session.json under the
        // final name. This is the session "create" marker and also the
        // post-finalize write, so we use the same pattern for both.
        durable_write::write_atomic_async(&path, json.into_bytes())
            .await
            .map_err(|e| {
                eyre!(
                    "Failed to write session metadata to {}: {}",
                    path.display(),
                    e
                )
            })?;
        Ok(())
    }

    /// Get the session ID
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Generate a unique session ID.
///
/// Format: `session_YYYYMMDD_HHMMSS_<8hex>`. The 8-hex suffix is drawn from
/// a UUIDv4 and prevents collisions when two sessions are created within
/// the same 1-second window (rare but possible with restart loops).
fn generate_session_id() -> String {
    let now = Utc::now();
    let uuid = uuid::Uuid::new_v4();
    let suffix: String = uuid.simple().to_string().chars().take(8).collect();
    format!(
        "session_{}{:02}{:02}_{:02}{:02}{:02}_{}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        suffix,
    )
}

/// Convert SystemTime to nanoseconds since Unix epoch
fn system_time_to_ns(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_session_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let manager = SessionManager::create(temp_dir.path(), "TestGame")
            .await
            .unwrap();

        assert!(manager.session_id().starts_with("session_"));
        assert!(manager.recordings_dir().exists());
        assert!(manager.streams_dir().exists());
        assert!(manager.metadata_dir().exists());
        assert!(manager.checksums_dir().exists());
        assert!(manager.session_metadata_path().exists());
    }

    #[tokio::test]
    async fn test_frame_counter() {
        let temp_dir = TempDir::new().unwrap();
        let manager = SessionManager::create(temp_dir.path(), "TestGame")
            .await
            .unwrap();

        assert_eq!(manager.current_frame(), 0);
        assert_eq!(manager.increment_frame(), 0);
        assert_eq!(manager.current_frame(), 1);
    }

    /// Two SessionManagers created back-to-back (within the same second) must
    /// produce distinct session paths. Prior to the nanosecond/random suffix
    /// fix this test would fail ~100% of the time.
    #[tokio::test]
    async fn back_to_back_sessions_have_distinct_paths() {
        let temp_dir = TempDir::new().unwrap();
        let a = SessionManager::create(temp_dir.path(), "TestGame")
            .await
            .unwrap();
        let b = SessionManager::create(temp_dir.path(), "TestGame")
            .await
            .unwrap();

        assert_ne!(
            a.session_path(),
            b.session_path(),
            "two sessions created within the same second must have different paths"
        );
    }

    /// Tight-loop stress: 50 sessions in under a second should all be unique.
    #[test]
    fn generate_session_id_is_unique_in_tight_loop() {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for _ in 0..50 {
            let id = generate_session_id();
            assert!(seen.insert(id.clone()), "duplicate session id: {id}");
        }
    }
}
