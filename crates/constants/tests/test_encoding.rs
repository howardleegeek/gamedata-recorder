//! Integration tests for `constants::encoding`.
//!
//! `VideoEncoderType` is the source of truth for which OBS encoder the
//! recorder picks on each codec path. The buyer spec mandates HEVC, with
//! an H.264 fallback when hardware HEVC is unavailable — that mapping is
//! encoded in `h264_fallback()`. Locking it down via tests prevents a
//! future refactor from silently mapping NVENC HEVC to AMF H.264 or
//! similar.
//
// `assertions_on_constants` is intentionally allowed in this file. These
// tests pin constant values *at compile time* — that is the whole point.
// If a refactor moves PSYCHO_AQ to a runtime config, the assertion still
// fires (and clippy's warning disappears); until then we want the lint
// quiet so CI stays green.

#![allow(clippy::assertions_on_constants)]
#![allow(clippy::manual_range_contains)]

use constants::encoding::{
    AMF_PRESETS, B_FRAMES, BITRATE, H264_VIDEO_PROFILE, HEVC_VIDEO_PROFILE, LOOKAHEAD,
    NVENC_PRESETS, NVENC_TUNE_OPTIONS, PSYCHO_AQ, QSV_TARGET_USAGES, RATE_CONTROL, VIDEO_PROFILE,
    VideoEncoderType, X264_PRESETS,
};

// ---------------------------------------------------------------------------
// VideoEncoderType::id() — string identifier per OBS plugin name
// ---------------------------------------------------------------------------

#[test]
fn encoder_id_matches_obs_plugin_names() {
    // The `id()` string is the exact OBS plugin name passed to
    // `obs_encoder_create()`. If any of these change, recording silently
    // breaks for that encoder. Lock down the full map.
    assert_eq!(VideoEncoderType::X264.id(), "x264");
    assert_eq!(VideoEncoderType::NvEncHevc.id(), "nvenc_hevc");
    assert_eq!(VideoEncoderType::NvEnc.id(), "nvenc");
    assert_eq!(VideoEncoderType::AmfHevc.id(), "amf_hevc");
    assert_eq!(VideoEncoderType::Amf.id(), "amf");
    assert_eq!(VideoEncoderType::QsvHevc.id(), "qsv_hevc");
    assert_eq!(VideoEncoderType::Qsv.id(), "qsv");
}

// ---------------------------------------------------------------------------
// VideoEncoderType::is_hevc() — buyer spec requires HEVC output
// ---------------------------------------------------------------------------

#[test]
fn hevc_encoders_report_is_hevc_true() {
    assert!(VideoEncoderType::NvEncHevc.is_hevc());
    assert!(VideoEncoderType::AmfHevc.is_hevc());
    assert!(VideoEncoderType::QsvHevc.is_hevc());
}

#[test]
fn h264_encoders_report_is_hevc_false() {
    assert!(!VideoEncoderType::X264.is_hevc());
    assert!(!VideoEncoderType::NvEnc.is_hevc());
    assert!(!VideoEncoderType::Amf.is_hevc());
    assert!(!VideoEncoderType::Qsv.is_hevc());
}

// ---------------------------------------------------------------------------
// VideoEncoderType::h264_fallback() — HEVC -> H.264 mapping
// ---------------------------------------------------------------------------

#[test]
fn h264_fallback_maps_hevc_variants_to_same_vendor_h264() {
    // CRITICAL CONTRACT: the recorder downgrades HEVC -> H.264 on the
    // *same vendor's* hardware path. A bug here that swaps vendors
    // (e.g. NVENC HEVC -> AMF H.264) would crash recording on the wrong
    // GPU. Lock the full map down.
    assert_eq!(
        VideoEncoderType::NvEncHevc.h264_fallback(),
        VideoEncoderType::NvEnc
    );
    assert_eq!(
        VideoEncoderType::AmfHevc.h264_fallback(),
        VideoEncoderType::Amf
    );
    assert_eq!(
        VideoEncoderType::QsvHevc.h264_fallback(),
        VideoEncoderType::Qsv
    );
}

#[test]
fn h264_fallback_is_identity_for_h264_variants() {
    // Calling `h264_fallback()` on an already-H.264 variant returns the
    // same variant unchanged. The recorder relies on this for the loop
    // "try HEVC, fall back to .h264_fallback() if init fails" — if the
    // identity property breaks, the fallback path goes around twice.
    assert_eq!(
        VideoEncoderType::X264.h264_fallback(),
        VideoEncoderType::X264
    );
    assert_eq!(
        VideoEncoderType::NvEnc.h264_fallback(),
        VideoEncoderType::NvEnc
    );
    assert_eq!(VideoEncoderType::Amf.h264_fallback(), VideoEncoderType::Amf);
    assert_eq!(VideoEncoderType::Qsv.h264_fallback(), VideoEncoderType::Qsv);
}

// ---------------------------------------------------------------------------
// VideoEncoderType::Display — must be human-readable for logs
// ---------------------------------------------------------------------------

#[test]
fn encoder_display_is_human_readable() {
    // Display strings get surfaced in logs, GUI status text, and crash
    // reports. They are part of the support-debugging contract — locking
    // them down means support engineers can grep with stable strings.
    assert_eq!(
        format!("{}", VideoEncoderType::X264),
        "OBS x264 (CPU, H.264)"
    );
    assert_eq!(
        format!("{}", VideoEncoderType::NvEncHevc),
        "NVIDIA NVENC (GPU, HEVC)"
    );
    assert_eq!(
        format!("{}", VideoEncoderType::NvEnc),
        "NVIDIA NVENC (GPU, H.264)"
    );
    assert_eq!(
        format!("{}", VideoEncoderType::AmfHevc),
        "AMD HW (GPU, HEVC)"
    );
    assert_eq!(format!("{}", VideoEncoderType::Amf), "AMD HW (GPU, H.264)");
    assert_eq!(
        format!("{}", VideoEncoderType::QsvHevc),
        "QuickSync (GPU, HEVC)"
    );
    assert_eq!(
        format!("{}", VideoEncoderType::Qsv),
        "QuickSync (GPU, H.264)"
    );
}

// ---------------------------------------------------------------------------
// Encoder derives — Copy, PartialEq, Hash
// ---------------------------------------------------------------------------

#[test]
fn encoder_type_is_copy_and_equatable() {
    // The recorder stores `VideoEncoderType` in HashMaps keyed by encoder
    // (codec stats per encoder). Hash + PartialEq + Copy must hold.
    let a = VideoEncoderType::NvEncHevc;
    let b = a; // Copy
    assert_eq!(a, b);
    use std::collections::HashSet;
    let mut s: HashSet<VideoEncoderType> = HashSet::new();
    s.insert(VideoEncoderType::NvEncHevc);
    s.insert(VideoEncoderType::NvEnc);
    s.insert(VideoEncoderType::NvEncHevc); // duplicate, set size stays
    assert_eq!(s.len(), 2);
}

#[test]
fn encoder_type_serde_round_trip() {
    // Configs persist the chosen encoder by serde-serializing the enum.
    // Round-trip must be exact — a renamed variant would brick stored
    // user configs.
    for v in [
        VideoEncoderType::X264,
        VideoEncoderType::NvEnc,
        VideoEncoderType::NvEncHevc,
        VideoEncoderType::Amf,
        VideoEncoderType::AmfHevc,
        VideoEncoderType::Qsv,
        VideoEncoderType::QsvHevc,
    ] {
        let s = serde_json::to_string(&v).unwrap();
        let back: VideoEncoderType = serde_json::from_str(&s).unwrap();
        assert_eq!(v, back, "round-trip failed for {v:?}");
    }
}

// ---------------------------------------------------------------------------
// Preset / target arrays — locked-down OBS knobs
// ---------------------------------------------------------------------------

#[test]
fn x264_preset_default_is_veryfast() {
    // R2.5.2 documented decision: x264 default is "veryfast" because the
    // AMD Ryzen 7640HS + Radeon 760M iGPU benchmark showed 1 FPS at
    // "fast". Index 0 of this array IS the default — locking it down
    // catches accidental reorderings.
    assert_eq!(X264_PRESETS[0], "veryfast");
    assert_eq!(X264_PRESETS.len(), 3);
    assert!(X264_PRESETS.contains(&"veryfast"));
    assert!(X264_PRESETS.contains(&"faster"));
    assert!(X264_PRESETS.contains(&"fast"));
}

#[test]
fn nvenc_presets_ordered_quality_to_speed() {
    // NVENC presets p1..p7 are highest-speed to highest-quality per
    // NVIDIA docs. Our convention: index 0 is highest quality (p7).
    assert_eq!(NVENC_PRESETS[0], "p7");
    assert_eq!(NVENC_PRESETS.last(), Some(&"p1"));
    assert_eq!(NVENC_PRESETS.len(), 7);
}

#[test]
fn qsv_target_usage_default_is_quality() {
    // QSV "quality" target maps to TU1 (highest quality). Buyer spec
    // wants the highest-quality knob in slot 0 so the default is the
    // best output we can hand to training pipelines.
    assert_eq!(QSV_TARGET_USAGES[0], "quality");
    assert_eq!(QSV_TARGET_USAGES.len(), 7);
}

#[test]
fn amf_presets_locked() {
    // AMD AMF presets are exactly three: quality, balanced, speed.
    assert_eq!(AMF_PRESETS, &["quality", "balanced", "speed"]);
}

#[test]
fn nvenc_tune_options_locked() {
    // The OBS NVENC plugin exposes hq / ll / ull (high quality / low
    // latency / ultra-low latency). Recording is "hq"; streaming would
    // be "ll" or "ull". Lock the order so config presets keep working.
    assert_eq!(NVENC_TUNE_OPTIONS, &["hq", "ll", "ull"]);
}

// ---------------------------------------------------------------------------
// Numeric / string constants — buyer spec invariants
// ---------------------------------------------------------------------------

#[test]
fn hevc_video_profile_is_main_per_buyer_spec() {
    // Buyer spec: HEVC main profile. main10 (10-bit) is NOT what they
    // want; lock down "main".
    assert_eq!(HEVC_VIDEO_PROFILE, "main");
}

#[test]
fn h264_video_profile_is_high_per_legacy_spec() {
    // H.264 fallback: high profile (broad decoder support).
    assert_eq!(H264_VIDEO_PROFILE, "high");
    assert_eq!(VIDEO_PROFILE, "high");
}

#[test]
fn bitrate_is_within_buyer_spec_range() {
    // Buyer spec: 8-12 Mbps for 1080p30 HEVC. We ship 10_000 (10 Mbps).
    assert!(BITRATE >= 8_000 && BITRATE <= 12_000);
    assert_eq!(BITRATE, 10_000);
}

#[test]
fn rate_control_is_cbr() {
    // CBR (constant bitrate) is the buyer spec's mandated mode for
    // training-data consistency — variable bitrates make frame-byte
    // accounting unreliable for downstream batch jobs.
    assert_eq!(RATE_CONTROL, "CBR");
}

#[test]
fn b_frames_count_is_two() {
    // B-frames = 2 is the OBS default. Locking it down for explicit
    // contract test rather than letting the default drift.
    assert_eq!(B_FRAMES, 2);
}

#[test]
fn quality_enhancers_enabled() {
    // PSY-AQ + lookahead = quality knobs we want on by default. Buyer
    // spec values quality over CPU savings.
    assert!(PSYCHO_AQ);
    assert!(LOOKAHEAD);
}
