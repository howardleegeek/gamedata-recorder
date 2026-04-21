//! Metadata Writer for LEM Format
//!
//! Writes all metadata files in the metadata/ directory.
//!
//! Audit 2026-04: replaced upstream (OWL Control) hardcoded stubs with real
//! system detection:
//!   - `recording_drive`: was `"NVMe SSD"` always; now `detect_disk_type`
//!     inspects the recording drive via `GetDriveTypeW` +
//!     `IOCTL_STORAGE_QUERY_PROPERTY` and returns NVMe / SATA SSD / SATA HDD
//!     / USB / Unknown.
//!   - `gpu`: was `"Unknown"`/`"Unknown GPU"`; now comes from the DXGI
//!     adapter list the caller already enumerates (same list the NVENC
//!     detector in `config.rs::detect_nvidia_gpu` uses).
//!   - `cpu`, `ram_gb`, `os`: were pulled from `sysinfo` but dropped the
//!     per-core and available-memory fields; now emits both with the new
//!     optional `cpu_physical_cores` / `cpu_logical_cores` /
//!     `cpu_frequency_mhz` / `ram_available_gb` fields.
//!   - `fps_actual` / `average_fps`: was a heartbeat-count approximation;
//!     now uses `FrameStats { total_frames, dropped_frames, duration }`
//!     parsed from OBS's `"number of skipped frames due to encoding lag:
//!     X/TOTAL"` log line by `TracingObsLogger`. The effective FPS is
//!     `(total - dropped) / duration`.
//!   - `fov`: was hardcoded `90`; now `None` — the schema field is
//!     `Option<f32>` with skip-serialize-when-None so downstream never sees
//!     a fabricated value.

use std::{sync::Arc, time::Duration};

use color_eyre::{Result, eyre::eyre};
use egui_wgpu::wgpu;
use sha2::{Digest, Sha256};
use tokio::{fs, io::AsyncReadExt};

use crate::{
    config::EncoderSettings,
    output_types::lem_metadata::*,
    record::session_manager::SessionManager,
    system::{
        disk_type::detect_disk_type,
        hardware_specs::{self, GpuSpecs},
    },
    util::durable_write,
};
use constants::encoding::VideoEncoderType;

/// Real-frame stats parsed from OBS's skipped-frames log line.
///
/// Supplied to the metadata writer after `stop_recording_phase2` has folded
/// the `SkippedFrames { skipped, total }` blob into the recorder's settings
/// Value. Populated from the authoritative OBS counter, not a heartbeat
/// approximation.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// Total frames emitted by the encoder (`TOTAL` in `X/TOTAL` log).
    pub total_frames: u64,
    /// Frames dropped due to encoder lag (`X` in `X/TOTAL`).
    pub dropped_frames: u64,
    /// Wall-clock recording duration.
    pub duration: Duration,
}

impl FrameStats {
    /// Effective FPS: delivered frames over elapsed wall clock.
    ///
    /// This mirrors the semantics of `fps_effective` in `local_recording.rs`:
    /// if the encoder dropped frames, `actual_fps < target_fps` by the drop
    /// ratio. Returns 0.0 for zero-duration recordings rather than NaN so
    /// downstream JSON stays valid (NaN isn't valid JSON).
    pub fn actual_fps(&self) -> f64 {
        let secs = self.duration.as_secs_f64();
        if secs <= 0.0 || self.total_frames == 0 {
            return 0.0;
        }
        let delivered = self.total_frames.saturating_sub(self.dropped_frames);
        delivered as f64 / secs
    }
}

/// DXGI-derived GPU info captured at recording start.
///
/// The active metadata path in `local_recording.rs` already receives
/// `&[wgpu::AdapterInfo]` from the recorder; we reuse the same source here
/// so the LEM writer sees the same vendor/name/VRAM as the legacy writer.
#[derive(Debug, Clone, Default)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: String,
    pub vram_mb: Option<u64>,
}

impl GpuInfo {
    /// Pick the primary GPU from a DXGI adapter list.
    ///
    /// Heuristic: prefer a discrete NVIDIA/AMD adapter over an integrated
    /// one, since that's what OBS will actually use for encoding. If no
    /// discrete adapter is present (iGPU-only systems), fall back to the
    /// first enumerated adapter.
    pub fn from_adapters(adapters: &[wgpu::AdapterInfo]) -> Option<Self> {
        if adapters.is_empty() {
            return None;
        }
        // PCI vendor IDs for discrete-GPU vendors that OBS prefers for NVENC/AMF.
        const NVIDIA: u32 = 0x10DE;
        const AMD: u32 = 0x1002;

        let primary = adapters
            .iter()
            .find(|a| a.vendor == NVIDIA || a.vendor == AMD)
            .unwrap_or(&adapters[0]);

        Some(Self {
            name: primary.name.clone(),
            vendor: GpuSpecs::from_name(&primary.name).vendor,
            // F15 (2026-04-20): we'd ideally query DXGI's
            // `IDXGIAdapter3::QueryVideoMemoryInfo` or at minimum
            // `IDXGIAdapter::GetDesc`.`DedicatedVideoMemory` here, but the
            // main workspace `Cargo.toml` does not enable the
            // `Win32_Graphics_Dxgi` / `Win32_Graphics_Dxgi_Common` features
            // on the `windows` crate (check with `grep Win32_Graphics_Dxgi
            // Cargo.toml`). The audit constraint forbids adding deps, so
            // we fall back to `None` and let the serializer skip the
            // field. `Option<u64>` is schema-forward-compatible: a later
            // PR that opts into the DXGI features can start populating
            // this without touching the LEM schema or downstream readers.
            vram_mb: None,
        })
    }
}

/// Writes metadata files for LEM format
pub struct MetadataWriter {
    session_manager: Arc<SessionManager>,
}

impl MetadataWriter {
    /// Create a new metadata writer
    pub fn new(session_manager: Arc<SessionManager>) -> Self {
        Self { session_manager }
    }

    /// Write all metadata files at the start of recording.
    ///
    /// `gpu_info` must come from the caller's DXGI adapter enumeration —
    /// same list the OBS recorder uses to pick its NVENC/AMF/QSV encoder.
    /// Passing `None` writes `"Unknown"` for the GPU (only happens on
    /// systems where DX12 adapter enumeration itself failed).
    pub async fn write_initial_metadata(
        &self,
        game_exe: &str,
        encoder_settings: &EncoderSettings,
        game_resolution: (u32, u32),
        gpu_info: Option<GpuInfo>,
    ) -> Result<()> {
        // Initial write has no frame stats yet — finalized via
        // `update_hardware_metadata_with_fps` at recording stop.
        self.write_hardware_metadata(gpu_info, None).await?;
        self.write_game_metadata(game_exe, game_resolution).await?;
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

        // Atomic + fsync so the finalized session metadata survives unclean
        // shutdown. This is the final metadata write in the LEM pipeline — if
        // it is torn, the session on disk ends up marked "complete" with
        // nonsense duration / frame counts.
        let json = serde_json::to_string_pretty(&metadata)?;
        durable_write::write_atomic_async(&path, json.into_bytes())
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

    /// Update hardware.json with real FPS stats once the encoder has emitted
    /// its skipped-frames line. Called from the Phase-2 stop path after
    /// `TracingObsLogger` has captured the `X/TOTAL` counter.
    ///
    /// This is an in-place update: we read hardware.json, overwrite the FPS
    /// fields, and write back atomically. Keeping it as a second write lets
    /// `write_initial_metadata` stay non-blocking at start and means partial
    /// recordings (app crash before stop) still have valid hardware.json
    /// minus the FPS line.
    pub async fn update_hardware_metadata_with_fps(
        &self,
        gpu_info: Option<GpuInfo>,
        frame_stats: FrameStats,
    ) -> Result<()> {
        // Re-derive everything and re-write rather than doing a JSON merge —
        // total write size is <1 KB, the atomicity is simpler, and we don't
        // have to worry about drifting between initial and final
        // representations.
        self.write_hardware_metadata(gpu_info, Some(frame_stats))
            .await
    }

    /// Write hardware metadata.
    ///
    /// Real detection fields replace the previous upstream stubs:
    ///   - CPU: `sysinfo` (name, physical cores, logical cores, MHz)
    ///   - RAM: `sysinfo` (total + available)
    ///   - OS: `sysinfo` (name + version)
    ///   - Disk: `detect_disk_type` (IOCTL_STORAGE_QUERY_PROPERTY)
    ///   - GPU: caller-supplied DXGI info (name, vendor, VRAM)
    ///   - FPS: caller-supplied `FrameStats` from OBS's skipped-frames log
    async fn write_hardware_metadata(
        &self,
        gpu_info: Option<GpuInfo>,
        frame_stats: Option<FrameStats>,
    ) -> Result<()> {
        // Build a GpuSpecs vec from the caller-supplied DXGI info. When the
        // DXGI enumeration itself failed (empty list), we emit a single
        // "Unknown" placeholder so downstream code always sees at least one
        // entry — matching the old upstream behavior but honestly labeled.
        let gpu_specs_list = match &gpu_info {
            Some(info) => vec![GpuSpecs {
                name: info.name.clone(),
                vendor: info.vendor.clone(),
            }],
            None => vec![GpuSpecs {
                name: "Unknown".to_string(),
                vendor: "Unknown".to_string(),
            }],
        };

        let specs = hardware_specs::get_hardware_specs(gpu_specs_list)?;

        // Available RAM — sysinfo::System exposes `available_memory()` but
        // our helper `hardware_specs::get_hardware_specs` only returns
        // `total_memory_gb`. Fetch it inline here so one extra sysinfo
        // refresh gives us both.
        let available_ram_gb = current_available_ram_gb();

        // Physical core count — sysinfo distinguishes physical cores from
        // logical (SMT/HT) threads. The brand string (e.g. "13th Gen
        // Intel(R) Core(TM) i7-13700K") doesn't reliably include this
        // either way, so we compute it directly.
        let physical_cores = current_physical_core_count();

        // Recording drive type. Per spec: call `detect_disk_type` on the
        // recording-location drive and use the string form so the wire
        // field stays compatible with legacy "NVMe SSD"/"SATA SSD"/... values.
        let disk_type = detect_disk_type(self.session_manager.session_path());
        let recording_drive = disk_type.to_string();

        // FPS accounting. When called from the initial-write path we don't
        // have stats yet — emit zeros to match the legacy wire shape; the
        // update path overwrites with real numbers.
        let (average_fps, dropped_frames, total_frames) = match frame_stats {
            Some(stats) => (
                stats.actual_fps(),
                u32::try_from(stats.dropped_frames).unwrap_or(u32::MAX),
                Some(stats.total_frames),
            ),
            None => (0.0, 0, None),
        };

        let metadata = HardwareMetadata {
            cpu: specs.cpu.brand.clone(),
            gpu: specs
                .gpus
                .first()
                .map(|g| g.name.clone())
                .unwrap_or_else(|| "Unknown".to_string()),
            ram_gb: specs.system.total_memory_gb as u32,
            os: format!("{} {}", specs.system.os_name, specs.system.os_version),
            recording_drive,
            average_fps,
            dropped_frames,
            cpu_physical_cores: physical_cores,
            cpu_logical_cores: Some(specs.cpu.cores as u32),
            cpu_frequency_mhz: if specs.cpu.frequency_mhz > 0 {
                Some(specs.cpu.frequency_mhz)
            } else {
                None
            },
            ram_available_gb: available_ram_gb,
            gpu_vendor: gpu_info.as_ref().map(|g| g.vendor.clone()),
            gpu_vram_mb: gpu_info.as_ref().and_then(|g| g.vram_mb),
            total_frames,
        };

        let path = self.session_manager.hardware_metadata_path();
        let json = serde_json::to_string_pretty(&metadata)?;
        durable_write::write_atomic_async(&path, json.into_bytes())
            .await
            .map_err(|e| eyre!("Failed to write hardware metadata: {}", e))?;

        Ok(())
    }

    /// Write game metadata.
    ///
    /// F15 fix (2026-04-20): we no longer emit hardcoded stubs for
    /// `quality`, `mouse_sensitivity`, `invert_y`, or `keybindings`. The
    /// previous values (`"medium"`, `1.0`, `false`, a WASD dict) were
    /// fabricated — the recorder has no generic way to read a game's
    /// graphics preset, mouse sensitivity, Y-axis inversion, or keybind
    /// map. Downstream AI-training consumers were treating those stubs as
    /// ground truth, poisoning the training set.
    ///
    /// The schema changed the four fields to `Option<T>` with
    /// `skip_serializing_if = "Option::is_none"`, so emitting `None` here
    /// causes the JSON to omit the field entirely — downstream reads
    /// "unknown" rather than a plausible-looking lie. `motion_blur` and
    /// `ray_tracing` are left as bool for now (they're also questionable,
    /// but outside this fix's scope and not flagged as poison).
    async fn write_game_metadata(&self, game_exe: &str, resolution: (u32, u32)) -> Result<()> {
        let metadata = GameMetadata {
            game: game_exe.to_string(),
            version: "unknown".to_string(),
            graphics_settings: GraphicsSettings {
                resolution: [resolution.0, resolution.1],
                // Unknown preset — skipped during serialization (F15).
                quality: None,
                // FOV detection requires per-game process-memory inspection
                // which we don't have. Emit None rather than fabricating 90.
                fov: None,
                motion_blur: false,
                ray_tracing: false,
            },
            control_settings: ControlSettings {
                // Unknown — skipped during serialization (F15).
                mouse_sensitivity: None,
                invert_y: None,
                keybindings: None,
            },
        };

        let path = self.session_manager.game_metadata_path();
        let json = serde_json::to_string_pretty(&metadata)?;
        durable_write::write_atomic_async(&path, json.into_bytes())
            .await
            .map_err(|e| eyre!("Failed to write game metadata: {}", e))?;

        Ok(())
    }

    /// Write recorder metadata
    async fn write_recorder_metadata(&self, settings: &EncoderSettings) -> Result<()> {
        let metadata = RecorderMetadata {
            recorder_version: env!("CARGO_PKG_VERSION").to_string(),
            target_fps: 60,
            video_codec: match settings.encoder {
                VideoEncoderType::X264 => "h264".to_string(),
                VideoEncoderType::NvEnc => "h264_nvenc".to_string(),
                VideoEncoderType::NvEncHevc => "hevc_nvenc".to_string(),
                VideoEncoderType::Amf => "h264_amf".to_string(),
                VideoEncoderType::AmfHevc => "hevc_amf".to_string(),
                VideoEncoderType::Qsv => "h264_qsv".to_string(),
                VideoEncoderType::QsvHevc => "hevc_qsv".to_string(),
            },
            video_bitrate_mbps: 10,
            capture_method: "game_capture".to_string(),
            record_audio: false,
            audio_bitrate: 128,
            record_depth: false,
            compress_actions: false,
        };

        let path = self.session_manager.recorder_metadata_path();
        let json = serde_json::to_string_pretty(&metadata)?;
        durable_write::write_atomic_async(&path, json.into_bytes())
            .await
            .map_err(|e| eyre!("Failed to write recorder metadata: {}", e))?;

        Ok(())
    }

    /// Write video metadata after recording
    pub async fn write_video_metadata(&self, metadata: &VideoMetadata) -> Result<()> {
        let path = self.session_manager.video_metadata_path();
        let json = serde_json::to_string_pretty(metadata)?;
        durable_write::write_atomic_async(&path, json.into_bytes())
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
        // Checksum files are the last thing written when finalizing a session;
        // a torn checksum file silently disagrees with the data it's supposed
        // to attest to and breaks upload-time validation.
        durable_write::write_atomic_async(path, content.into_bytes()).await?;
        Ok(())
    }
}

/// Available RAM in GB, or None if sysinfo fails.
///
/// Separate from `hardware_specs::get_hardware_specs` because the shared
/// helper only returns `total_memory_gb`. One `System::new_all()` is cheap
/// (~5 ms) and keeps the new `ram_available_gb` field honest — it's
/// measured at recording start, not some stale cached value.
fn current_available_ram_gb() -> Option<f64> {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let bytes = sys.available_memory();
    if bytes == 0 {
        None
    } else {
        Some(bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Physical (non-SMT) core count, or None if sysinfo can't determine it.
///
/// `sysinfo::System::physical_core_count` returns `Option<usize>` — not all
/// platforms can tell us, and Hyper-V / container boundaries sometimes
/// hide topology. Returning None here is more honest than extrapolating
/// from logical count.
///
/// sysinfo 0.32.x makes `physical_core_count` an instance method that
/// requires CPU info refreshed, so we do a targeted refresh rather than
/// the full `new_all()` (which is ~20ms slower on spinning disks).
fn current_physical_core_count() -> Option<u32> {
    use sysinfo::{CpuRefreshKind, RefreshKind, System};
    let sys = System::new_with_specifics(RefreshKind::new().with_cpu(CpuRefreshKind::everything()));
    sys.physical_core_count().map(|n| n as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_stats_actual_fps_basic() {
        // 3580 delivered / 60s = 59.67 effective fps
        let stats = FrameStats {
            total_frames: 3600,
            dropped_frames: 20,
            duration: Duration::from_secs(60),
        };
        let fps = stats.actual_fps();
        assert!(
            (fps - 59.666_666_7).abs() < 0.001,
            "expected ~59.67 fps, got {fps}"
        );
    }

    #[test]
    fn frame_stats_actual_fps_no_drops() {
        // Perfect 60 fps recording
        let stats = FrameStats {
            total_frames: 3600,
            dropped_frames: 0,
            duration: Duration::from_secs(60),
        };
        assert!((stats.actual_fps() - 60.0).abs() < 0.001);
    }

    #[test]
    fn frame_stats_actual_fps_all_dropped() {
        // Encoder totally bottlenecked — 0 delivered frames
        let stats = FrameStats {
            total_frames: 3600,
            dropped_frames: 3600,
            duration: Duration::from_secs(60),
        };
        assert_eq!(stats.actual_fps(), 0.0);
    }

    #[test]
    fn frame_stats_actual_fps_zero_duration_returns_zero_not_nan() {
        // Zero-duration recording must not produce NaN (invalid JSON).
        let stats = FrameStats {
            total_frames: 100,
            dropped_frames: 0,
            duration: Duration::from_secs(0),
        };
        assert_eq!(stats.actual_fps(), 0.0);
    }

    #[test]
    fn frame_stats_actual_fps_empty_recording() {
        let stats = FrameStats::default();
        assert_eq!(stats.actual_fps(), 0.0);
    }

    #[test]
    fn frame_stats_actual_fps_saturates_on_bogus_drop_count() {
        // If OBS ever reports dropped > total (shouldn't happen but be
        // defensive), saturating_sub clamps delivered to 0 rather than
        // underflowing.
        let stats = FrameStats {
            total_frames: 100,
            dropped_frames: 200,
            duration: Duration::from_secs(10),
        };
        assert_eq!(stats.actual_fps(), 0.0);
    }
}
