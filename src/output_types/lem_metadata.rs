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

/// Hardware metadata.
///
/// Wire-format preservation: `cpu`, `gpu`, `ram_gb`, `os`, `recording_drive`,
/// `average_fps`, `dropped_frames` are load-bearing field names that the
/// backend and analysts key on. Do NOT rename. Added fields are all optional
/// so missing values in historical recordings deserialize cleanly.
///
/// `recording_drive` retains its `String` type (not an enum) so legacy
/// recordings with freeform values like `"NVMe SSD"` / `"SATA SSD"` /
/// `"Unknown"` parse unchanged. New recordings write the exact same legacy
/// strings via `DiskType::as_str()`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HardwareMetadata {
    pub cpu: String,
    pub gpu: String,
    pub ram_gb: u32,
    pub os: String,
    pub recording_drive: String,
    pub average_fps: f64,
    pub dropped_frames: u32,

    // --- Added 2026-04: real detection fields to replace upstream stubs. ---
    // All optional + skip-when-None so historical recordings without these
    // fields (and the non-Windows CI build) deserialize and serialize
    // cleanly with no wire-format change for legacy consumers.
    /// Physical CPU cores (not threads). From `sysinfo::System::physical_core_count`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu_physical_cores: Option<u32>,
    /// Logical CPU cores (threads). From `sysinfo::System::cpus().len()`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu_logical_cores: Option<u32>,
    /// Base CPU frequency in MHz, as reported by the OS at recording start.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu_frequency_mhz: Option<u64>,
    /// Available (not just total) RAM at recording start, in GB.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ram_available_gb: Option<f64>,
    /// GPU vendor ("NVIDIA" / "AMD" / "Intel" / "Unknown"), from DXGI.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gpu_vendor: Option<String>,
    /// GPU VRAM in MB, from DXGI adapter info.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gpu_vram_mb: Option<u64>,
    /// Total frames emitted by OBS (matches `number of skipped frames … X/TOTAL`).
    /// Present when OBS's skipped-frames log line was successfully parsed.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub total_frames: Option<u64>,
}

/// Graphics settings.
///
/// Schema note (2026-04): `fov` changed from hardcoded `u32` (always `90`)
/// to `Option<f32>`. Per-game FOV detection requires process-memory
/// inspection we don't have; the previous `"Unknown"` / `90` defaults were
/// being ignored by downstream analysts anyway. Emitting `None` (skipped
/// during serialization) is more honest than a fabricated number.
///
/// F15 fix (2026-04-20): `quality` became `Option<String>` for the same
/// reason. The recorder has no cross-game way to read the in-game graphics
/// preset (each engine stores this differently and usually inside process
/// memory), so the prior hardcoded `"medium"` was AI-training poison —
/// downstream consumers treated the field as authoritative. `None` is
/// skipped during serialization so legacy readers see a missing field
/// rather than a fabricated preset.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GraphicsSettings {
    pub resolution: [u32; 2],
    /// In-game graphics preset (`"low"`/`"medium"`/`"high"`/engine-specific),
    /// if we can detect it. `None` when the recorder has no way to read the
    /// game's quality setting — the field is skipped during serialization so
    /// downstream sees absence instead of a fabricated value.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub quality: Option<String>,
    /// Field-of-view in degrees, if we can detect it. `None` when the
    /// recorder has no way to read the game's FOV setting — serialization
    /// skips the field entirely so legacy consumers don't see a phony value.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fov: Option<f32>,
    pub motion_blur: bool,
    pub ray_tracing: bool,
}

/// Control settings.
///
/// F15 fix (2026-04-20): `mouse_sensitivity`, `invert_y`, and `keybindings`
/// all became `Option<T>`. The recorder doesn't read game config files or
/// probe process memory, so the prior hardcoded `1.0` / `false` / WASD dict
/// were fabricated stubs. Downstream AI-training consumers trained on those
/// stubs as if they were ground truth. Emitting `None` (skipped during
/// serialization) signals "unknown" without corrupting the training set.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ControlSettings {
    /// Mouse sensitivity multiplier (engine-specific scale), if detected.
    /// `None` when unknown — skipped during serialization.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mouse_sensitivity: Option<f64>,
    /// Whether the game's vertical mouse axis is inverted, if detected.
    /// `None` when unknown — skipped during serialization.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub invert_y: Option<bool>,
    /// Game-specific action -> key mapping (e.g. `forward -> W`), if
    /// detected. `None` when unknown — skipped during serialization so
    /// downstream doesn't train on a fabricated WASD default.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub keybindings: Option<HashMap<String, String>>,
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

    /// Regression: the legacy hardware.json shape (no cpu_physical_cores,
    /// no cpu_logical_cores, no gpu_vendor, etc.) must still deserialize
    /// cleanly. We have lots of recordings in the wild shipped by v2.5.x
    /// that only emit the seven original fields; losing them would break
    /// the backend ingestion pipeline.
    #[test]
    fn hardware_metadata_deserializes_legacy_wire_shape() {
        let legacy = r#"{
            "cpu": "Intel Core i7-13700K",
            "gpu": "NVIDIA GeForce RTX 4090",
            "ram_gb": 32,
            "os": "Windows 11 Pro",
            "recording_drive": "NVMe SSD",
            "average_fps": 59.8,
            "dropped_frames": 12
        }"#;
        let hw: HardwareMetadata = serde_json::from_str(legacy).unwrap();
        assert_eq!(hw.cpu, "Intel Core i7-13700K");
        assert_eq!(hw.recording_drive, "NVMe SSD");
        assert_eq!(hw.cpu_physical_cores, None);
        assert_eq!(hw.ram_available_gb, None);
        assert_eq!(hw.total_frames, None);
    }

    /// Regression: serializing with all optional fields set to None must
    /// skip them, producing the legacy shape byte-for-byte. Catches accidental
    /// removal of `#[serde(skip_serializing_if = "Option::is_none")]`.
    #[test]
    fn hardware_metadata_serializes_without_optional_fields_when_none() {
        let hw = HardwareMetadata {
            cpu: "AMD Ryzen 9 7950X".to_string(),
            gpu: "NVIDIA GeForce RTX 4090".to_string(),
            ram_gb: 64,
            os: "Windows 11".to_string(),
            recording_drive: "NVMe SSD".to_string(),
            average_fps: 60.0,
            dropped_frames: 0,
            cpu_physical_cores: None,
            cpu_logical_cores: None,
            cpu_frequency_mhz: None,
            ram_available_gb: None,
            gpu_vendor: None,
            gpu_vram_mb: None,
            total_frames: None,
        };
        let json = serde_json::to_string(&hw).unwrap();
        assert!(!json.contains("cpu_physical_cores"));
        assert!(!json.contains("cpu_logical_cores"));
        assert!(!json.contains("gpu_vendor"));
        assert!(!json.contains("total_frames"));
    }

    /// Regression: when optional fields are populated, they serialize with
    /// their snake_case field names as declared.
    #[test]
    fn hardware_metadata_serializes_new_fields_when_populated() {
        let hw = HardwareMetadata {
            cpu: "AMD Ryzen 9 7950X".to_string(),
            gpu: "NVIDIA GeForce RTX 4090".to_string(),
            ram_gb: 64,
            os: "Windows 11".to_string(),
            recording_drive: "NVMe SSD".to_string(),
            average_fps: 59.67,
            dropped_frames: 20,
            cpu_physical_cores: Some(16),
            cpu_logical_cores: Some(32),
            cpu_frequency_mhz: Some(4500),
            ram_available_gb: Some(42.3),
            gpu_vendor: Some("NVIDIA".to_string()),
            gpu_vram_mb: Some(24_576),
            total_frames: Some(3600),
        };
        let json = serde_json::to_string(&hw).unwrap();
        assert!(json.contains("\"cpu_physical_cores\":16"));
        assert!(json.contains("\"cpu_logical_cores\":32"));
        assert!(json.contains("\"total_frames\":3600"));
    }

    /// Regression: fov=None serializes with no `fov` field, per the schema
    /// note above. Analysts were ignoring the old `"Unknown"`/`90` stubs —
    /// skipping is the only way to signal "we don't know" in JSON.
    #[test]
    fn graphics_settings_fov_none_is_skipped() {
        let gs = GraphicsSettings {
            resolution: [1920, 1080],
            quality: Some("high".to_string()),
            fov: None,
            motion_blur: false,
            ray_tracing: false,
        };
        let json = serde_json::to_string(&gs).unwrap();
        assert!(
            !json.contains("fov"),
            "fov should be absent when None, got: {json}"
        );
    }

    /// When fov IS detected (future enhancement), it must serialize with
    /// its f32 value.
    #[test]
    fn graphics_settings_fov_some_is_emitted() {
        let gs = GraphicsSettings {
            resolution: [1920, 1080],
            quality: Some("high".to_string()),
            fov: Some(103.5),
            motion_blur: false,
            ray_tracing: false,
        };
        let json = serde_json::to_string(&gs).unwrap();
        assert!(
            json.contains("\"fov\":103.5"),
            "expected fov:103.5, got: {json}"
        );
    }

    /// F15 regression: quality=None must not serialize a `quality` field.
    /// The pre-fix writer emitted `"quality":"medium"` unconditionally,
    /// which downstream trained on as if it were ground truth.
    #[test]
    fn graphics_settings_quality_none_is_skipped() {
        let gs = GraphicsSettings {
            resolution: [1920, 1080],
            quality: None,
            fov: None,
            motion_blur: false,
            ray_tracing: false,
        };
        let json = serde_json::to_string(&gs).unwrap();
        assert!(
            !json.contains("quality"),
            "quality should be absent when None, got: {json}"
        );
    }

    /// F15 regression: mouse_sensitivity=None, invert_y=None, and
    /// keybindings=None must all be omitted from the serialized JSON.
    /// The pre-fix writer hardcoded `1.0` / `false` / a WASD dict, all
    /// fabricated — downstream training pipelines were ingesting them
    /// as ground truth and poisoning the dataset.
    #[test]
    fn control_settings_all_none_fields_are_skipped() {
        let cs = ControlSettings {
            mouse_sensitivity: None,
            invert_y: None,
            keybindings: None,
        };
        let json = serde_json::to_string(&cs).unwrap();
        assert!(
            !json.contains("mouse_sensitivity"),
            "mouse_sensitivity should be absent when None, got: {json}"
        );
        assert!(
            !json.contains("invert_y"),
            "invert_y should be absent when None, got: {json}"
        );
        assert!(
            !json.contains("keybindings"),
            "keybindings should be absent when None, got: {json}"
        );
        // The whole object should serialize as `{}` since every field is
        // None — this is what downstream sees when we have no game-config
        // detection. Explicit check so we catch future additions that
        // sneak poison defaults back in.
        assert_eq!(json, "{}");
    }

    /// F15: when detection lands in the future, the Some(...) branches
    /// must still serialize correctly.
    #[test]
    fn control_settings_some_fields_are_emitted() {
        let mut kb = HashMap::new();
        kb.insert("forward".to_string(), "W".to_string());
        let cs = ControlSettings {
            mouse_sensitivity: Some(2.5),
            invert_y: Some(true),
            keybindings: Some(kb),
        };
        let json = serde_json::to_string(&cs).unwrap();
        assert!(json.contains("\"mouse_sensitivity\":2.5"));
        assert!(json.contains("\"invert_y\":true"));
        assert!(json.contains("\"forward\":\"W\""));
    }
}
