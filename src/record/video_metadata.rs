//! Video Metadata Extractor
//!
//! Extracts metadata from video files including codec info, keyframes, etc.

use std::path::Path;

use color_eyre::{Result, eyre::eyre};

use crate::output_types::lem_metadata::{KeyframeInfo, VideoMetadata};

/// Extracts metadata from video files
pub struct VideoMetadataExtractor;

impl VideoMetadataExtractor {
    /// Extract metadata from a video file
    pub async fn extract(
        video_path: &Path,
        codec: &str,
        fps: u32,
        resolution: [u32; 2],
        start_time_ns: u64,
    ) -> Result<VideoMetadata> {
        let metadata = tokio::fs::metadata(video_path).await?;
        let file_size = metadata.len();

        let keyframes = match Self::extract_keyframes_with_ffprobe(video_path).await {
            Ok(frames) => frames,
            Err(e) => {
                tracing::warn!("Failed to extract keyframes with ffprobe: {}", e);
                Vec::new()
            }
        };

        let mut video_metadata =
            VideoMetadata::new(codec.to_string(), fps, resolution, start_time_ns);

        video_metadata.file_size_bytes = file_size;
        video_metadata.keyframes = keyframes;

        Ok(video_metadata)
    }

    /// Extract keyframe information using ffprobe
    async fn extract_keyframes_with_ffprobe(video_path: &Path) -> Result<Vec<KeyframeInfo>> {
        let output = tokio::process::Command::new("ffprobe")
            .args(&[
                "-print_format",
                "json",
                "-show_frames",
                "-select_streams",
                "v:0",
                "-show_entries",
                "frame=pkt_pts_time,pkt_pos,key_frame",
                &video_path.to_string_lossy(),
            ])
            .output()
            .await
            .map_err(|e| eyre!("Failed to run ffprobe: {}", e))?;

        if !output.status.success() {
            return Err(eyre!(
                "ffprobe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| eyre!("Failed to parse ffprobe output: {}", e))?;

        let mut keyframes = Vec::new();

        if let Some(frames) = json.get("frames").and_then(|f| f.as_array()) {
            for (idx, frame) in frames.iter().enumerate() {
                let is_keyframe = frame
                    .get("key_frame")
                    .and_then(|k| k.as_i64())
                    .map(|k| k == 1)
                    .unwrap_or(false);

                if is_keyframe {
                    let pts = frame
                        .get("pkt_pts_time")
                        .and_then(|p| {
                            // ffprobe may return the value as either a number or a string
                            p.as_f64()
                                .or_else(|| p.as_str().and_then(|s| s.parse::<f64>().ok()))
                        })
                        .map(|p| (p * 1_000_000_000.0) as u64)
                        .unwrap_or(0);

                    let byte_offset = frame
                        .get("pkt_pos")
                        .and_then(|p| p.as_str())
                        .and_then(|p| p.parse::<u64>().ok())
                        .unwrap_or(0);

                    keyframes.push(KeyframeInfo {
                        frame_index: idx as u64,
                        byte_offset,
                        pts,
                    });
                }
            }
        }

        Ok(keyframes)
    }

    /// Estimate total frames from file size and bitrate
    pub fn estimate_frame_count(file_size_bytes: u64, bitrate_mbps: u32, fps: u32) -> u64 {
        let bitrate_bps = bitrate_mbps as u64 * 1_000_000;
        if bitrate_bps == 0 || fps == 0 {
            return 0;
        }
        let duration_seconds = file_size_bytes * 8 / bitrate_bps;
        duration_seconds * fps as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_metadata_creation() {
        let metadata = VideoMetadata::new(
            "h264".to_string(),
            60,
            [1920, 1080],
            1_564_290_958_000_000_000,
        );

        assert_eq!(metadata.codec, "h264");
        assert_eq!(metadata.fps, 60);
        assert_eq!(metadata.resolution, [1920, 1080]);
        assert_eq!(metadata.frame_duration_ns, 16_666_667);
    }

    #[test]
    fn test_frame_count_estimation() {
        let file_size = 1_000_000_000u64;
        let frames = VideoMetadataExtractor::estimate_frame_count(file_size, 20, 60);
        assert!(frames > 20_000 && frames < 30_000);
    }
}
