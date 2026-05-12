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

use crate::output_types::lem_metadata::SessionMetadata;

/// Manages a recording session in LEM format
pub struct SessionManager {
    /// Unique session identifier
    session_id: String,
    /// Root path of the session
    session_path: PathBuf,
    /// Session start time
    start_time: SystemTime,
    /// Start time in nanoseconds
    start_ns: u64,
    /// Frame counter for generating frame indices
    frame_counter: Arc<AtomicU64>,
}

impl SessionManager {
    /// Create a new session with LEM directory structure
    ///
    /// # Arguments
    /// * `base_path` - Base directory where sessions are stored
    /// * `game_name` - Name of the game being recorded
    ///
    /// # Returns
    /// * `Result<Self>` - SessionManager instance or error
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
            "unknown".to_string(), // Will be updated when game version is detected
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
            tokio::fs::create_dir_all(&path).await.map_err(|e| {
                eyre!("Failed to create directory {}: {}", path.display(), e)
            })?;
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
    
    /// Get the session root directory
    pub fn session_path(&self) -> &Path {
        &self.session_path
    }
    
    /// Get the recordings directory
    pub fn recordings_dir(&self) -> PathBuf {
        self.session_path.join("recordings")
    }
    
    /// Get the streams directory
    pub fn streams_dir(&self) -> PathBuf {
        self.session_path.join("streams")
    }
    
    /// Get the extracted directory
    pub fn extracted_dir(&self) -> PathBuf {
        self.session_path.join("extracted")
    }
    
    /// Get the RGB extracted directory
    pub fn extracted_rgb_dir(&self) -> PathBuf {
        self.session_path.join("extracted/rgb")
    }
    
    /// Get the depth extracted directory
    pub fn extracted_depth_dir(&self) -> PathBuf {
        self.session_path.join("extracted/depth")
    }
    
    /// Get the metadata directory
    pub fn metadata_dir(&self) -> PathBuf {
        self.session_path.join("metadata")
    }
    
    /// Get the checksums directory
    pub fn checksums_dir(&self) -> PathBuf {
        self.session_path.join("checksums")
    }
    
    // File path getters
    
    /// Get the main video file path
    pub fn main_video_path(&self) -> PathBuf {
        self.recordings_dir().join("main_record.mp4")
    }
    
    /// Get the video metadata file path
    pub fn video_metadata_path(&self) -> PathBuf {
        self.recordings_dir().join("main_record.meta.json")
    }
    
    /// Get the depth video file path (if enabled)
    pub fn depth_video_path(&self) -> PathBuf {
        self.recordings_dir().join("depth_record.avi")
    }
    
    /// Get the actions stream file path
    pub fn actions_path(&self) -> PathBuf {
        self.streams_dir().join("actions.jsonl")
    }
    
    /// Get the states stream file path
    pub fn states_path(&self) -> PathBuf {
        self.streams_dir().join("states.jsonl")
    }
    
    /// Get the events stream file path
    pub fn events_path(&self) -> PathBuf {
        self.streams_dir().join("events.jsonl")
    }
    
    /// Get the timestamps stream file path
    pub fn timestamps_path(&self) -> PathBuf {
        self.streams_dir().join("timestamps.jsonl")
    }
    
    /// Get the session metadata file path
    pub fn session_metadata_path(&self) -> PathBuf {
        self.metadata_dir().join("session.json")
    }
    
    /// Get the hardware metadata file path
    pub fn hardware_metadata_path(&self) -> PathBuf {
        self.metadata_dir().join("hardware.json")
    }
    
    /// Get the game metadata file path
    pub fn game_metadata_path(&self) -> PathBuf {
        self.metadata_dir().join("game.json")
    }
    
    /// Get the recorder metadata file path
    pub fn recorder_metadata_path(&self) -> PathBuf {
        self.metadata_dir().join("recorder.json")
    }
    
    /// Get the extraction log file path
    pub fn extraction_log_path(&self) -> PathBuf {
        self.extracted_dir().join("extraction_log.json")
    }
    
    /// Get the recordings checksum file path
    pub fn recordings_checksum_path(&self) -> PathBuf {
        self.checksums_dir().join("recordings.sha256")
    }
    
    /// Get the streams checksum file path
    pub fn streams_checksum_path(&self) -> PathBuf {
        self.checksums_dir().join("streams.sha256")
    }
    
    /// Get the extracted checksum file path
    pub fn extracted_checksum_path(&self) -> PathBuf {
        self.checksums_dir().join("extracted.sha256")
    }
    
    /// Write session metadata to file
    pub async fn write_session_metadata(&self, metadata: &SessionMetadata) -> Result<()> {
        let path = self.session_metadata_path();
        let json = serde_json::to_string_pretty(metadata)?;
        tokio::fs::write(&path, json).await.map_err(|e| {
            eyre!("Failed to write session metadata to {}: {}", path.display(), e)
        })?;
        Ok(())
    }
    
    /// Get the session ID
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Generate a unique session ID
/// Format: session_YYYYMMDD_HHMMSS
fn generate_session_id() -> String {
    let now = Utc::now();
    format!(
        "session_{}{:02}{:02}_{:02}{:02}{:02}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// Convert SystemTime to nanoseconds since Unix epoch
fn system_time_to_ns(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Convert nanoseconds to SystemTime
fn ns_to_system_time(ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ns as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    #[tokio::test]
    async fn test_session_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let manager = SessionManager::create(temp_dir.path(), "TestGame").await.unwrap();
        
        // Check session ID format
        assert!(manager.session_id().starts_with("session_"));
        assert_eq!(manager.session_id().len(), 22); // session_YYYYMMDD_HHMMSS
        
        // Check directories exist
        assert!(manager.recordings_dir().exists());
        assert!(manager.streams_dir().exists());
        assert!(manager.metadata_dir().exists());
        assert!(manager.checksums_dir().exists());
        
        // Check session metadata was written
        assert!(manager.session_metadata_path().exists());
    }
    
    #[tokio::test]
    async fn test_frame_counter() {
        let temp_dir = TempDir::new().unwrap();
        let manager = SessionManager::create(temp_dir.path(), "TestGame").await.unwrap();
        
        assert_eq!(manager.current_frame(), 0);
        assert_eq!(manager.increment_frame(), 0);
        assert_eq!(manager.current_frame(), 1);
        assert_eq!(manager.increment_frame(), 1);
        assert_eq!(manager.current_frame(), 2);
    }
    
    #[tokio::test]
    async fn test_path_generation() {
        let temp_dir = TempDir::new().unwrap();
        let manager = SessionManager::create(temp_dir.path(), "TestGame").await.unwrap();
        
        let video_path = manager.main_video_path();
        assert!(video_path.to_string_lossy().contains("recordings/main_record.mp4"));
        
        let actions_path = manager.actions_path();
        assert!(actions_path.to_string_lossy().contains("streams/actions.jsonl"));
    }
    
    #[test]
    fn test_session_id_format() {
        let id = generate_session_id();
        assert!(id.starts_with("session_"));
        assert_eq!(id.len(), 22);
        
        // Verify format with regex-like check
        let parts: Vec<&str> = id.split('_').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "session");
        assert_eq!(parts[1].len(), 15); // YYYYMMDD_HHMMSS
    }
    
    #[test]
    fn test_time_conversion() {
        let now = SystemTime::now();
        let ns = system_time_to_ns(now);
        let back = ns_to_system_time(ns);
        
        // Allow 1ms tolerance for conversion
        let diff = now.duration_since(back).unwrap_or_default();
        assert!(diff.as_millis() < 1);
    }
}