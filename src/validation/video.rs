use std::{path::Path, time::Duration};

use constants::{MAX_FOOTAGE, MIN_FOOTAGE};

use crate::output_types::Metadata;

pub fn validate(video_path: &Path, metadata: &Metadata) -> Vec<String> {
    let mut invalid_reasons = vec![];

    let duration = Duration::from_secs_f64(metadata.duration);
    if duration < MIN_FOOTAGE {
        invalid_reasons.push(format!("Video length {} too short.", metadata.duration));
    }
    if duration > Duration::from_secs_f32(MAX_FOOTAGE.as_secs_f32() * 1.5) {
        invalid_reasons.push(format!("Video length {} too long.", metadata.duration));
    }

    let size_bytes = match std::fs::metadata(video_path).map(|m| m.len()) {
        Ok(size_bytes) => size_bytes,
        Err(e) => {
            invalid_reasons.push(format!("Video size unknown: {e}"));
            return invalid_reasons;
        }
    };

    let size_mbytes = size_bytes as f64 / (1024.0 * 1024.0);
    let size_mbits = size_mbytes * 8.0;

    // Use a baseline bitrate for validation (slightly lower than actual encoding bitrate)
    // Actual encoding is ~4.8 Mbps, we use 3.8 Mbps to allow some variance
    let baseline_bitrate_mbps = 3.8;
    let expected_mbits = baseline_bitrate_mbps * metadata.duration;

    // Log video file stats for diagnostics
    let actual_bitrate_mbps = size_mbits / metadata.duration;
    tracing::info!(
        "Video validation: size={:.2}MB ({} bytes), duration={:.2}s, actual_bitrate={:.2}Mbps, expected_baseline={:.2}Mb",
        size_mbytes,
        size_bytes,
        metadata.duration,
        actual_bitrate_mbps,
        expected_mbits
    );

    if size_mbits < 0.25 * expected_mbits {
        invalid_reasons.push(format!(
            "Video size {size_mbits:.2}Mb too small compared to expected {expected_mbits:.2}Mb",
        ));
    }

    // Check for oversized files (> 4x expected size suggests encoder malfunction)
    // At 4777 kbps CBR, a 600s video should be ~358MB. Using 3.8 Mbps baseline,
    // 4x expected would be ~1.14GB for a 600s video, which gives headroom for
    // normal variation while catching encoder malfunctions (e.g., multi-GB files).
    if size_mbits > 4.0 * expected_mbits {
        tracing::error!(
            "Video file is abnormally large: {:.2}MB for {:.2}s video (actual bitrate: {:.2}Mbps, expected max: ~{:.1}Mbps). Possible encoder malfunction.",
            size_mbytes,
            metadata.duration,
            actual_bitrate_mbps,
            baseline_bitrate_mbps * 4.0
        );
        invalid_reasons.push(format!(
            "Video size {size_mbits:.2}Mb too large compared to expected {expected_mbits:.2}Mb (possible encoder malfunction)",
        ));
    }

    invalid_reasons
}
