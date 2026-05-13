//! Bitrate-band logic for `Preferences::recording_bitrate_kbps` (PRD R2.10).
//!
//! Split out from `src/config.rs` for two reasons:
//!
//!   1. **Cross-platform testability.** `config.rs` pulls in `libobs_wrapper`,
//!      `input_capture::ConsentGuard`, `egui_wgpu::wgpu`, and other
//!      Windows-only deps the moment you source-include it from a sibling
//!      test crate. This file deliberately depends on nothing but `tracing`
//!      so the `bitrate-precision-tests` crate can `#[path = "..."]` it from
//!      macOS / Linux CI and exercise the clamp logic.
//!
//!   2. **Single source of truth for the band constants.** The buyer-spec
//!      band (`[6 000, 12 000]` kbps) shows up in three places:
//!      `Preferences::recording_bitrate_kbps`, the OBS encoder `bitrate`
//!      key, and `Metadata::encoder_bitrate_kbps`. Centralising the
//!      constants here makes a future buyer-spec change a one-line edit.
//!
//! The values here are kbps to match what OBS's encoder `bitrate` setting
//! takes (an `i64` of kbps) and to keep the on-disk JSON int-only.

/// Buyer-accepted bitrate band lower bound, in kbps (PRD R2.10).
///
/// 6 Mbps is the lowest 1080p30 bitrate the GameData Labs ingest will accept;
/// below this, residual blocking artifacts measurably hurt the world-model
/// training signal. Values below this clamp UP to `BITRATE_BAND_MIN_KBPS`.
pub const BITRATE_BAND_MIN_KBPS: u32 = 6_000;

/// Buyer-accepted bitrate band upper bound, in kbps (PRD R2.10).
///
/// 12 Mbps is the highest 1080p30 bitrate the buyer pays for; above this we
/// just spend disk + upload bandwidth without improving the training signal.
/// Values above this clamp DOWN to `BITRATE_BAND_MAX_KBPS`.
pub const BITRATE_BAND_MAX_KBPS: u32 = 12_000;

/// Default target bitrate for new recordings (PRD R2.10).
///
/// 8 Mbps is the midpoint of the `[6, 12]` Mbps buyer band and the spec's
/// stated target. Old `config.json` files without `recording_bitrate_kbps`
/// pick this up via `#[serde(default = ...)]` on the field.
pub const DEFAULT_RECORDING_BITRATE_KBPS: u32 = 8_000;

/// Serde default function for `Preferences::recording_bitrate_kbps`.
///
/// Returns [`DEFAULT_RECORDING_BITRATE_KBPS`]. Kept as a named function (not
/// an inline closure) because `#[serde(default = "...")]` requires a path
/// expression that resolves to a `fn() -> T`.
pub fn default_recording_bitrate_kbps() -> u32 {
    DEFAULT_RECORDING_BITRATE_KBPS
}

/// Latch ensuring the "bitrate out of band, clamped" warning fires at most
/// once per process. The clamp itself is cheap and idempotent; the warning
/// is the noisy part — if a misconfigured `config.json` is loaded once and
/// then queried per-recording, we'd otherwise spam the log on every start.
///
/// `OnceLock` rather than `AtomicBool` so the message is emitted exactly
/// when the latch transitions from unset → set; subsequent `set` calls
/// return `Err` and the warn block is skipped without a separate
/// load+store dance.
static BITRATE_CLAMP_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Outcome of a bitrate clamp — returned to callers that want to observe
/// whether a clamp happened (e.g. tests). Production callers just want the
/// `effective_kbps`; tests want the `clamped` flag too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitrateClampResult {
    /// The clamped value handed to the encoder.
    pub effective_kbps: u32,
    /// `true` iff the raw input was outside `[BITRATE_BAND_MIN_KBPS,
    /// BITRATE_BAND_MAX_KBPS]` and had to be moved.
    pub clamped: bool,
}

/// Clamp a raw kbps value into the buyer-accepted band, emitting a one-shot
/// warning if it had to be moved. Returns the full
/// [`BitrateClampResult`] so callers that care (tests, telemetry) can see
/// whether clamping kicked in.
///
/// The warning fires at most once per process — see [`BITRATE_CLAMP_WARNED`].
/// Test crates that want to observe the warning side-effect should call this
/// once per test binary (`cargo test` gives each test binary its own
/// process) and inspect the result via [`BitrateClampResult::clamped`].
pub fn clamp_recording_bitrate_kbps_detailed(raw_kbps: u32) -> BitrateClampResult {
    if raw_kbps < BITRATE_BAND_MIN_KBPS {
        if BITRATE_CLAMP_WARNED.set(()).is_ok() {
            tracing::warn!(
                requested_kbps = raw_kbps,
                effective_kbps = BITRATE_BAND_MIN_KBPS,
                min_kbps = BITRATE_BAND_MIN_KBPS,
                max_kbps = BITRATE_BAND_MAX_KBPS,
                "recording_bitrate_kbps below buyer-accepted band; clamping up. \
                 This warning fires once per process."
            );
        }
        BitrateClampResult {
            effective_kbps: BITRATE_BAND_MIN_KBPS,
            clamped: true,
        }
    } else if raw_kbps > BITRATE_BAND_MAX_KBPS {
        if BITRATE_CLAMP_WARNED.set(()).is_ok() {
            tracing::warn!(
                requested_kbps = raw_kbps,
                effective_kbps = BITRATE_BAND_MAX_KBPS,
                min_kbps = BITRATE_BAND_MIN_KBPS,
                max_kbps = BITRATE_BAND_MAX_KBPS,
                "recording_bitrate_kbps above buyer-accepted band; clamping down. \
                 This warning fires once per process."
            );
        }
        BitrateClampResult {
            effective_kbps: BITRATE_BAND_MAX_KBPS,
            clamped: true,
        }
    } else {
        BitrateClampResult {
            effective_kbps: raw_kbps,
            clamped: false,
        }
    }
}

/// Production helper: just the effective kbps, discarding the clamp flag.
///
/// Most callers (the OBS encoder wiring, the metadata writer) only care
/// about the value to send through. Tests should call
/// [`clamp_recording_bitrate_kbps_detailed`] instead so they can assert on
/// whether clamping happened.
pub fn clamp_recording_bitrate_kbps(raw_kbps: u32) -> u32 {
    clamp_recording_bitrate_kbps_detailed(raw_kbps).effective_kbps
}
