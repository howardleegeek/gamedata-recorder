//! LEM (Large Entity Models) Format Metadata Types
//!
//! This module defines all metadata structures for the LEM output format.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// Session metadata for metadata/session.json
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionMetadata {
    /// Unique session identifier
    pub session_id: String,
    /// Session creation time (ISO 8601 format)
    pub created_at: String,
    /// Recording duration in seconds
    pub duration_seconds: u64,
    /// Total number of frames recorded
    pub total_frames: u64,
    /// Total number of action events
    pub total_actions: u64,
    /// Game name
    pub game: String,
    /// Game version
    pub version: String,
    /// Optional notes about the session
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl SessionMetadata {
    /// Create new session metadata
    pub fn new(
        session_id: String,
        game: String,
        version: String,
    ) -> Self {
        Self {
            session_id,
            created_at: chrono::Utc::now().to_rfc3339(),
            duration_seconds: 0,
            total_frames: 0,
            total_actions: 0,
            game,
            version,
            notes: None,
        }
    }
    
    /// Update with final statistics after recording
    pub fn finalize(&mut self, duration: std::time::Duration, total_frames: u64, total_actions: u64) {
        self.duration_seconds = duration.as_secs();
        self.total_frames = total_frames;
        self.total_actions = total_actions;
    }
}

/// Hardware metadata for metadata/hardware.json
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HardwareMetadata {
    /// CPU model
    pub cpu: String,
    /// GPU model
    pub gpu: String,
    /// RAM in GB
    pub ram_gb: u32,
    /// Operating system
    pub os: String,
    /// Recording drive type
    pub recording_drive: String,
    /// Average FPS during recording
    pub average_fps: f64,
    /// Number of dropped frames
    pub dropped_frames: u32,
}

impl HardwareMetadata {
    /// Create from system information
    pub fn from_system_specs(specs: &crate::system::hardware_specs::HardwareSpecs) -> Self {
        Self {
            cpu: specs.cpu.clone(),
            gpu: specs.gpu.clone(),
            ram_gb: specs.ram_gb,
            os: format!("{:?}", specs.os),
            recording_drive: "Unknown".to_string(), // Will be filled by caller
            average_fps: 0.0, // Will be updated during recording
            dropped_frames: 0,
        }
    }
}

/// Graphics settings for game metadata
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GraphicsSettings {
    /// Resolution [width, height]
    pub resolution: [u32; 2],
    /// Quality preset (low, medium, high, ultra)
    pub quality: String,
    /// Field of view
    pub fov: u32,
    /// Motion blur enabled
    pub motion_blur: bool,
    /// Ray tracing enabled
    pub ray_tracing: bool,
}

/// Control settings for game metadata
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ControlSettings {
    /// Mouse sensitivity
    pub mouse_sensitivity: f64,
    /// Invert Y-axis
    pub invert_y: bool,
    /// Key bindings mapping
    pub keybindings: HashMap<String, String>,
}

/// Game metadata for metadata/game.json
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GameMetadata {
    /// Game name
    pub game: String,
    /// Game version
    pub version: String,
    /// Graphics settings
    pub graphics_settings: GraphicsSettings,
    /// Control settings
    pub control_settings: ControlSettings,
}

impl GameMetadata {
    /// Create from game configuration
    pub fn from_config(
        game: String,
        version: String,
        resolution: (u32, u32),
    ) -> Self {
        Self {
            game,
            version,
            graphics_settings: GraphicsSettings {
                resolution: [resolution.0, resolution.1],
                quality: "high".to_string(),
                fov: 90,
                motion_blur: false,
                ray_tracing: false,
            },
            control_settings: ControlSettings {
                mouse_sensitivity: 5.0,
                invert_y: false,
                keybindings: default_keybindings(),
            },
        }
    }
}

fn default_keybindings() -> HashMap<String, String> {
    let mut bindings = HashMap::new();
    bindings.insert("forward".to_string(), "W".to_string());
    bindings.insert("back".to_string(), "S".to_string());
    bindings.insert("left".to_string(), "A".to_string());
    bindings.insert("right".to_string(), "D".to_string());
    bindings.insert("shoot".to_string(), "mouse_left".to_string());
    bindings.insert("aim".to_string(), "mouse_right".to_string());
    bindings.insert("jump".to_string(), "Space".to_string());
    bindings.insert("crouch".to_string(), "Control".to_string());
    bindings.insert("reload".to_string(), "R".to_string());
    bindings
}

/// Recorder metadata for metadata/recorder.json
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RecorderMetadata {
    /// Recorder version
    pub recorder_version: String,
    /// Target FPS
    pub target_fps: u32,
    /// Video codec
    pub video_codec: String,
    /// Video bitrate in Mbps
    pub video_bitrate_mbps: u32,
    /// Capture method
    pub capture_method: String, // display_capture, game_capture, window_capture
    /// Record audio
    pub record_audio: bool,
    /// Audio bitrate in kbps
    pub audio_bitrate: u32,
    /// Record depth video
    pub record_depth: bool,
    /// Compress actions stream
    pub compress_actions: bool,
}

impl RecorderMetadata {
    /// Create from encoder settings
    pub fn from_settings(settings: &crate::config::EncoderSettings) -> Self {
        Self {
            recorder_version: env!("CARGO_PKG_VERSION").to_string(),
            target_fps: settings.fps,
            video_codec: settings.encoder.clone(),
            video_bitrate_mbps: settings.bitrate_mbps,
            capture_method: "game_capture".to_string(),
            record_audio: settings.record_audio,
            audio_bitrate: settings.audio_bitrate_kbps,
            record_depth: false,
            compress_actions: false,
        }
    }
}

/// Keyframe information for video metadata
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct KeyframeInfo {
    /// Frame index
    pub frame_index: u64,
    /// Byte offset in file
    pub byte_offset: u64,
    /// Presentation timestamp in nanoseconds
    pub pts: u64,
}

/// Video metadata for recordings/main_record.meta.json
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VideoMetadata {
    /// Video codec
    pub codec: String,
    /// Codec profile
    pub profile: String,
    /// Bitrate (e.g., "20 Mbps")
    pub bitrate: String,
    /// Frames per second
    pub fps: u32,
    /// Resolution [width, height]
    pub resolution: [u32; 2],
    /// Pixel format
    pub pixel_format: String,
    /// Duration in seconds
    pub duration_seconds: u64,
    /// Total number of frames
    pub total_frames: u64,
    /// File size in bytes
    pub file_size_bytes: u64,
    /// Keyframe information
    pub keyframes: Vec<KeyframeInfo>,
    /// Frame duration in nanoseconds
    pub frame_duration_ns: u64,
    /// Start time in nanoseconds since Unix epoch
    pub start_time_ns: u64,
    /// End time in nanoseconds since Unix epoch
    pub end_time_ns: u64,
}

impl VideoMetadata {
    /// Calculate frame duration from FPS
    pub fn frame_duration_from_fps(fps: u32) -> u64 {
        1_000_000_000 / fps as u64
    }
    
    /// Create from basic parameters
    pub fn new(
        codec: String,
        fps: u32,
        resolution: [u32; 2],
        start_time_ns: u64,
    ) -> Self {
        Self {
            codec,
            profile: "high".to_string(),
            bitrate: "20 Mbps".to_string(),
            fps,
            resolution,
            pixel_format: "yuv420p".to_string(),
            duration_seconds: 0,
            total_frames: 0,
            file_size_bytes: 0,
            keyframes: Vec::new(),
            frame_duration_ns: Self::frame_duration_from_fps(fps),
            start_time_ns,
            end_time_ns: start_time_ns,
        }
    }
    
    /// Finalize after recording completes
    pub fn finalize(&mut self, total_frames: u64, file_size: u64, end_time_ns: u64) {
        self.total_frames = total_frames;
        self.file_size_bytes = file_size;
        self.end_time_ns = end_time_ns;
        self.duration_seconds = (end_time_ns - self.start_time_ns) / 1_000_000_000;
    }
}

/// Extraction log for extracted/extraction_log.json
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExtractionLog {
    /// Extraction timestamp (ISO 8601)
    pub extraction_date: String,
    /// Source video file path
    pub source_video: String,
    /// Extraction parameters
    pub extraction_params: ExtractionParams,
    /// Number of frames extracted
    pub frames_extracted: u64,
    /// Failed frame indices
    pub failed_frames: Vec<u64>,
    /// Total size in GB
    pub total_size_gb: f64,
    /// Extraction time in seconds
    pub extraction_time_seconds: u64,
}

/// Extraction parameters
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExtractionParams {
    /// Sampling mode: "all_frames" or "key_frames"
    pub sampling: String,
    /// Image format: "jpg", "png"
    pub format: String,
    /// JPEG quality (0-100)
    pub quality: u8,
}

/// Checksum entry for checksum files
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChecksumEntry {
    /// File path (relative to session root)
    pub file: String,
    /// SHA-256 hash
    pub sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_session_metadata_serialization() {
        let meta = SessionMetadata::new(
            "session_20260115_143022".to_string(),
            "Cyberpunk2077".to_string(),
            "2.1".to_string(),
        );
        
        let json = serde_json::to_string_pretty(&meta).unwrap();
        assert!(json.contains("session_20260115_143022"));
        assert!(json.contains("Cyberpunk2077"));
    }
    
    #[test]
    fn test_video_metadata_calculation() {
        let mut meta = VideoMetadata::new(
            "h264".to_string(),
            60,
            [1920, 1080],
            1_564_290_958_000_000_000,
        );
        
        assert_eq!(meta.frame_duration_ns, 16_666_667); // ~60fps
        
        meta.finalize(216_000, 5_400_000_000, 1_564_294_558_000_000_000);
        assert_eq!(meta.total_frames, 216_000);
        assert_eq!(meta.duration_seconds, 3600);
    }
    
    #[test]
    fn test_game_metadata_keybindings() {
        let meta = GameMetadata::from_config(
            "TestGame".to_string(),
            "1.0".to_string(),
            (1920, 1080),
        );
        
        assert_eq!(meta.control_settings.keybindings.get("forward"), Some(&"W".to_string()));
        assert_eq!(meta.graphics_settings.resolution, [1920, 1080]);
    }
}