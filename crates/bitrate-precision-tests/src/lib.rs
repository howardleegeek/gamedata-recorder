//! Cross-platform shim around `src/config_bitrate.rs`.
//!
//! Source-includes one file from the top-level `gamedata-recorder` crate via
//! `#[path = ...]`:
//!
//!   - `src/config_bitrate.rs` (R2.10 bitrate-band constants + clamp logic)
//!
//! Building only this crate on macOS/Linux CI lets us run the clamp logic's
//! tests without pulling the Windows-only deps (libobs-wrapper, glfw,
//! tray-icon, egui_overlay) that the top-level crate requires. Mirrors the
//! pattern used by `crates/action-camera-tests/`.
//!
//! The bitrate source file deliberately depends on nothing but `tracing` —
//! see the top-of-file comment in `config_bitrate.rs` for the rationale.

#[path = "../../../src/config_bitrate.rs"]
pub mod config_bitrate;

// Re-export the public surface the integration tests need so they can
// `use bitrate_precision_tests::*` without spelling the inner module path.
pub use config_bitrate::{
    BITRATE_BAND_MAX_KBPS, BITRATE_BAND_MIN_KBPS, BitrateClampResult,
    DEFAULT_RECORDING_BITRATE_KBPS, clamp_recording_bitrate_kbps,
    clamp_recording_bitrate_kbps_detailed, default_recording_bitrate_kbps,
};

/// Helper struct mimicking the relevant subset of `Preferences` for the
/// "backward compat: config without `recording_bitrate_kbps` deserializes
/// to default" test case. The real `Preferences` struct has ~20 fields and
/// drags in Windows-only deps; this is the minimal shape that exercises the
/// `#[serde(default = "default_recording_bitrate_kbps")]` contract.
///
/// Kept here (rather than in the test file) so other future R2.10 tests in
/// this crate can reuse it.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesShim {
    #[serde(default = "default_recording_bitrate_kbps")]
    pub recording_bitrate_kbps: u32,
}

impl Default for PreferencesShim {
    fn default() -> Self {
        Self {
            recording_bitrate_kbps: default_recording_bitrate_kbps(),
        }
    }
}

/// Helper struct mimicking the on-disk metadata.json shape (subset).
///
/// Used by the "metadata serializes `encoder_bitrate_kbps` as int" test —
/// we don't need the full `output_types::Metadata` (40+ fields, all Option
/// gated on Option-serialization rules); the wire-contract assertion is
/// just "`encoder_bitrate_kbps` is a bare JSON int, not a string".
///
/// `Option<u32>` matches the production field type and exercises both the
/// `Some(_)` (new recordings) and `None` (legacy recordings) paths.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MetadataShim {
    pub game_exe: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub encoder_bitrate_kbps: Option<u32>,
}
