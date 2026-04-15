//! LEM (Large Entity Models) Format Metadata Types
//!
//! This module defines all metadata structures for the LEM output format.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Session metadata for metadata/session.json
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionMetadata {
    pub session_id: String,
    pub created_at: String,
    pub duration_seconds: u64,
    pub total_frames: u64,
    pub total_actions: u64,
    pub game: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl SessionMetadata {
    pub fn new(session_id: String, game: String, version: String) -> Self {
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

    pub fn finalize(
        &mut self,
        duration: std::time::Duration,
        total_frames: u64,
        total_actions: u64,
    ) {
        self.duration_seconds = duration.as_secs();
        self.total_frames = total_frames;
        self.total_actions = total_actions;
    }
}

/// Hardware metadata
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HardwareMetadata {
    pub cpu: String,
    pub gpu: String,
    pub ram_gb: u32,
    pub os: String,
    pub recording_drive: String,
    pub average_fps: f64,
    pub dropped_frames: u32,
}

/// Graphics settings
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GraphicsSettings {
    pub resolution: [u32; 2],
    pub quality: String,
    pub fov: u32,
    pub motion_blur: bool,
    pub ray_tracing: bool,
}

/// Control settings
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ControlSettings {
    pub mouse_sensitivity: f64,
    pub invert_y: bool,
    pub keybindings: HashMap<String, String>,
}

/// Game metadata
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GameMetadata {
    pub game: String,
    pub version: String,
    pub graphics_settings: GraphicsSettings,
    pub control_settings: ControlSettings,
}

/// Recorder metadata
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RecorderMetadata {
    pub recorder_version: String,
    pub target_fps: u32,
    pub video_codec: String,
    pub video_bitrate_mbps: u32,
    pub capture_method: String,
    pub record_audio: bool,
    pub audio_bitrate: u32,
    pub record_depth: bool,
    pub compress_actions: bool,
}

/// Keyframe info
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct KeyframeInfo {
    pub frame_index: u64,
    pub byte_offset: u64,
    pub pts: u64,
}

/// Video metadata
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VideoMetadata {
    pub codec: String,
    pub profile: String,
    pub bitrate: String,
    pub fps: u32,
    pub resolution: [u32; 2],
    pub pixel_format: String,
    pub duration_seconds: u64,
    pub total_frames: u64,
    pub file_size_bytes: u64,
    pub keyframes: Vec<KeyframeInfo>,
    pub frame_duration_ns: u64,
    pub start_time_ns: u64,
    pub end_time_ns: u64,
}

impl VideoMetadata {
    pub fn frame_duration_from_fps(fps: u32) -> u64 {
        1_000_000_000 / fps as u64
    }

    pub fn new(codec: String, fps: u32, resolution: [u32; 2], start_time_ns: u64) -> Self {
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

    pub fn finalize(&mut self, total_frames: u64, file_size: u64, end_time_ns: u64) {
        self.total_frames = total_frames;
        self.file_size_bytes = file_size;
        self.end_time_ns = end_time_ns;
        self.duration_seconds = (end_time_ns - self.start_time_ns) / 1_000_000_000;
    }
}

/// Extraction log
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExtractionLog {
    pub extraction_date: String,
    pub source_video: String,
    pub extraction_params: ExtractionParams,
    pub frames_extracted: u64,
    pub failed_frames: Vec<u64>,
    pub total_size_gb: f64,
    pub extraction_time_seconds: u64,
}

/// Extraction params
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExtractionParams {
    pub sampling: String,
    pub format: String,
    pub quality: u8,
}

/// Checksum entry
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChecksumEntry {
    pub file: String,
    pub sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_metadata() {
        let meta = SessionMetadata::new(
            "session_20260115_143022".to_string(),
            "Cyberpunk2077".to_string(),
            "2.1".to_string(),
        );
        let json = serde_json::to_string_pretty(&meta).unwrap();
        assert!(json.contains("session_20260115_143022"));
    }

    #[test]
    fn test_video_metadata() {
        let mut meta = VideoMetadata::new(
            "h264".to_string(),
            60,
            [1920, 1080],
            1_564_290_958_000_000_000,
        );
        assert_eq!(meta.frame_duration_ns, 16_666_667);
        meta.finalize(216_000, 5_400_000_000, 1_564_294_558_000_000_000);
        assert_eq!(meta.total_frames, 216_000);
    }
}
