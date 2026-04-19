use serde::{Deserialize, Serialize};

/// Supported video encoder types — HEVC (H.265) preferred for GameData Labs buyer spec
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoEncoderType {
    // NOTE: X265 was removed — OBS has no software HEVC encoder.
    // It silently fell back to x264 (H.264), misleading users.
    // Use NvEncHevc/AmfHevc/QsvHevc for hardware HEVC instead.
    X264,
    NvEncHevc,
    NvEnc,
    AmfHevc,
    Amf,
    QsvHevc,
    Qsv,
}
impl std::fmt::Display for VideoEncoderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VideoEncoderType::X264 => write!(f, "OBS x264 (CPU, H.264)"),
            VideoEncoderType::NvEncHevc => write!(f, "NVIDIA NVENC (GPU, HEVC)"),
            VideoEncoderType::NvEnc => write!(f, "NVIDIA NVENC (GPU, H.264)"),
            VideoEncoderType::AmfHevc => write!(f, "AMD HW (GPU, HEVC)"),
            VideoEncoderType::Amf => write!(f, "AMD HW (GPU, H.264)"),
            VideoEncoderType::QsvHevc => write!(f, "QuickSync (GPU, HEVC)"),
            VideoEncoderType::Qsv => write!(f, "QuickSync (GPU, H.264)"),
        }
    }
}
impl VideoEncoderType {
    pub fn id(&self) -> &str {
        match self {
            VideoEncoderType::X264 => "x264",
            VideoEncoderType::NvEncHevc => "nvenc_hevc",
            VideoEncoderType::NvEnc => "nvenc",
            VideoEncoderType::AmfHevc => "amf_hevc",
            VideoEncoderType::Amf => "amf",
            VideoEncoderType::QsvHevc => "qsv_hevc",
            VideoEncoderType::Qsv => "qsv",
        }
    }

    /// Whether this encoder produces HEVC (H.265) output
    pub fn is_hevc(&self) -> bool {
        matches!(
            self,
            VideoEncoderType::NvEncHevc | VideoEncoderType::AmfHevc | VideoEncoderType::QsvHevc
        )
    }

    /// Get the H.264 fallback for a HEVC encoder
    pub fn h264_fallback(&self) -> Self {
        match self {
            VideoEncoderType::NvEncHevc => VideoEncoderType::NvEnc,
            VideoEncoderType::AmfHevc => VideoEncoderType::Amf,
            VideoEncoderType::QsvHevc => VideoEncoderType::Qsv,
            other => *other,
        }
    }
}

/// Preset options for different encoder types.
/// v2.5.2: reordered so the DEFAULT (index 0) is "veryfast".
/// Session logs on AMD Ryzen 7640HS + Radeon 760M iGPU with "fast" preset
/// produced 1 FPS effective throughput at 1080p/10Mbps — the software
/// encoder was CPU-bound. "veryfast" delivers ~3× throughput with a
/// tiny quality hit that AI-world-model training can absorb.
/// https://github.com/obsproject/obs-studio/blob/5ec3af3f6d6465122dc2b0abff9661cbe64b406b/plugins/obs-x264/obs-x264.c
pub const X264_PRESETS: &[&str] = &["veryfast", "faster", "fast"];

/// https://github.com/obsproject/obs-studio/blob/0b1229632063a13dfd26cf1cd9dd43431d8c68f6/plugins/obs-nvenc/nvenc-properties.c#L145
pub const NVENC_PRESETS: &[&str] = &["p7", "p6", "p5", "p4", "p3", "p2", "p1"];

/// https://github.com/obsproject/obs-studio/blob/c025f210d36ada93c6b9ef2affd0f671b34c9775/plugins/obs-qsv11/obs-qsv11.c#L293-L311
pub const QSV_TARGET_USAGES: &[&str] = &[
    "quality", "balanced", "speed", "veryfast", "faster", "fast", "medium",
];

/// https://github.com/obsproject/obs-studio/blob/c025f210d36ada93c6b9ef2affd0f671b34c9775/plugins/obs-ffmpeg/texture-amf.cpp#L1276-L1284
pub const AMF_PRESETS: &[&str] = &["quality", "balanced", "speed"];

/// ffmpeg-nvenc: https://github.com/obsproject/obs-studio/blob/0b1229632063a13dfd26cf1cd9dd43431d8c68f6/plugins/obs-ffmpeg/obs-ffmpeg-nvenc.c#L504
/// obs-nvenc: https://github.com/obsproject/obs-studio/blob/0b1229632063a13dfd26cf1cd9dd43431d8c68f6/plugins/obs-nvenc/nvenc-properties.c#L159
/// both are the same
pub const NVENC_TUNE_OPTIONS: &[&str] = &["hq", "ll", "ull"];

/// H.265 profile for HEVC encoders (buyer spec: main profile)
pub const HEVC_VIDEO_PROFILE: &str = "main";

/// H.264 profile (legacy fallback)
pub const H264_VIDEO_PROFILE: &str = "high";

/// Bitrate (kbps) — buyer spec: 8-12 Mbps for 1080p30 HEVC
pub const BITRATE: i64 = 10_000;

/// Rate control
pub const RATE_CONTROL: &str = "CBR";

/// B-frames
pub const B_FRAMES: i64 = 2;

/// Psycho AQ
pub const PSYCHO_AQ: bool = true;

/// Lookahead
pub const LOOKAHEAD: bool = true;

pub const VIDEO_PROFILE: &str = "high";
