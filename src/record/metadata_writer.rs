//! Metadata Writer for LEM Format
//!
//! Writes all metadata files in the metadata/ directory

use std::{collections::HashMap, sync::Arc};

use color_eyre::{Result, eyre::eyre};
use sha2::{Digest, Sha256};
use tokio::{fs, io::AsyncReadExt};

use crate::{
    config::{EncoderSettings, GameConfig},
    output_types::lem_metadata::*,
    record::session_manager::SessionManager,
    system::hardware_specs,
};

/// Writes metadata files for LEM format
pub struct MetadataWriter {
    session_manager: Arc<SessionManager>,
}

impl MetadataWriter {
    /// Create a new metadata writer
    pub fn new(session_manager: Arc<SessionManager>) -> Self {
        Self { session_manager }
    }

    /// Write all metadata files at the start of recording
    pub async fn write_initial_metadata(
        &self,
        game_exe: &str,
        game_config: &GameConfig,
        encoder_settings: &EncoderSettings,
        game_resolution: (u32, u32),
    ) -> Result<()> {
        self.write_hardware_metadata().await?;
        self.write_game_metadata(game_exe, game_config, game_resolution)
            .await?;
        self.write_recorder_metadata(encoder_settings).await?;

        tracing::info!("Wrote initial metadata files");
        Ok(())
    }

    /// Update session metadata after recording completes
    pub async fn finalize_session_metadata(
        &self,
        duration: std::time::Duration,
        total_frames: u64,
        total_actions: u64,
    ) -> Result<()> {
        let path = self.session_manager.session_metadata_path();

        let contents = fs::read_to_string(&path)
            .await
            .map_err(|e| eyre!("Failed to read session metadata: {}", e))?;

        let mut metadata: SessionMetadata = serde_json::from_str(&contents)
            .map_err(|e| eyre!("Failed to parse session metadata: {}", e))?;

        metadata.finalize(duration, total_frames, total_actions);

        let json = serde_json::to_string_pretty(&metadata)?;
        fs::write(&path, json)
            .await
            .map_err(|e| eyre!("Failed to write finalized session metadata: {}", e))?;

        tracing::info!(
            duration_seconds = metadata.duration_seconds,
            total_frames = metadata.total_frames,
            total_actions = metadata.total_actions,
            "Finalized session metadata"
        );

        Ok(())
    }

    /// Write hardware metadata
    async fn write_hardware_metadata(&self) -> Result<()> {
        let specs = hardware_specs::get_hardware_specs();

        let metadata = HardwareMetadata {
            cpu: specs.cpu,
            gpu: specs.gpu,
            ram_gb: specs.ram_gb,
            os: format!("{:?}", specs.os),
            recording_drive: "NVMe SSD".to_string(),
            average_fps: 0.0,
            dropped_frames: 0,
        };

        let path = self.session_manager.hardware_metadata_path();
        let json = serde_json::to_string_pretty(&metadata)?;
        fs::write(&path, json)
            .await
            .map_err(|e| eyre!("Failed to write hardware metadata: {}", e))?;

        Ok(())
    }

    /// Write game metadata
    async fn write_game_metadata(
        &self,
        game_exe: &str,
        game_config: &GameConfig,
        resolution: (u32, u32),
    ) -> Result<()> {
        let mut keybindings = HashMap::new();
        keybindings.insert("forward".to_string(), "W".to_string());
        keybindings.insert("back".to_string(), "S".to_string());
        keybindings.insert("left".to_string(), "A".to_string());
        keybindings.insert("right".to_string(), "D".to_string());
        keybindings.insert("shoot".to_string(), "mouse_left".to_string());

        let metadata = GameMetadata {
            game: game_exe.to_string(),
            version: game_config
                .version
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            graphics_settings: GraphicsSettings {
                resolution: [resolution.0, resolution.1],
                quality: game_config.quality_preset.clone(),
                fov: game_config.fov,
                motion_blur: game_config.motion_blur,
                ray_tracing: game_config.ray_tracing,
            },
            control_settings: ControlSettings {
                mouse_sensitivity: game_config.mouse_sensitivity,
                invert_y: game_config.invert_y,
                keybindings,
            },
        };

        let path = self.session_manager.game_metadata_path();
        let json = serde_json::to_string_pretty(&metadata)?;
        fs::write(&path, json)
            .await
            .map_err(|e| eyre!("Failed to write game metadata: {}", e))?;

        Ok(())
    }

    /// Write recorder metadata
    async fn write_recorder_metadata(&self, settings: &EncoderSettings) -> Result<()> {
        let metadata = RecorderMetadata {
            recorder_version: env!("CARGO_PKG_VERSION").to_string(),
            target_fps: settings.fps,
            video_codec: settings.encoder.clone(),
            video_bitrate_mbps: settings.bitrate_mbps,
            capture_method: "game_capture".to_string(),
            record_audio: settings.record_audio,
            audio_bitrate: settings.audio_bitrate_kbps,
            record_depth: false,
            compress_actions: false,
        };

        let path = self.session_manager.recorder_metadata_path();
        let json = serde_json::to_string_pretty(&metadata)?;
        fs::write(&path, json)
            .await
            .map_err(|e| eyre!("Failed to write recorder metadata: {}", e))?;

        Ok(())
    }

    /// Write video metadata after recording
    pub async fn write_video_metadata(&self, metadata: &VideoMetadata) -> Result<()> {
        let path = self.session_manager.video_metadata_path();
        let json = serde_json::to_string_pretty(metadata)?;
        fs::write(&path, json)
            .await
            .map_err(|e| eyre!("Failed to write video metadata: {}", e))?;

        tracing::info!(
            total_frames = metadata.total_frames,
            file_size_mb = metadata.file_size_bytes / 1_000_000,
            "Wrote video metadata"
        );

        Ok(())
    }

    /// Generate SHA-256 checksums for all files
    pub async fn generate_checksums(&self) -> Result<()> {
        let recordings_checksums = self
            .checksum_directory(&self.session_manager.recordings_dir())
            .await?;
        self.write_checksum_file(
            &self.session_manager.recordings_checksum_path(),
            &recordings_checksums,
        )
        .await?;

        let streams_checksums = self
            .checksum_directory(&self.session_manager.streams_dir())
            .await?;
        self.write_checksum_file(
            &self.session_manager.streams_checksum_path(),
            &streams_checksums,
        )
        .await?;

        tracing::info!("Generated checksums for all directories");
        Ok(())
    }

    /// Calculate checksums for all files in a directory
    async fn checksum_directory(&self, dir: &std::path::Path) -> Result<Vec<ChecksumEntry>> {
        let mut entries = Vec::new();
        let mut read_dir = fs::read_dir(dir).await?;

        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            if path.is_file() {
                let hash = self.calculate_file_hash(&path).await?;
                let relative_path = path
                    .strip_prefix(&self.session_manager.session_path())
                    .map_err(|e| eyre!("Failed to get relative path: {}", e))?
                    .to_string_lossy()
                    .to_string();

                entries.push(ChecksumEntry {
                    file: relative_path,
                    sha256: hash,
                });
            }
        }

        Ok(entries)
    }

    /// Calculate SHA-256 hash of a file
    async fn calculate_file_hash(&self, path: &std::path::Path) -> Result<String> {
        let mut file = fs::File::open(path).await?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];

        loop {
            let bytes_read = file.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        let result = hasher.finalize();
        Ok(format!("{:x}", result))
    }

    /// Write checksum file
    async fn write_checksum_file(
        &self,
        path: &std::path::Path,
        entries: &[ChecksumEntry],
    ) -> Result<()> {
        let mut content = String::new();
        for entry in entries {
            content.push_str(&format!("{}  {}\n", entry.sha256, entry.file));
        }
        fs::write(path, content).await?;
        Ok(())
    }
}
