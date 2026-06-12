use color_eyre::eyre::{Result, eyre};
use constants::encoding::VideoEncoderType;
use input_capture::{ConsentGuard, ConsentStatus};
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// PCI vendor IDs — used to identify GPUs in DXGI adapter enumeration.
/// Stable across every GPU that vendor has ever shipped.
const NVIDIA_PCI_VENDOR_ID: u32 = 0x10DE;
const AMD_PCI_VENDOR_ID: u32 = 0x1002;
const INTEL_PCI_VENDOR_ID: u32 = 0x8086;

/// A GPU vendor we can map to a hardware video encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
}

/// Quick probe: which hardware-encoder-capable GPU vendor does the system
/// report? Returns the first matching adapter's vendor, or `None` if no
/// NVIDIA / AMD / Intel adapter is found.
///
/// v2.5.5: rewritten to use direct DXGI adapter enumeration via `wgpu`.
/// v2.5.4 shelled out to `wmic path win32_VideoController get Name`, but
/// `wmic.exe` is deprecated on Windows 11 22H2+ and absent on Windows N /
/// LTSC / Group-Policy-hardened installs. The shell-out returned an error
/// that was swallowed, and NVIDIA users on those systems silently stayed
/// on X264 (software) encoding. The recorder already enumerates DX12
/// adapters at startup (see `src/main.rs` via `wgpu::Instance`), so doing
/// it here too is essentially free, has no external process dependency,
/// and works on every modern Windows SKU.
///
/// v2.6.0: generalized from NVIDIA-only (`detect_nvidia_gpu`) to also
/// recognize AMD (AMF) and Intel (QuickSync). Previously AMD/Intel users
/// stayed on software X264, which is CPU-bound — confirmed ~1 FPS effective
/// plus system-wide lag on an AMD Radeon 780M iGPU.
fn detect_gpu_vendor() -> Result<Option<GpuVendor>> {
    #[cfg(target_os = "windows")]
    {
        use egui_wgpu::wgpu;

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        // Prefer the first adapter that maps to a hardware encoder. wgpu
        // enumerates discrete adapters ahead of integrated ones, so the
        // first NVIDIA/AMD/Intel match is the most capable available GPU.
        let vendor = instance
            .enumerate_adapters(wgpu::Backends::DX12)
            .into_iter()
            .find_map(|adapter| match adapter.get_info().vendor {
                NVIDIA_PCI_VENDOR_ID => Some(GpuVendor::Nvidia),
                AMD_PCI_VENDOR_ID => Some(GpuVendor::Amd),
                INTEL_PCI_VENDOR_ID => Some(GpuVendor::Intel),
                _ => None,
            });
        Ok(vendor)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(None)
    }
}

/// Hardware-encoder preference order, best first.
///
/// This is the single source of truth for "which hardware encoder do we
/// prefer when more than one is actually available" and is consulted by
/// [`select_best_available_encoder`]. Order rationale:
///
/// * NVENC first — discrete NVIDIA silicon has a dedicated encode ASIC that is
///   essentially free of the render pipeline; it is the highest-quality and
///   lowest-overhead option whenever it is present.
/// * AMF second — AMD's VCN encoder. On the AMD Radeon 780M test machine this
///   is the proven-working path; keeping it ahead of QSV preserves that pick.
/// * QSV last — Intel QuickSync, typically the integrated fallback on Optimus /
///   hybrid laptops where it shares the iGPU with rendering.
///
/// Within each vendor the HEVC (H.265) variant is preferred over H.264 to match
/// the GameData Labs buyer spec, but H.264 is accepted when HEVC is absent (some
/// older silicon / drivers expose only the H.264 encoder).
const HW_ENCODER_PREFERENCE: &[VideoEncoderType] = &[
    VideoEncoderType::NvEncHevc,
    VideoEncoderType::NvEnc,
    VideoEncoderType::AmfHevc,
    VideoEncoderType::Amf,
    VideoEncoderType::QsvHevc,
    VideoEncoderType::Qsv,
];

/// Reconcile a *requested* encoder against the encoders OBS actually probed as
/// available on this machine, returning an encoder that is GUARANTEED to be a
/// safe choice for the recorder to construct.
///
/// This is the robustness keystone for multi-GPU compatibility. `config.rs`
/// (`detect_gpu_vendor` → `Config::load`) optimistically upgrades the encoder
/// to a vendor's hardware encoder based purely on which GPU adapter is present
/// in DXGI. But a GPU being *present* does not mean its OBS encoder *registered*:
///
/// * NVIDIA NVENC can fail to register when the driver is too old, the
///   consumer-driver NVENC session cap is already saturated by other apps, or
///   (Optimus / hybrid laptops) the discrete GPU is parked and the display is
///   driven by the iGPU.
/// * AMD AMF is only listed when libobs's `obs-amf-test.exe` probe succeeds; if
///   that helper is missing, blocked, or returns nothing, AMF silently drops
///   out of the available list.
/// * Intel QSV requires a supported iGPU + driver.
///
/// In every one of those cases the requested encoder will be ABSENT from
/// `available`. Constructing it anyway makes libobs error out and the whole
/// recording fail. Instead we degrade deterministically:
///
/// 0. If a *software* encoder (`X264`) is requested but any hardware encoder
///    genuinely registered with OBS, UPGRADE to the best available hardware.
///    OBS always lists x264 as available (it is the always-constructible
///    software encoder), so without this step a stale `X264` config would be
///    honoured by step 1 even on a perfectly good NVENC machine — the exact
///    RTX 4060 tester regression (software x264, CPU-bound, ~2 FPS). The
///    OBS-probed `available` list is firmer ground truth than the DXGI vendor
///    probe that drives the optimistic `Config::load` upgrade, so this rescue
///    fires even when that probe silently failed. Hardware requests skip this
///    step and fall through unchanged, preserving the proven AMD-AMF path.
/// 1. If the requested encoder is genuinely available, keep it (this is the
///    common path and keeps the proven AMD-AMF behaviour byte-for-byte
///    identical).
/// 2. If a HEVC encoder was requested but only its H.264 sibling is available
///    (same vendor), step down to H.264 before changing vendor — the smallest
///    possible degradation.
/// 3. Otherwise pick the best *available* hardware encoder by
///    [`HW_ENCODER_PREFERENCE`] (NVENC > AMF > QSV). This is what rescues an
///    Optimus laptop whose NVENC didn't register but whose Intel QSV did: we
///    use QSV instead of collapsing all the way to software.
/// 4. If no hardware encoder is available at all, fall back to software
///    [`VideoEncoderType::X264`]. x264 is OBS's built-in software encoder and is
///    treated as always-constructible; the recorder additionally caps software
///    output to 720p (see `obs_embedded_recorder::video_info`) so a weak/iGPU
///    CPU isn't pegged.
///
/// The return value is therefore never "unset" and, whenever `available`
/// contains at least one usable encoder, is always a member of `available`.
/// In the degenerate case where the probe returned an empty list (or a list
/// without x264), we still return `X264` as the universal last resort — that
/// matches the recorder's existing "failed to probe, assume x264 only"
/// contract and guarantees the caller always has *something* to construct.
pub(crate) fn select_best_available_encoder(
    requested: VideoEncoderType,
    available: &[VideoEncoderType],
) -> VideoEncoderType {
    // 0. Software-to-hardware rescue (the RTX 4060 tester bug). A stored config
    //    of `X264` must NOT win just because OBS always lists x264 as
    //    "available" — x264 is the always-constructible software encoder, so it
    //    is present in `available` on every machine. Honouring it here is
    //    exactly how a tester with a perfectly good NVENC GPU ended up CPU-bound
    //    on software x264 at ~2 FPS: the optimistic `Config::load` upgrade
    //    (DXGI vendor probe → NvEnc) is the ONLY thing that flips X264->NvEnc,
    //    and that probe can silently return `None`/`Err` (deprecated `wmic`,
    //    remote desktop, hardened Windows SKU, driver quirks). When it does, a
    //    stale software choice reaches the recorder unchallenged.
    //
    //    The encoders OBS actually probed (`available`) are the ground truth a
    //    DXGI vendor string is not: a hardware encoder appearing in this list
    //    means libobs registered it and it WILL construct. So if a *software*
    //    encoder is requested but any hardware encoder genuinely registered,
    //    upgrade to the best available hardware regardless of what the stored
    //    config or the DXGI probe said. Hardware requests are unaffected (they
    //    fall through to the availability check below), so the proven AMD-AMF
    //    and explicit-hardware paths stay byte-for-byte identical. This is the
    //    reconcile-side safety net the start_recording guard relies on.
    if !HW_ENCODER_PREFERENCE.contains(&requested) {
        if let Some(best) = HW_ENCODER_PREFERENCE
            .iter()
            .copied()
            .find(|enc| available.contains(enc))
        {
            return best;
        }
    }

    // 1. Honour the request when it's actually available. Keeps the working
    //    AMD-AMF path (and any explicit user selection) identical.
    if available.contains(&requested) {
        return requested;
    }

    // 2. HEVC was requested but unavailable — try the same-vendor H.264 sibling
    //    before switching vendor. `h264_fallback` is a no-op for non-HEVC
    //    inputs, so this only ever fires for the HEVC variants.
    if requested.is_hevc() {
        let h264_sibling = requested.h264_fallback();
        if h264_sibling != requested && available.contains(&h264_sibling) {
            return h264_sibling;
        }
    }

    // 3. Pick the best hardware encoder that genuinely registered.
    if let Some(best) = HW_ENCODER_PREFERENCE
        .iter()
        .copied()
        .find(|enc| available.contains(enc))
    {
        return best;
    }

    // 4. No hardware encoder available — software x264 is the guaranteed
    //    terminal fallback (720p cap applied downstream in `video_info`).
    VideoEncoderType::X264
}

/// Ordered list of encoders to *attempt to construct*, best first, when the
/// already-selected `primary` encoder might fail to build at runtime.
///
/// [`select_best_available_encoder`] guarantees `primary` is *listed* by OBS,
/// but a listed hardware encoder can still fail [`ObsVideoEncoder::new_from_info`]
/// at construction time — the consumer-NVIDIA NVENC session cap is already
/// saturated (another app, or our own prior session, holds all the encode
/// sessions), the driver is stale, an Optimus dGPU is parked, or AMD's
/// `obs-amf-test.exe` was blocked between probe and use. This chain lets the
/// recorder degrade through the next-best *constructible* encoder instead of
/// failing the whole recording with no video.
///
/// Ordering, deduplicated, with `primary` always first:
///   1. `primary` (the encoder selection already settled on).
///   2. Every hardware encoder in `available`, walked in [`HW_ENCODER_PREFERENCE`]
///      order (NVENC-HEVC > NVENC > AMF-HEVC > AMF > QSV-HEVC > QSV), excluding
///      `primary`. This is the SAME preference the rest of the pipeline uses.
///   3. [`VideoEncoderType::X264`] as the terminal element — OBS's built-in
///      software encoder is always constructible, so the chain can never end in
///      "no encoder". (Skipped only if `primary` already *is* x264, since it's
///      then already the first element.)
///
/// Purely a function of its inputs (no OBS state), so the degradation order is
/// unit-tested below without a live OBS context.
pub(crate) fn runtime_fallback_chain(
    primary: VideoEncoderType,
    available: &[VideoEncoderType],
) -> Vec<VideoEncoderType> {
    let mut chain = Vec::with_capacity(HW_ENCODER_PREFERENCE.len() + 2);
    chain.push(primary);

    // Next-best hardware encoders that actually registered, in canonical order,
    // skipping the primary (already at the front) and any duplicates.
    for enc in HW_ENCODER_PREFERENCE
        .iter()
        .copied()
        .filter(|enc| available.contains(enc) && *enc != primary)
    {
        chain.push(enc);
    }

    // Terminal software fallback — always constructible. Only appended if it
    // isn't already the primary, so x264-primary recordings don't list it twice.
    if primary != VideoEncoderType::X264 {
        chain.push(VideoEncoderType::X264);
    }

    chain
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
// camel case renames are legacy from old existing configs, we want it to be backwards-compatible with previous owl releases that used electron
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(default = "default_start_key")]
    pub start_recording_key: String,
    #[serde(default = "default_stop_key")]
    pub stop_recording_key: String,
    #[serde(default)]
    pub stop_hotkey_enabled: bool,
    #[serde(default)]
    pub unreliable_connection: bool,
    #[serde(default)]
    pub overlay_location: OverlayLocation,
    #[serde(default = "default_opacity")]
    pub overlay_opacity: u8,
    #[serde(default)]
    pub delete_uploaded_files: bool,
    #[serde(default)]
    pub auto_upload_on_completion: bool,
    #[serde(default)]
    pub honk: bool,
    #[serde(default = "default_honk_volume")]
    pub honk_volume: u8,
    #[serde(default)]
    pub audio_cues: AudioCues,
    /// Capture microphone audio alongside desktop audio when using monitor
    /// capture. Default: false — microphone is OFF by default to avoid
    /// privacy surprises. Desktop audio is always captured in monitor-capture
    /// mode; this flag only controls whether the default input device is
    /// additionally routed into the recording.
    ///
    /// Game-capture (hook) mode is unaffected: it taps game audio via the
    /// OBS hook and does not consult this flag.
    #[serde(default)]
    pub record_microphone: bool,
    /// Suppress writing `action_camera.json` next to each session's other
    /// artifacts. Default: `false` — the file is written by default because
    /// the buyer's training plugin treats it as a wire contract. Power
    /// users who don't ship to the buyer pipeline can opt out to save
    /// ~7-15 MB per 30-minute 30 fps session.
    ///
    /// When `true`, the recorder still writes `inputs.jsonl`, `frames.jsonl`,
    /// and `metadata.json` exactly as before — `action_camera.json` is the
    /// ONLY file affected by this flag. The post-hoc Python adapter at
    /// `oyster-enrichment/bin/convert_to_action_camera.py` can rebuild the
    /// file from the other artifacts at any time.
    #[serde(default)]
    pub disable_action_camera_output: bool,
    #[serde(default)]
    pub recording_backend: RecordingBackend,
    #[serde(default)]
    pub encoder: EncoderSettings,
    #[serde(default = "default_recording_location")]
    pub recording_location: std::path::PathBuf,
    /// Per-game configuration settings, keyed by executable name (e.g., "hl2")
    #[serde(default)]
    pub games: HashMap<String, GameConfig>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            start_recording_key: default_start_key(),
            stop_recording_key: default_stop_key(),
            stop_hotkey_enabled: Default::default(),
            unreliable_connection: Default::default(),
            overlay_location: Default::default(),
            overlay_opacity: default_opacity(),
            delete_uploaded_files: Default::default(),
            auto_upload_on_completion: Default::default(),
            honk: Default::default(),
            honk_volume: default_honk_volume(),
            audio_cues: Default::default(),
            record_microphone: false,
            disable_action_camera_output: false,
            recording_backend: Default::default(),
            encoder: Default::default(),
            recording_location: default_recording_location(),
            games: Default::default(),
        }
    }
}
impl Preferences {
    pub fn start_recording_key(&self) -> &str {
        &self.start_recording_key
    }
    pub fn stop_recording_key(&self) -> &str {
        if self.stop_hotkey_enabled {
            &self.stop_recording_key
        } else {
            &self.start_recording_key
        }
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum RecordingBackend {
    #[default]
    Embedded,
    Socket,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum OverlayLocation {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}
impl OverlayLocation {
    pub const ALL: [OverlayLocation; 4] = [
        OverlayLocation::TopLeft,
        OverlayLocation::TopRight,
        OverlayLocation::BottomLeft,
        OverlayLocation::BottomRight,
    ];
}
impl std::fmt::Display for OverlayLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OverlayLocation::TopLeft => write!(f, "Top Left"),
            OverlayLocation::TopRight => write!(f, "Top Right"),
            OverlayLocation::BottomLeft => write!(f, "Bottom Left"),
            OverlayLocation::BottomRight => write!(f, "Bottom Right"),
        }
    }
}

/// Audio cue settings for recording events
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioCues {
    pub start_recording: String,
    pub stop_recording: String,
}
impl Default for AudioCues {
    fn default() -> Self {
        Self {
            start_recording: "default_start.mp3".to_string(),
            stop_recording: "default_end.mp3".to_string(),
        }
    }
}

/// OBS capture strategy for a particular game.
///
/// This supersedes the v2.5.8 binary `use_window_capture` flag (kept for
/// legacy config compatibility — it is only consulted when `capture_mode`
/// is absent/`Auto` and the game is NOT on the hook-required allowlist,
/// in which case the historical meaning is preserved).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Decide at start-of-recording. The app targets
    /// `x86_64-pc-windows-msvc` and requires Win10, so WGC (Win10 1903+)
    /// is always available. Resolution order:
    /// - `test_game` → Monitor (the CI harness is DWM-composited and
    ///   Monitor captures it fine; pinning Monitor avoids churning the
    ///   existing E2E green-pixel assertions in the same PR that
    ///   introduces WGC).
    /// - Game on [`constants::KNOWN_HOOK_REQUIRED_GAMES`] → GameHook,
    ///   because empirical testing has shown WGC is broken for them.
    /// - Legacy `use_window_capture = false` override → GameHook, to
    ///   preserve the v2.5.8 power-user escape hatch.
    /// - Anything else → WGC, Microsoft's modern official capture API.
    ///   It handles exclusive fullscreen cleanly, doesn't require DLL
    ///   injection, and is the industry-standard recommendation for
    ///   games that strip the `game_capture` hook (CS2 under anti-hook).
    #[default]
    Auto,
    /// Force monitor capture regardless of game. Correct choice for games
    /// that always run windowed / borderless and for users who want the
    /// absolute-safest anti-cheat footprint.
    Monitor,
    /// Force the OBS `game_capture` hook — libobs injects a module into
    /// the target process to grab frames directly out of the swap chain.
    /// Required for games where WGC is known-broken (see
    /// [`constants::KNOWN_HOOK_REQUIRED_GAMES`]). Beware: stronger
    /// anti-hook titles (CS2 vs VAC-whitelisted OBS) will still refuse
    /// the injection and leave you with a black MP4.
    GameHook,
    /// Force Windows.Graphics.Capture (WGC) — Microsoft's official capture
    /// API, Win10 1903+. Captures the game's surface through the OS
    /// compositor without injecting into the process, so it bypasses
    /// anti-hook heuristics that stop `GameCapture`. This is the Auto
    /// default for games not on [`constants::KNOWN_HOOK_REQUIRED_GAMES`].
    Wgc,
}

/// Per-game configuration settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GameConfig {
    /// Legacy (v2.5.8) capture selector: `true` = monitor capture,
    /// `false` = game-capture hook. Retained so existing persisted configs
    /// continue to Just Work, but new logic should use `capture_mode`
    /// instead. See [`CaptureMode::Auto`] for the resolution rule.
    pub use_window_capture: bool,
    /// Modern capture selector. Defaults to `Auto` which prefers WGC
    /// on Win10 1903+ and falls back to GameHook for games on
    /// [`constants::KNOWN_HOOK_REQUIRED_GAMES`] (see `CaptureMode`
    /// docs for the full resolution order).
    #[serde(default)]
    pub capture_mode: CaptureMode,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            use_window_capture: true, // v2.5.8+: screen capture is the default for compatibility
            capture_mode: CaptureMode::default(),
        }
    }
}

/// Concrete capture mode after resolving `CaptureMode::Auto`. Used by
/// the recorder plumbing when actually constructing OBS sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveCaptureMode {
    /// MonitorCaptureSource attached to the display under the game HWND.
    Monitor,
    /// GameCaptureSource hooked into the game process.
    GameHook,
    /// Windows.Graphics.Capture (`wgc_capture` source). Win10 1903+.
    /// Captures the window's swapchain surface via OS compositor APIs —
    /// no DLL injection, works for exclusive fullscreen D3D11/D3D12.
    /// This is the Auto default for games not on
    /// [`constants::KNOWN_HOOK_REQUIRED_GAMES`].
    Wgc,
}

/// `test_game` is the synthetic GPU-rendered harness window the CI spins
/// up to smoke-test end-to-end recording (see `.github/workflows/
/// ci-e2e.yml`). It's DWM-composited and Monitor captures it fine; we
/// pin it to Monitor specifically to avoid changing CI behaviour now
/// that Auto's default is WGC. WGC would also work in practice, but
/// the existing green-pixel assertions in the E2E harness were written
/// against Monitor capture output and we'd rather not churn them in
/// the same PR that flips the Auto default.
const TEST_GAME_EXE_STEM: &str = "test_game";

impl GameConfig {
    /// Resolve the per-recording capture mode, folding in the
    /// hook-required allowlist, the test_game carve-out, and the legacy
    /// `use_window_capture` fallback.
    ///
    /// `game_exe_stem` is the lowercase filename without extension (e.g.
    /// `"cs2"`), matching the style used by `constants::GAME_WHITELIST`.
    pub fn effective_capture_mode(&self, game_exe_stem: &str) -> EffectiveCaptureMode {
        // Minecraft (javaw) MUST use GameHook regardless of the persisted/bundled
        // config. Confirmed on a real RTX 4060 + HAGS rig (2026-05-31): Monitor
        // capture (DXGI Desktop Duplication) records a FULL-BLACK 5-minute video
        // with only the hardware cursor visible — Hardware-Accelerated GPU
        // Scheduling makes the duplication surface come back empty/black — and
        // WGC can't grab MC's OpenGL surface either. GameHook injects into the
        // game's GL swapchain directly (GPU→GPU), bypassing the desktop
        // compositor and HAGS entirely. This is a hard override so a stale
        // `capture_mode = Monitor` saved in %APPDATA% (e.g. from the v2.6.12
        // bundle) can never reintroduce the black-screen regression.
        if matches!(game_exe_stem, "javaw" | "minecraft") {
            return EffectiveCaptureMode::GameHook;
        }
        match self.capture_mode {
            CaptureMode::Monitor => EffectiveCaptureMode::Monitor,
            CaptureMode::GameHook => EffectiveCaptureMode::GameHook,
            CaptureMode::Wgc => EffectiveCaptureMode::Wgc,
            CaptureMode::Auto => {
                // test_game carve-out — keep CI on Monitor so the
                // existing E2E green-pixel assertions don't churn.
                if game_exe_stem == TEST_GAME_EXE_STEM {
                    return EffectiveCaptureMode::Monitor;
                }
                // Games that regressed under WGC and need the legacy
                // hook path. Start empty; grow as specific games
                // regress in production.
                if constants::KNOWN_HOOK_REQUIRED_GAMES
                    .iter()
                    .any(|g| *g == game_exe_stem)
                {
                    return EffectiveCaptureMode::GameHook;
                }
                // Legacy v2.5.8 escape hatch: if the user explicitly
                // flipped `use_window_capture = false` in their
                // persisted config, they asked for game-capture.
                // Honour that over the new WGC default so upgrades
                // don't surprise anyone who deliberately set it.
                if !self.use_window_capture {
                    return EffectiveCaptureMode::GameHook;
                }
                // Default for Win10 1903+: WGC. Safer than monitor
                // duplication on fullscreen-exclusive games, doesn't
                // require DLL injection like GameHook, and is the
                // Microsoft-blessed path for modern Windows capture.
                EffectiveCaptureMode::Wgc
            }
        }
    }
}

/// Start and stop recording are mapped to the same key (F9 toggle).
/// F9 matches the competitor's hotkey convention. F5 was previously
/// used but users reported it didn't work — F9 is less likely to
/// conflict with game keybinds.
fn default_start_key() -> String {
    "F9".to_string()
}
fn default_stop_key() -> String {
    "F9".to_string()
}
fn default_opacity() -> u8 {
    85
}
fn default_honk_volume() -> u8 {
    255
}
fn default_recording_location() -> std::path::PathBuf {
    // Use the system-standard local data directory (e.g. C:\Users\<user>\AppData\Local\GameData Recorder\recordings)
    // Falls back to ./data_dump/games if the system directory can't be determined
    dirs::data_local_dir()
        .map(|d| d.join("GameData Recorder").join("recordings"))
        .unwrap_or_else(|| std::path::PathBuf::from("./data_dump/games"))
}

/// Return the directory that `recording_location` is allowed to live under.
///
/// Any recording folder outside this tree is rejected. We intentionally
/// restrict to the current user's LocalAppData tree so that a malicious or
/// confused user cannot point the app at `C:\Windows\System32`, a SYSTEM-owned
/// directory, or another user's profile. The app's "safe cleanup" (remove
/// uploaded recordings) trusts that nothing in this tree is load-bearing
/// outside of our own recordings.
fn allowed_recording_root() -> Result<PathBuf> {
    dirs::data_local_dir().ok_or_else(|| eyre!("Could not resolve LocalAppData directory"))
}

/// Validate that the given path is a safe target for recordings.
///
/// Rejects:
/// * paths that are a symlink / reparse point at the leaf (anti-symlink-attack
///   — the attacker may have created the directory entry as a link into
///   System32 between `mkdir` and our cleanup pass)
/// * paths whose canonical form escapes `allowed_recording_root()`
/// * paths containing `..` after canonicalization (defence-in-depth)
///
/// The check is deliberately applied every time we load or set the value,
/// not just on write — a malicious installer could replace a legitimate
/// recording directory with a symlink between two launches.
pub fn validate_recording_location(path: &Path) -> Result<()> {
    // Reject reparse points / symlinks at the leaf. `symlink_metadata` does
    // NOT follow the link, so `file_type().is_symlink()` catches the case
    // where the entry itself is a link — which is the only case that can
    // redirect our writes somewhere unexpected.
    //
    // If the path does not yet exist that's fine (we create it on first use);
    // symlink_metadata returns NotFound, which is unambiguously safe.
    match path.symlink_metadata() {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(eyre!(
                    "Recording location {} is a symlink / reparse point, which is not allowed \
                     for safety reasons. Please choose a regular directory.",
                    path.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Not yet created — acceptable. The parent will be validated by
            // canonicalization below via the closest existing ancestor.
        }
        Err(e) => {
            return Err(eyre!(
                "Could not inspect recording location {}: {}",
                path.display(),
                e
            ));
        }
    }

    // Reject `..` in the raw components — defence-in-depth against path
    // traversal in sloppy callers.
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(eyre!(
            "Recording location {} contains '..' which is not allowed",
            path.display()
        ));
    }

    // Canonicalize via the closest existing ancestor (the leaf may not exist
    // yet). `dunce::canonicalize` strips the Windows verbatim prefix so the
    // comparison with `allowed_recording_root()` works.
    let canonical_under_check = canonicalize_existing_prefix(path)?;
    let canonical_root = canonicalize_existing_prefix(&allowed_recording_root()?)?;

    if !canonical_under_check.starts_with(&canonical_root) {
        return Err(eyre!(
            "Recording location {} must be inside {} for safety reasons. \
             Please choose a folder under your LocalAppData directory.",
            canonical_under_check.display(),
            canonical_root.display()
        ));
    }

    // Final belt-and-braces check: canonical form must not contain ParentDir
    // either (it shouldn't after canonicalization, but paranoid callers win
    // audits).
    if canonical_under_check
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(eyre!(
            "Canonical form of {} still contains '..'",
            canonical_under_check.display()
        ));
    }

    Ok(())
}

/// Canonicalize `path` if it exists, otherwise canonicalize the closest
/// existing ancestor and re-append the non-existent tail. This lets us
/// validate a directory we're about to create without first creating it.
fn canonicalize_existing_prefix(path: &Path) -> Result<PathBuf> {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cur: &Path = path;
    loop {
        match dunce::canonicalize(cur) {
            Ok(mut resolved) => {
                for segment in tail.iter().rev() {
                    resolved.push(segment);
                }
                return Ok(resolved);
            }
            Err(_) => {
                let Some(parent) = cur.parent() else {
                    return Err(eyre!(
                        "Could not canonicalize any ancestor of {}",
                        path.display()
                    ));
                };
                if let Some(name) = cur.file_name() {
                    tail.push(name);
                }
                cur = parent;
                if cur.as_os_str().is_empty() {
                    return Err(eyre!(
                        "Could not canonicalize any ancestor of {}",
                        path.display()
                    ));
                }
            }
        }
    }
}

// For some reason, previous electron configs saved hasConsented as a string instead of a boolean? So now we need a custom deserializer
// to take that into account for backwards compatibility
fn deserialize_string_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Bool(b) => Ok(b),
        serde_json::Value::String(s) => match s.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(Error::custom(format!("Invalid boolean string: {s}"))),
        },
        _ => Err(Error::custom("Expected boolean or string")),
    }
}

/// Same as `deserialize_string_bool` but wraps in `Option` so a missing field
/// in a legacy config round-trips to `None` rather than a default-false that
/// would overwrite the existing value on save.
fn deserialize_optional_string_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let v = Option::<serde_json::Value>::deserialize(deserializer)?;
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(b)) => Ok(Some(b)),
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            _ => Err(Error::custom(format!("Invalid boolean string: {s}"))),
        },
        _ => Err(Error::custom("Expected boolean or string")),
    }
}

/// Credentials.
///
/// In-memory and on-wire format: `api_key` is plaintext (the backend requires
/// plaintext over HTTPS). On-disk format: the plaintext is never written —
/// instead we serialize a DPAPI-encrypted blob (`api_key_encrypted`) using the
/// current-user entropy scope, so exfiltrating `config.json` from disk doesn't
/// leak the key. Legacy configs that contain `api_key` as plaintext are
/// transparently migrated on first read and re-written encrypted on next save.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub api_key: String,
    pub has_consented: bool,
    /// R46 (GDPR/CCPA): the consent **disclosure** version the user accepted.
    /// `None` means the user has never accepted any version. If this does not
    /// match the active `constants::CONSENT_DISCLOSURE_VERSION`, the ConsentView
    /// is shown again so the user must re-consent to the updated disclosure
    /// text.
    ///
    /// IMPORTANT: this is the disclosure-text version, NOT the app's
    /// `CARGO_PKG_VERSION`. Comparing against the disclosure version (rather
    /// than the binary version) means patch/minor app updates keep consent
    /// valid; only a disclosure-text change re-prompts. See
    /// `consent_disclosure_version` for the full rationale.
    ///
    /// This field gates every code path that installs a global input hook or
    /// opens a video/audio capture pipeline — see `Credentials::consent_status`
    /// and `input_capture::ConsentGuard`. Serialized as a semver string (e.g.
    /// `"2.6.0"`); `None` round-trips as `null` / missing.
    ///
    /// Credentials uses a manual Serialize/Deserialize via `CredentialsOnDisk`
    /// (the DPAPI wrap path), so `#[serde(default)]` would be a no-op here
    /// — the field is threaded through the shadow struct instead.
    pub consent_given_at_version: Option<Version>,
}

/// Raw wire/disk shape for `Credentials`. Both fields are optional so we can
/// round-trip old plaintext configs, new encrypted configs, and configs
/// written by a different-user DPAPI scope (decrypt will fail, we fall back
/// to empty and require re-login).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredentialsOnDisk {
    /// Legacy plaintext field. Present on configs written by pre-hardening
    /// builds. On read we decrypt-roundtrip and drop it from the next save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    /// DPAPI-protected API key bytes. Base64-encoded in JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key_encrypted: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_bool")]
    has_consented: Option<bool>,
    /// R46 consent version — semver string of the binary the user accepted.
    /// `None` means never accepted or stored under an older schema. Bumped
    /// package versions invalidate stored consent and re-prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    consent_given_at_version: Option<Version>,
}

impl serde::Serialize for Credentials {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Only ever write the encrypted field to disk. If encryption fails
        // we refuse to persist the key rather than silently leak plaintext.
        let api_key_encrypted = if self.api_key.is_empty() {
            None
        } else {
            match dpapi_protect(self.api_key.as_bytes()) {
                Ok(bytes) => Some(base64_encode(&bytes)),
                Err(e) => {
                    tracing::error!(
                        error = ?e,
                        "DPAPI encrypt failed; dropping api_key from serialized config \
                         to avoid leaking plaintext. User will need to re-login."
                    );
                    None
                }
            }
        };

        CredentialsOnDisk {
            api_key: None,
            api_key_encrypted,
            has_consented: Some(self.has_consented),
            consent_given_at_version: self.consent_given_at_version.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Credentials {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = CredentialsOnDisk::deserialize(deserializer)?;

        // Prefer encrypted; fall back to plaintext for legacy configs.
        let api_key = match (raw.api_key_encrypted, raw.api_key) {
            (Some(encoded), _) => match base64_decode(&encoded) {
                Ok(bytes) => match dpapi_unprotect(&bytes) {
                    Ok(plain) => String::from_utf8(plain).unwrap_or_else(|e| {
                        tracing::warn!(
                            error = ?e,
                            "Decrypted api_key was not valid UTF-8, treating as missing"
                        );
                        String::new()
                    }),
                    Err(e) => {
                        tracing::warn!(
                            error = ?e,
                            "DPAPI unprotect failed (different user or corrupted blob); \
                             user will need to re-login"
                        );
                        String::new()
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        "api_key_encrypted field is not valid base64, treating as missing"
                    );
                    String::new()
                }
            },
            (None, Some(plain)) => {
                // Legacy migration path: we decrypt-roundtrip once here (no-op
                // because the value is already plaintext) and on the next
                // Config::save() the on-disk `api_key` field will be dropped
                // and only `api_key_encrypted` written. This matches the
                // Gate C requirement: "decrypt-roundtrip once then remove
                // the plaintext field."
                if !plain.is_empty() {
                    tracing::info!(
                        "Migrating legacy plaintext api_key to DPAPI-encrypted storage on next save"
                    );
                }
                plain
            }
            (None, None) => String::new(),
        };

        Ok(Credentials {
            api_key,
            has_consented: raw.has_consented.unwrap_or(false),
            consent_given_at_version: raw.consent_given_at_version,
        })
    }
}

impl Credentials {
    pub fn logout(&mut self) {
        self.api_key = String::new();
        self.has_consented = false;
        self.consent_given_at_version = None;
    }

    /// Validate the API key format.
    /// Returns an error if the API key is non-empty and doesn't match the expected format.
    pub fn validate(&self) -> Result<(), String> {
        if !self.api_key.is_empty() {
            // Basic validation: API key should be at least 10 characters
            if self.api_key.len() < 10 {
                return Err("API key is too short (minimum 10 characters)".to_string());
            }
            // API key should only contain alphanumeric characters, underscores, and hyphens
            if !self
                .api_key
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                return Err("API key contains invalid characters".to_string());
            }
        }
        Ok(())
    }

    /// Compute the consent status against the active consent **disclosure**
    /// version.
    ///
    /// `disclosure_version` is the version of the consent disclosure *text*
    /// (see [`consent_disclosure_version`] /
    /// [`constants::CONSENT_DISCLOSURE_VERSION`]), NOT the app's
    /// `CARGO_PKG_VERSION`. Returns `Granted` iff the stored
    /// `consent_given_at_version` equals `disclosure_version`, `NotGranted` if
    /// no version has ever been accepted, and `VersionMismatch` if the user
    /// accepted a different (older) disclosure that has since changed.
    ///
    /// Because the disclosure version only moves when the disclosure text
    /// changes, patch/minor app updates no longer invalidate stored consent.
    pub fn consent_status(&self, disclosure_version: &Version) -> ConsentStatus {
        match &self.consent_given_at_version {
            None => ConsentStatus::NotGranted,
            Some(v) if v == disclosure_version => ConsentStatus::Granted,
            Some(_) => ConsentStatus::VersionMismatch,
        }
    }

    /// Record that the user has accepted the consent disclosure at the given
    /// version. The UI calls this when the user clicks "Accept", passing the
    /// active disclosure version (see [`consent_disclosure_version`]) so the
    /// stored value matches what the gate later compares against.
    pub fn record_consent(&mut self, disclosure_version: Version) {
        self.has_consented = true;
        self.consent_given_at_version = Some(disclosure_version);
    }
}

/// Parse the compile-time `CARGO_PKG_VERSION` as semver.
///
/// Panics only if the Cargo.toml version literal is malformed — which would
/// be caught at build time. Callers can treat this as infallible at runtime.
///
/// NOTE: This is the *application* version. It is intentionally NOT used to
/// gate consent anymore — see [`consent_disclosure_version`] for why. Keeping
/// it because other call sites (telemetry, update checks) still want the live
/// binary version.
pub fn current_pkg_version() -> Version {
    // `env!` is compile-time; the value comes straight from Cargo.toml.
    Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("CARGO_PKG_VERSION must be valid semver; this is a build-time contract")
}

/// The version of the **consent disclosure text** the consent gate keys on.
///
/// R46 fix: the gate previously compared stored consent against
/// `CARGO_PKG_VERSION`, so EVERY app version bump silently invalidated consent
/// and blocked auto-recording (a tester hit exactly this on the 2.6.0 -> 2.6.3
/// bumps: the recorder booted but never recorded). We now compare against the
/// disclosure-text version in [`constants::CONSENT_DISCLOSURE_VERSION`], which
/// is bumped only when the disclosure itself changes. Patch/minor app updates
/// therefore keep consent valid; only a disclosure change re-prompts.
///
/// Panics only if the constant is malformed — a build-time contract, exactly
/// like [`current_pkg_version`].
pub fn consent_disclosure_version() -> Version {
    Version::parse(constants::CONSENT_DISCLOSURE_VERSION).expect(
        "constants::CONSENT_DISCLOSURE_VERSION must be valid semver; this is a build-time contract",
    )
}

/// Build a [`ConsentGuard`] from the current config and the active consent
/// disclosure version.
///
/// This is the single entry point for every recording path that needs to
/// verify consent — input capture, OBS recorder, etc.
///
/// The comparison is against [`consent_disclosure_version`] (the disclosure
/// text version), NOT [`current_pkg_version`] (the app version), so a patch or
/// minor app update keeps prior consent valid. The legal gate is preserved:
/// the user must still have consented at the current disclosure version.
///
/// CI mode (see [`ci_mode`]) short-circuits to a session-only granted guard
/// without consulting the on-disk config. This is a test-scaffolding bypass:
/// it never persists `has_consented` to disk, so the next non-CI launch still
/// requires a real user click on the ConsentView.
pub fn consent_guard_from_config(config: &Config) -> ConsentGuard {
    if ci_mode() {
        return ConsentGuard::granted();
    }
    let disclosure = consent_disclosure_version();
    ConsentGuard::new(config.credentials.consent_status(&disclosure))
}

/// Returns `true` when the recorder is running under the automated CI test
/// harness (`run_ci.ps1`).
///
/// Activated by setting the environment variable `GAMEDATA_CI_MODE=1` before
/// launching the binary. The value is sampled once at first call and cached
/// in a `OnceLock` so subsequent reads are branch-prediction-friendly and
/// agree with each other for the lifetime of the process.
///
/// When active, the binary:
/// * auto-grants consent in-memory only (no disk write)
/// * treats any foreground window with a non-null HWND as a recordable game,
///   bypassing `GAME_WHITELIST` and `is_process_game_shaped`
/// * if `GAMEDATA_OUTPUT_DIR` is also set, redirects recordings there
///   instead of `%LocalAppData%\GameData Recorder\recordings`
///
/// Production builds with neither variable set behave exactly as before.
pub fn ci_mode() -> bool {
    use std::sync::OnceLock;
    static CI_MODE: OnceLock<bool> = OnceLock::new();
    *CI_MODE.get_or_init(|| {
        // F8 fix: the original match only accepted `"1"|"true"|"TRUE"`,
        // which silently rejected common truthy values like `"yes"`,
        // `"on"`, `"True"`, or `"YES"`. Accept them case-insensitively so
        // the env-var contract matches what operators expect.
        match std::env::var("GAMEDATA_CI_MODE").ok().as_deref() {
            Some(v)
                if v.eq_ignore_ascii_case("1")
                    || v.eq_ignore_ascii_case("true")
                    || v.eq_ignore_ascii_case("yes")
                    || v.eq_ignore_ascii_case("on") =>
            {
                true
            }
            _ => false,
        }
    })
}

/// If `GAMEDATA_OUTPUT_DIR` is set (and CI mode is active), return its value
/// as the recording root. Otherwise return `None` and the caller should fall
/// back to `Preferences::recording_location`.
///
/// The value is sampled and validated once on first call. We deliberately
/// SKIP `validate_recording_location` here because the CI harness writes to
/// `<repo>\ci_output`, which is outside the user's `LocalAppData` tree and
/// would be rejected by the normal symlink/path-escape guard. The CI mode
/// gate (env-var presence) is the trust boundary for this override.
pub fn ci_output_dir_override() -> Option<PathBuf> {
    use std::sync::OnceLock;
    static OVERRIDE: OnceLock<Option<PathBuf>> = OnceLock::new();
    OVERRIDE
        .get_or_init(|| {
            if !ci_mode() {
                return None;
            }
            std::env::var_os("GAMEDATA_OUTPUT_DIR").map(PathBuf::from)
        })
        .clone()
}

// ---------------------------------------------------------------------------
// DPAPI helpers
//
// On Windows we wrap the key with `CryptProtectData` using the current-user
// entropy scope — bound to the user's logon credentials, so the blob is
// useless to another user (including SYSTEM) or on another machine.
//
// On non-Windows (dev/test builds on mac/linux) we fall back to storing the
// bytes as-is. Production builds are `target_os = "windows"` only, so this
// is purely for `cargo check` / cross-platform test development.
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn dpapi_protect(plaintext: &[u8]) -> Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData};
    use windows::core::PCWSTR;

    // SAFETY: CryptProtectData is thread-safe; we allocate a fresh input blob
    // and let Windows write the output blob into its own allocation which we
    // free via LocalFree. Lifetimes of input pointers are bounded by the
    // duration of the FFI call.
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len() as u32,
            pbData: plaintext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        CryptProtectData(
            &input as *const _,
            PCWSTR::null(), // description
            None,           // optional entropy — current-user scope is sufficient
            None,           // reserved
            None,           // prompt struct
            0,              // flags (no UI, current user)
            &mut output as *mut _,
        )
        .map_err(|e| eyre!("CryptProtectData failed: {e}"))?;

        if output.pbData.is_null() || output.cbData == 0 {
            return Err(eyre!("CryptProtectData returned empty blob"));
        }

        // Copy out of the Windows-owned allocation before freeing it.
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let owned = slice.to_vec();

        // LocalFree returns an HLOCAL on failure; we ignore it — a leak on
        // the error path is preferable to a panic.
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Ok(owned)
    }
}

#[cfg(windows)]
fn dpapi_unprotect(ciphertext: &[u8]) -> Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};

    // SAFETY: symmetric to dpapi_protect above.
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: ciphertext.len() as u32,
            pbData: ciphertext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        CryptUnprotectData(
            &input as *const _,
            None, // ppszDataDescr
            None, // pOptionalEntropy
            None, // pvReserved
            None, // pPromptStruct
            0,    // flags
            &mut output as *mut _,
        )
        .map_err(|e| eyre!("CryptUnprotectData failed: {e}"))?;

        if output.pbData.is_null() {
            return Err(eyre!("CryptUnprotectData returned null blob"));
        }

        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let owned = slice.to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Ok(owned)
    }
}

// Non-Windows fallback for cross-platform builds (tests, `cargo check` on dev
// machines). This is NEVER compiled into the shipped Windows binary because
// main.rs gates `windows_subsystem = "windows"` on target_os = "windows".
// If someone actually runs a non-Windows build, the key is stored unwrapped;
// that's no worse than the pre-hardening baseline and is clearly marked.
#[cfg(not(windows))]
fn dpapi_protect(plaintext: &[u8]) -> Result<Vec<u8>> {
    Ok(plaintext.to_vec())
}

#[cfg(not(windows))]
fn dpapi_unprotect(ciphertext: &[u8]) -> Result<Vec<u8>> {
    Ok(ciphertext.to_vec())
}

// ---------------------------------------------------------------------------
// Base64 (tiny, dependency-free). We avoid pulling in the `base64` crate for
// one small call site — this is standard RFC-4648 alphabet, no URL variant.
// ---------------------------------------------------------------------------

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHA[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(eyre!("base64 length not a multiple of 4"));
    }
    let val = |c: u8| -> Result<u32> {
        Ok(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            b'=' => 0,
            _ => return Err(eyre!("invalid base64 char")),
        })
    };
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let n = (val(chunk[0])? << 18)
            | (val(chunk[1])? << 12)
            | (val(chunk[2])? << 6)
            | val(chunk[3])?;
        out.push(((n >> 16) & 0xFF) as u8);
        if chunk[2] != b'=' {
            out.push(((n >> 8) & 0xFF) as u8);
        }
        if chunk[3] != b'=' {
            out.push((n & 0xFF) as u8);
        }
    }
    Ok(out)
}

/// The directory in which all persistent config data should be stored.
pub fn get_persistent_dir() -> Result<PathBuf> {
    tracing::debug!("get_persistent_dir() called");
    let dir = dirs::data_dir()
        .ok_or_else(|| eyre!("Could not find user data directory"))?
        .join("GameData Recorder");
    fs::create_dir_all(&dir)?;
    tracing::debug!("Persistent dir: {:?}", dir);
    Ok(dir)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Config {
    #[serde(default)]
    pub credentials: Credentials,
    #[serde(default)]
    pub preferences: Preferences,
    #[serde(default)]
    pub output_format: Option<OutputFormat>,
}

/// Output format configuration for LEM support
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputFormat {
    /// Output format version
    pub version: OutputFormatVersion,
    /// Enable LEM format directory structure
    pub use_lem_format: bool,
    /// Record depth video (LEM only)
    pub record_depth: bool,
    /// Record game states (LEM only)
    pub record_states: bool,
    /// Record game events (LEM only)
    pub record_events: bool,
}

impl Default for OutputFormat {
    fn default() -> Self {
        Self {
            version: OutputFormatVersion::Legacy,
            use_lem_format: false,
            record_depth: false,
            record_states: false,
            record_events: false,
        }
    }
}

/// Output format version
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OutputFormatVersion {
    #[serde(rename = "legacy")]
    Legacy,
    #[serde(rename = "lem_v1")]
    LemV1,
}

impl Default for OutputFormatVersion {
    fn default() -> Self {
        OutputFormatVersion::Legacy
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        tracing::debug!("Config::load() called");
        let config_path = match (Self::get_path(), Self::get_legacy_path()) {
            (Ok(path), _) if path.exists() => {
                tracing::info!("Loading from standard config path");
                tracing::debug!("Config path: {:?}", path);
                path
            }
            (_, Ok(path)) if path.exists() => {
                tracing::info!("Loading from legacy config path");
                tracing::debug!("Config path: {:?}", path);
                path
            }
            _ => {
                tracing::warn!("No config file found, using defaults");
                return Ok(Self::default());
            }
        };

        tracing::debug!("Reading config file");
        let contents = match fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    "Failed to read config file at {}: {e}",
                    config_path.display()
                );
                tracing::warn!("Using default configuration");
                return Ok(Self::default());
            }
        };
        tracing::debug!("Parsing config file");
        let mut config = match serde_json::from_str::<Config>(&contents) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    "Config file at {} is corrupted or invalid JSON: {e}",
                    config_path.display()
                );
                tracing::warn!(
                    "Using default configuration. The corrupted file will be overwritten on next save."
                );
                return Ok(Self::default());
            }
        };

        // Migrate hotkeys: F5 was the old default but users reported it
        // didn't work. Upgrade to F9 (matches competitor convention).
        if config.preferences.start_recording_key.is_empty()
            || config.preferences.start_recording_key == "F5"
        {
            config.preferences.start_recording_key = default_start_key();
        }
        if config.preferences.stop_recording_key.is_empty()
            || config.preferences.stop_recording_key == "F5"
        {
            config.preferences.stop_recording_key = default_stop_key();
        }

        // Note: v2.5.4 migration removed in v2.5.8+ since screen capture is now the default.
        // The find_window_for_pid fix from v2.5.4 remains active.

        // Security: validate the recording_location loaded from disk against
        // the symlink / path-escape guard. If the stored value is unsafe
        // (e.g. an attacker replaced it with a reparse point, or the user
        // hand-edited config.json to point at System32), we reset to the
        // default and warn. We do NOT refuse to start — the recorder is
        // usable with the default path.
        if let Err(e) = validate_recording_location(&config.preferences.recording_location) {
            tracing::warn!(
                error = ?e,
                rejected = %config.preferences.recording_location.display(),
                "Stored recording_location failed safety validation; \
                 falling back to default. This protects against symlink-based \
                 cleanup attacks and config tampering."
            );
            config.preferences.recording_location = default_recording_location();
            if let Err(e) = config.save() {
                tracing::warn!(e = ?e, "Failed to persist recording_location reset");
            }
        }

        // v2.5.4: NVENC auto-selection. If the user has an NVIDIA GPU but
        // encoder is still on X264, flip to NvEnc. Discrete NVIDIA GPUs can
        // encode essentially for free; x264 software encoding chews CPU
        // (we saw 1 FPS effective on an AMD iGPU, and NVIDIA users often
        // end up here by default too).
        //
        // v2.6.0: extended to AMD (AMF) and Intel (QuickSync). AMD/Intel
        // users were stuck on software X264 — maxing the CPU and lagging the
        // whole system (~1 FPS effective on an AMD Radeon 780M). We pick the
        // H.264 hardware variant for each vendor to mirror the existing
        // NVIDIA choice (NvEnc, not NvEncHevc) — lowest-risk codec parity.
        if matches!(
            config.preferences.encoder.encoder,
            constants::encoding::VideoEncoderType::X264
        ) {
            match detect_gpu_vendor() {
                Ok(Some(GpuVendor::Nvidia)) => {
                    tracing::info!("NVIDIA GPU detected — upgrading encoder X264 -> NvEnc");
                    config.preferences.encoder.encoder =
                        constants::encoding::VideoEncoderType::NvEnc;
                    if let Err(e) = config.save() {
                        tracing::warn!(e=?e, "Failed to persist NvEnc migration");
                    }
                }
                Ok(Some(GpuVendor::Amd)) => {
                    tracing::info!("AMD GPU detected — upgrading encoder X264 -> Amf");
                    config.preferences.encoder.encoder = constants::encoding::VideoEncoderType::Amf;
                    if let Err(e) = config.save() {
                        tracing::warn!(e=?e, "Failed to persist Amf migration");
                    }
                }
                Ok(Some(GpuVendor::Intel)) => {
                    tracing::info!("Intel GPU detected — upgrading encoder X264 -> Qsv");
                    config.preferences.encoder.encoder = constants::encoding::VideoEncoderType::Qsv;
                    if let Err(e) = config.save() {
                        tracing::warn!(e=?e, "Failed to persist Qsv migration");
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::debug!(e=?e, "GPU vendor probe failed, keeping X264");
                }
            }
        }

        tracing::debug!("Config::load() complete");
        Ok(config)
    }

    fn get_legacy_path() -> Result<PathBuf> {
        // Get user data directory (equivalent to app.getPath("userData"))
        let user_data_dir = dirs::data_dir()
            .ok_or_else(|| eyre!("Could not find user data directory"))?
            .join("vg-control");

        Ok(user_data_dir.join("config.json"))
    }

    fn get_path() -> Result<PathBuf> {
        Ok(get_persistent_dir()?.join(constants::filename::persistent::CONFIG))
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::get_path()?;
        tracing::info!("Saving configs to {}", config_path.to_string_lossy());
        let json = serde_json::to_string_pretty(&self)?;
        // Atomic + fsync write: the helper writes <path>.tmp, calls
        // `File::sync_all()` so the bytes reach durable storage, then
        // renames into place and (on POSIX) syncs the containing directory.
        // Replaces the prior tmp+rename pair that omitted the fsync and
        // could leave a 0-byte config.json on power loss between write and
        // rename.
        if let Err(e) = crate::util::durable_write::write_atomic(&config_path, json.as_bytes()) {
            tracing::error!(
                "Atomic config write failed ({e}). Falling back to direct write to avoid \
                 losing the user's preferences entirely."
            );
            // Fallback: direct write (less safe but better than nothing) —
            // preserves v2.5.5 behaviour for systems where the atomic path
            // keeps failing (e.g. weird network shares that disallow rename).
            fs::write(&config_path, &json)?;
        }
        Ok(())
    }

    /// Check if LEM format is enabled
    pub fn is_lem_format(&self) -> bool {
        self.output_format
            .as_ref()
            .map(|f| f.use_lem_format)
            .unwrap_or(false)
    }

    /// Get output format version
    pub fn output_format_version(&self) -> OutputFormatVersion {
        self.output_format
            .as_ref()
            .map(|f| f.version)
            .unwrap_or(OutputFormatVersion::Legacy)
    }
}

/// Base struct containing common video encoder settings shared across all encoders
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct EncoderSettings {
    /// Encoder type
    pub encoder: VideoEncoderType,

    /// Encoder specific settings
    pub x264: ObsX264Settings,
    pub nvenc: FfmpegNvencSettings,
    pub qsv: ObsQsvSettings,
    pub amf: ObsAmfSettings,
}
impl Default for EncoderSettings {
    fn default() -> Self {
        Self {
            encoder: VideoEncoderType::X264,
            x264: Default::default(),
            nvenc: Default::default(),
            qsv: Default::default(),
            amf: Default::default(),
        }
    }
}
impl EncoderSettings {
    /// Apply encoder settings to ObsData
    pub fn apply_to_obs_data(
        &self,
        mut data: libobs_wrapper::data::ObsData,
    ) -> color_eyre::Result<libobs_wrapper::data::ObsData> {
        // Apply common settings shared by all encoders
        let mut updater = data.bulk_update();
        updater = updater
            .set_int("bitrate", constants::encoding::BITRATE)
            .set_string("rate_control", constants::encoding::RATE_CONTROL)
            .set_string("profile", constants::encoding::VIDEO_PROFILE)
            .set_int("bf", constants::encoding::B_FRAMES)
            .set_bool("psycho_aq", constants::encoding::PSYCHO_AQ)
            .set_bool("lookahead", constants::encoding::LOOKAHEAD);

        updater = match self.encoder {
            VideoEncoderType::X264 => self.x264.apply_to_data_updater(updater),
            VideoEncoderType::NvEncHevc | VideoEncoderType::NvEnc => {
                self.nvenc.apply_to_data_updater(updater)
            }
            VideoEncoderType::AmfHevc | VideoEncoderType::Amf => {
                self.amf.apply_to_data_updater(updater)
            }
            VideoEncoderType::QsvHevc | VideoEncoderType::Qsv => {
                self.qsv.apply_to_data_updater(updater)
            }
        };
        updater.update()?;

        Ok(data)
    }
}

/// OBS x264 (CPU) encoder specific settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ObsX264Settings {
    pub preset: String,
    pub tune: String,
}
impl Default for ObsX264Settings {
    fn default() -> Self {
        Self {
            preset: constants::encoding::X264_PRESETS[0].to_string(),
            tune: String::new(),
        }
    }
}
impl ObsX264Settings {
    fn apply_to_data_updater(
        &self,
        updater: libobs_wrapper::data::ObsDataUpdater,
    ) -> libobs_wrapper::data::ObsDataUpdater {
        updater
            .set_string("preset", self.preset.as_str())
            .set_string("tune", self.tune.as_str())
    }
}

/// NVENC default preset for "record while gaming".
///
/// We record at 1080p30 / ~10 Mbps WHILE the user plays the same game on the
/// same GPU, so the encoder must stay out of the game's way. NVENC presets run
/// `p1` (fastest, lowest GPU load) .. `p7` (slowest, max quality, highest GPU +
/// VRAM contention). The old default was `NVENC_PRESETS[0]` == `p7`, which
/// maximises contention — the opposite of what this workload wants. `p5` is the
/// balanced preset and is exactly OBS's own shipped default for the modern
/// `obs-nvenc` encoder (`obs_data_set_default_string(settings, "preset", "p5")`
/// in plugins/obs-nvenc/nvenc-properties.c), so it is a conservative, proven
/// choice rather than an aggressive one. We resolve it by value from the
/// validated `NVENC_PRESETS` list (never reordering that list — it also drives
/// the settings-UI dropdown order) and fall back to the list's first element if
/// `p5` is ever removed upstream.
const NVENC_BALANCED_PRESET: &str = "p5";

fn nvenc_default_preset() -> String {
    constants::encoding::NVENC_PRESETS
        .iter()
        .copied()
        .find(|p| *p == NVENC_BALANCED_PRESET)
        .unwrap_or(constants::encoding::NVENC_PRESETS[0])
        .to_string()
}

/// NVENC (NVIDIA GPU) encoder specific settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct FfmpegNvencSettings {
    pub preset2: String,
    pub tune: String,
}
impl Default for FfmpegNvencSettings {
    fn default() -> Self {
        Self {
            // `preset2` is the field/JSON name kept for config back-compat; the
            // value is the balanced `p5` preset (see `nvenc_default_preset`).
            preset2: nvenc_default_preset(),
            // `hq` (high quality) — kept as-is. Dropping the preset p7 -> p5 is
            // the load reduction we want; `hq` remains OBS's default tune and is
            // the safest choice without runtime A/B data. `ll`/`ull` (low
            // latency) are available in NVENC_TUNE_OPTIONS if a future,
            // GPU-tested change wants to lean further toward latency.
            tune: constants::encoding::NVENC_TUNE_OPTIONS[0].to_string(),
        }
    }
}
impl FfmpegNvencSettings {
    fn apply_to_data_updater(
        &self,
        updater: libobs_wrapper::data::ObsDataUpdater,
    ) -> libobs_wrapper::data::ObsDataUpdater {
        // Set BOTH `preset` and `preset2` to the same value. This recorder
        // instantiates the modern texture NVENC encoders (OBS_NVENC_H264_TEX /
        // OBS_NVENC_HEVC_TEX); that plugin reads the `preset` key
        // (plugins/obs-nvenc/nvenc-properties.c) and IGNORES `preset2`, while
        // the legacy ffmpeg-nvenc plugin reads `preset2`
        // (plugins/obs-ffmpeg/obs-ffmpeg-nvenc.c). Writing both means whichever
        // NVENC plugin OBS ends up using honours our preset; the unused key is
        // harmlessly ignored. Both keys accept the identical `p1`..`p7` enum.
        updater
            .set_string("preset", self.preset2.as_str())
            .set_string("preset2", self.preset2.as_str())
            .set_string("tune", self.tune.as_str())
    }
}

/// QuickSync default `target_usage` for "record while gaming".
///
/// QSV `target_usage` maps speed/quality (obs-qsv11.c `update_targetusage`):
/// `quality`/`veryslow` -> TU1 (slowest, most encoder passes), `balanced`/
/// `medium` -> TU4, `speed`/`veryfast` -> TU7 (fastest). Intel QSV usually runs
/// on the integrated GPU, which shares silicon and memory bandwidth with the
/// game's own rendering, so the old default `QSV_TARGET_USAGES[0]` == `quality`
/// (TU1) is the worst case for stutter. `balanced` (TU4) is OBS's own shipped
/// default (`obs_data_set_default_string(settings, "target_usage", "TU4")` in
/// obs-qsv11.c) — a conservative middle that frees iGPU headroom while keeping
/// quality fine for AI-world-model training video. Resolved by value from the
/// validated list (which also feeds the settings-UI dropdown, so it is NOT
/// reordered), falling back to the list head if `balanced` is removed upstream.
const QSV_BALANCED_TARGET_USAGE: &str = "balanced";

fn qsv_default_target_usage() -> String {
    constants::encoding::QSV_TARGET_USAGES
        .iter()
        .copied()
        .find(|t| *t == QSV_BALANCED_TARGET_USAGE)
        .unwrap_or(constants::encoding::QSV_TARGET_USAGES[0])
        .to_string()
}

/// QuickSync H.264 encoder specific settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ObsQsvSettings {
    pub target_usage: String,
}
impl Default for ObsQsvSettings {
    fn default() -> Self {
        Self {
            target_usage: qsv_default_target_usage(),
        }
    }
}
impl ObsQsvSettings {
    fn apply_to_data_updater(
        &self,
        updater: libobs_wrapper::data::ObsDataUpdater,
    ) -> libobs_wrapper::data::ObsDataUpdater {
        updater.set_string("target_usage", self.target_usage.as_str())
    }
}

/// AMD HW H.264 (AVC) encoder specific settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ObsAmfSettings {
    pub preset: String,
}
impl Default for ObsAmfSettings {
    fn default() -> Self {
        Self {
            preset: constants::encoding::AMF_PRESETS[0].to_string(),
        }
    }
}
impl ObsAmfSettings {
    fn apply_to_data_updater(
        &self,
        updater: libobs_wrapper::data::ObsDataUpdater,
    ) -> libobs_wrapper::data::ObsDataUpdater {
        updater.set_string("preset", self.preset.as_str())
    }
}

// ---------------------------------------------------------------------------
// Security hardening tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A path containing `..` must be rejected regardless of platform.
    #[test]
    fn validate_recording_location_rejects_parent_dir_components() {
        // Build a path with an explicit `..` component. We don't care whether
        // the path canonicalizes — the raw-component check must fire first.
        let p = PathBuf::from("some").join("..").join("elsewhere");
        let err = validate_recording_location(&p).expect_err("path with `..` must be rejected");
        assert!(
            format!("{err}").contains(".."),
            "error should mention `..`, got: {err}"
        );
    }

    /// A path outside LocalAppData must be rejected (e.g. System32).
    #[test]
    fn validate_recording_location_rejects_escape_from_local_appdata() {
        // Use the tempdir's canonical form, which on most CI is not under
        // LocalAppData, to stand in for an escape. We create the directory
        // so canonicalization succeeds; the guard should reject it based on
        // the allowed-root check.
        let tmp = TempDir::new().expect("tempdir");
        // If the tempdir happens to live under LocalAppData (rare, but
        // possible on CI), skip — the test's premise doesn't hold.
        if let Ok(root) = allowed_recording_root() {
            if let (Ok(t), Ok(r)) = (dunce::canonicalize(tmp.path()), dunce::canonicalize(&root)) {
                if t.starts_with(&r) {
                    eprintln!(
                        "test skipped: tempdir {} is under allowed root {}",
                        t.display(),
                        r.display()
                    );
                    return;
                }
            }
        } else {
            // Platform has no LocalAppData (unusual). Skip.
            return;
        }

        let err = validate_recording_location(tmp.path())
            .expect_err("path outside LocalAppData must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("LocalAppData") || msg.contains("inside"),
            "error should mention allowed root, got: {msg}"
        );
    }

    /// A path under LocalAppData must be accepted.
    #[test]
    fn validate_recording_location_accepts_path_under_local_appdata() {
        let Ok(root) = allowed_recording_root() else {
            eprintln!("test skipped: no LocalAppData on this platform");
            return;
        };
        // Use a unique subfolder that may not exist yet — the guard must
        // handle non-existent leaves by canonicalizing the existing prefix.
        let candidate = root
            .join("GameData Recorder")
            .join("test-validate-recording-location-accept");
        // Clean up if left over from a prior run.
        let _ = std::fs::remove_dir_all(&candidate);

        validate_recording_location(&candidate)
            .expect("path under LocalAppData should be accepted");
    }

    /// A leaf symlink must be rejected (core anti-symlink-attack test).
    #[cfg(unix)]
    #[test]
    fn validate_recording_location_rejects_symlink_leaf() {
        use std::os::unix::fs::symlink;

        // Set up under the allowed root so the ONLY failing check is the
        // symlink leaf check. Fall back to tempdir if LocalAppData doesn't
        // exist; the error will still be non-Ok (outside-root) which is
        // also acceptable.
        let root = allowed_recording_root().unwrap_or_else(|_| std::env::temp_dir());
        let base = root.join("GameData Recorder").join("test-symlink-guard");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("create base");

        let target = base.join("real-target");
        std::fs::create_dir_all(&target).expect("create target");

        let link = base.join("link-to-target");
        let _ = std::fs::remove_file(&link);
        symlink(&target, &link).expect("create symlink");

        let err = validate_recording_location(&link).expect_err("symlink leaf must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("symlink") || msg.contains("reparse"),
            "error should mention symlink, got: {msg}"
        );

        // Cleanup
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// DPAPI round-trip: encrypt then decrypt yields the original bytes.
    /// Runs only on Windows; on other platforms the helpers are identity
    /// and wouldn't exercise anything meaningful.
    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip() {
        let original = b"sk_test_very_secret_api_key_12345";
        let encrypted = dpapi_protect(original).expect("protect");
        assert_ne!(
            encrypted.as_slice(),
            original,
            "encrypted output must differ from plaintext"
        );
        let decrypted = dpapi_unprotect(&encrypted).expect("unprotect");
        assert_eq!(
            decrypted.as_slice(),
            original,
            "round-trip must recover plaintext"
        );
    }

    /// DPAPI round-trip through the serde boundary: serializing writes only
    /// `apiKeyEncrypted`, deserializing recovers the plaintext. Windows only.
    #[cfg(windows)]
    #[test]
    fn credentials_serde_round_trip_encrypted() {
        let creds = Credentials {
            api_key: "sk_test_abcdef123456".to_string(),
            has_consented: true,
        };
        let json = serde_json::to_string(&creds).expect("serialize");
        assert!(
            !json.contains("sk_test_abcdef123456"),
            "serialized form must NOT contain plaintext api_key: {json}"
        );
        assert!(
            json.contains("apiKeyEncrypted"),
            "serialized form must contain apiKeyEncrypted: {json}"
        );

        let restored: Credentials = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.api_key, "sk_test_abcdef123456");
        assert!(restored.has_consented);
    }

    /// Legacy migration: a config that only has the plaintext `apiKey` field
    /// must be accepted on first read, and subsequent serialize must drop
    /// the plaintext field and emit only the encrypted one.
    #[cfg(windows)]
    #[test]
    fn credentials_legacy_plaintext_migrates_on_roundtrip() {
        let legacy = r#"{"apiKey":"sk_legacy_12345678","hasConsented":true}"#;
        let creds: Credentials = serde_json::from_str(legacy).expect("read legacy");
        assert_eq!(creds.api_key, "sk_legacy_12345678");
        assert!(creds.has_consented);

        let rewritten = serde_json::to_string(&creds).expect("re-serialize");
        assert!(
            !rewritten.contains("sk_legacy_12345678"),
            "rewritten form must not leak legacy plaintext: {rewritten}"
        );
        assert!(
            !rewritten.contains("\"apiKey\""),
            "rewritten form must drop the legacy `apiKey` field: {rewritten}"
        );
        assert!(
            rewritten.contains("apiKeyEncrypted"),
            "rewritten form must write apiKeyEncrypted: {rewritten}"
        );
    }

    /// Base64 round-trip for the dependency-free encoder.
    #[test]
    fn base64_round_trip() {
        for input in [
            &b""[..],
            &b"f"[..],
            &b"fo"[..],
            &b"foo"[..],
            &b"foobar"[..],
            &[0u8, 1, 2, 3, 4, 255, 254, 253][..],
        ] {
            let enc = base64_encode(input);
            let dec = base64_decode(&enc).expect("decode");
            assert_eq!(dec.as_slice(), input, "round-trip for {input:?}");
        }
    }

    // -----------------------------------------------------------------------
    // Encoder fallback-chain tests (multi-GPU compatibility hardening)
    //
    // These pin the guaranteed decision tree of `select_best_available_encoder`
    // so the recorder can never be handed an encoder OBS didn't actually probe
    // as available, and always degrades NVENC > AMF > QSV > x264.
    // -----------------------------------------------------------------------
    use constants::encoding::VideoEncoderType as VE;

    #[test]
    fn encoder_keeps_request_when_available() {
        // The common path: the requested encoder is present, so it is kept
        // verbatim. This is what preserves the proven AMD-AMF behaviour.
        let available = [VE::AmfHevc, VE::Amf, VE::X264];
        assert_eq!(
            select_best_available_encoder(VE::Amf, &available),
            VE::Amf,
            "an available requested encoder must be kept unchanged"
        );
        assert_eq!(
            select_best_available_encoder(VE::AmfHevc, &available),
            VE::AmfHevc
        );
    }

    #[test]
    fn encoder_hevc_steps_down_to_same_vendor_h264() {
        // HEVC requested, only the H.264 sibling registered → smallest possible
        // degradation is the same-vendor H.264 encoder, NOT a vendor switch.
        let available = [VE::NvEnc, VE::X264];
        assert_eq!(
            select_best_available_encoder(VE::NvEncHevc, &available),
            VE::NvEnc,
            "HEVC must step down to same-vendor H.264 before changing vendor"
        );
    }

    #[test]
    fn encoder_falls_through_to_best_available_hardware() {
        // Optimus laptop scenario: config picked NVENC (NVIDIA adapter present)
        // but NVENC never registered (old driver / session cap / parked dGPU).
        // Intel QSV did register → we must use QSV, not collapse to software.
        let available = [VE::QsvHevc, VE::Qsv, VE::X264];
        assert_eq!(
            select_best_available_encoder(VE::NvEnc, &available),
            VE::QsvHevc,
            "must pick the best AVAILABLE hardware encoder when the requested one is absent"
        );
        // And the HEVC-NVENC request degrades the same way.
        assert_eq!(
            select_best_available_encoder(VE::NvEncHevc, &available),
            VE::QsvHevc
        );
    }

    #[test]
    fn encoder_prefers_nvenc_over_amf_over_qsv() {
        // When several hardware encoders are simultaneously available and the
        // request matches none of them, the priority order must hold.
        let all_hw = [
            VE::Qsv,
            VE::QsvHevc,
            VE::Amf,
            VE::AmfHevc,
            VE::NvEnc,
            VE::NvEncHevc,
            VE::X264,
        ];
        // Request something not in the list to force the preference walk.
        // (Construct an impossible request by asking for a HW encoder we then
        // exclude.) Easiest: request X264's "upgrade" by asking for QSV when
        // everything is present — QSV IS present so it would be kept; instead
        // exclude the requested one explicitly.
        let without_request: Vec<VE> = all_hw.iter().copied().filter(|e| *e != VE::Qsv).collect();
        assert_eq!(
            select_best_available_encoder(VE::Qsv, &without_request),
            VE::NvEncHevc,
            "NVENC (HEVC) must win when present alongside AMF and QSV"
        );

        let nvenc_absent: Vec<VE> = without_request
            .iter()
            .copied()
            .filter(|e| *e != VE::NvEncHevc && *e != VE::NvEnc)
            .collect();
        assert_eq!(
            select_best_available_encoder(VE::Qsv, &nvenc_absent),
            VE::AmfHevc,
            "AMF (HEVC) must win over QSV when NVENC is absent"
        );
    }

    #[test]
    fn encoder_falls_back_to_x264_when_no_hardware() {
        // No-discrete-GPU / iGPU-without-encoder machine: only software x264 is
        // available. The requested hardware encoder must collapse to x264.
        let available = [VE::X264];
        assert_eq!(
            select_best_available_encoder(VE::NvEncHevc, &available),
            VE::X264,
            "with no hardware encoder available, must fall back to software x264"
        );
        assert_eq!(select_best_available_encoder(VE::Amf, &available), VE::X264);
    }

    #[test]
    fn encoder_x264_request_upgrades_to_available_hardware() {
        // The RTX 4060 tester bug, pinned. A stored config of x264 must NOT be
        // honoured when a real hardware encoder registered with OBS: x264 is
        // always in `available` (it is the always-constructible software
        // encoder), so honouring it is precisely how an NVENC-capable machine
        // ended up CPU-bound on software at ~2 FPS. With NVENC present we must
        // upgrade to it, deterministically preferring HEVC NVENC.
        let available = [VE::NvEncHevc, VE::NvEnc, VE::X264];
        assert_eq!(
            select_best_available_encoder(VE::X264, &available),
            VE::NvEncHevc,
            "a stale x264 config must be overridden to the best available \
             hardware encoder (NVENC) — this is the tester's choppy-video fix"
        );

        // AMD-only machine: x264 config must upgrade to AMF, not stay software.
        let amd = [VE::AmfHevc, VE::Amf, VE::X264];
        assert_eq!(select_best_available_encoder(VE::X264, &amd), VE::AmfHevc);

        // Intel-only machine: x264 config must upgrade to QSV.
        let intel = [VE::QsvHevc, VE::Qsv, VE::X264];
        assert_eq!(select_best_available_encoder(VE::X264, &intel), VE::QsvHevc);
    }

    #[test]
    fn encoder_x264_request_stays_x264_without_hardware() {
        // The only case x264 is honoured: no hardware encoder registered at
        // all. x264 is then the correct (and only) choice, and the downstream
        // 720p software cap protects the CPU.
        let software_only = [VE::X264];
        assert_eq!(
            select_best_available_encoder(VE::X264, &software_only),
            VE::X264,
            "x264 must remain x264 when no hardware encoder is available"
        );
    }

    #[test]
    fn encoder_empty_probe_yields_x264() {
        // Degenerate case: the probe returned nothing usable. We still hand the
        // caller x264 (the universal software encoder) rather than something
        // unconstructible or leaving the choice unset.
        assert_eq!(
            select_best_available_encoder(VE::NvEnc, &[]),
            VE::X264,
            "an empty availability list must still resolve to x264"
        );
    }

    // -----------------------------------------------------------------------
    // Runtime construct-time fallback chain (a *listed* encoder can still fail
    // to construct: saturated NVENC session cap, stale driver, parked Optimus
    // dGPU, blocked AMD obs-amf-test). These pin the degradation ORDER, which
    // is the only part testable without a live OBS context.
    // -----------------------------------------------------------------------

    #[test]
    fn fallback_chain_tries_primary_first_then_x264_terminal() {
        // The common multi-GPU machine: NVENC-HEVC primary, with its H.264
        // sibling also listed. The chain must lead with the primary, walk the
        // remaining available hardware in preference order, and ALWAYS end at
        // x264 so a construction storm can still degrade to software.
        let available = [VE::NvEncHevc, VE::NvEnc, VE::X264];
        assert_eq!(
            runtime_fallback_chain(VE::NvEncHevc, &available),
            vec![VE::NvEncHevc, VE::NvEnc, VE::X264],
            "primary first, then next-best hardware, then x264 terminal"
        );
    }

    #[test]
    fn fallback_chain_excludes_primary_from_hardware_walk() {
        // Primary must never appear twice. Here AMF is primary; the hardware
        // walk skips it and offers QSV next, then x264.
        let available = [VE::AmfHevc, VE::Amf, VE::QsvHevc, VE::Qsv, VE::X264];
        assert_eq!(
            runtime_fallback_chain(VE::Amf, &available),
            vec![VE::Amf, VE::AmfHevc, VE::QsvHevc, VE::Qsv, VE::X264],
            "primary (AMF) appears once; rest follow HW preference order"
        );
    }

    #[test]
    fn fallback_chain_walks_full_preference_order() {
        // Everything is available and the primary is the lowest-priority QSV:
        // the remaining hardware must be walked NVENC>AMF>QSV (minus the
        // primary), terminating in x264.
        let all = [
            VE::NvEncHevc,
            VE::NvEnc,
            VE::AmfHevc,
            VE::Amf,
            VE::QsvHevc,
            VE::Qsv,
            VE::X264,
        ];
        assert_eq!(
            runtime_fallback_chain(VE::Qsv, &all),
            vec![
                VE::Qsv,
                VE::NvEncHevc,
                VE::NvEnc,
                VE::AmfHevc,
                VE::Amf,
                VE::QsvHevc,
                VE::X264,
            ],
        );
    }

    #[test]
    fn fallback_chain_x264_primary_is_not_duplicated() {
        // When x264 itself is the primary (no hardware registered), the chain
        // is just [x264] — the terminal append is suppressed so it isn't listed
        // twice.
        assert_eq!(
            runtime_fallback_chain(VE::X264, &[VE::X264]),
            vec![VE::X264],
            "x264 primary must not duplicate the terminal x264"
        );
    }

    #[test]
    fn fallback_chain_appends_x264_even_when_probe_omitted_it() {
        // Defense-in-depth: even a degenerate `available` that somehow lacks
        // x264 must still terminate in x264 (the always-constructible software
        // encoder), so the recorder can never be left with "no encoder".
        assert_eq!(
            runtime_fallback_chain(VE::NvEncHevc, &[VE::NvEncHevc]),
            vec![VE::NvEncHevc, VE::X264],
            "x264 terminal is appended regardless of the probed list"
        );
    }

    // -----------------------------------------------------------------------
    // Encoder-SETTINGS defaults (record-while-gaming, low GPU contention).
    //
    // These pin the per-encoder preset / rate-control defaults so we record at
    // 1080p30 ~10 Mbps without the encoder fighting the game for GPU/VRAM.
    // They assert pure `Default` values only — `apply_to_data_updater` needs a
    // live OBS runtime (Windows/GPU) and is not unit-testable here.
    //
    // Every asserted value is also checked to be a member of the validated,
    // OBS-recognised constant list for that key, so a typo can never ship a
    // string the encoder would silently ignore.
    // -----------------------------------------------------------------------

    #[test]
    fn nvenc_default_preset_is_balanced_not_max_quality() {
        let s = FfmpegNvencSettings::default();
        // Balanced `p5` (OBS's own default), NOT the slowest/max-contention p7.
        assert_eq!(
            s.preset2, "p5",
            "NVENC default preset must be the balanced p5, not p7"
        );
        assert_ne!(
            s.preset2,
            constants::encoding::NVENC_PRESETS[0],
            "NVENC default must no longer be NVENC_PRESETS[0] (p7, max quality)"
        );
        // Must be a value the obs-nvenc/ffmpeg-nvenc plugins recognise.
        assert!(
            constants::encoding::NVENC_PRESETS.contains(&s.preset2.as_str()),
            "NVENC preset must be a recognised p1..p7 value, got {:?}",
            s.preset2
        );
        // Tune stays a recognised option (hq/ll/ull); we kept hq.
        assert!(
            constants::encoding::NVENC_TUNE_OPTIONS.contains(&s.tune.as_str()),
            "NVENC tune must be a recognised value, got {:?}",
            s.tune
        );
    }

    #[test]
    fn nvenc_default_preset_resolver_falls_back_to_list_head() {
        // The resolver picks the balanced preset by value; if it is ever
        // missing from the list it must degrade to the first element rather
        // than panic or emit an empty string.
        let p = nvenc_default_preset();
        assert!(
            constants::encoding::NVENC_PRESETS.contains(&p.as_str()),
            "resolved NVENC preset must come from the validated list, got {p:?}"
        );
        assert!(!p.is_empty(), "resolved NVENC preset must be non-empty");
    }

    #[test]
    fn qsv_default_target_usage_is_balanced_not_max_quality() {
        let s = ObsQsvSettings::default();
        // Balanced (TU4, OBS's own default), NOT `quality` (TU1, slowest).
        assert_eq!(
            s.target_usage, "balanced",
            "QSV default target_usage must be balanced, not quality"
        );
        assert_ne!(
            s.target_usage,
            constants::encoding::QSV_TARGET_USAGES[0],
            "QSV default must no longer be QSV_TARGET_USAGES[0] (quality, slowest)"
        );
        assert!(
            constants::encoding::QSV_TARGET_USAGES.contains(&s.target_usage.as_str()),
            "QSV target_usage must be a recognised value, got {:?}",
            s.target_usage
        );
    }

    #[test]
    fn qsv_default_target_usage_resolver_falls_back_to_list_head() {
        let t = qsv_default_target_usage();
        assert!(
            constants::encoding::QSV_TARGET_USAGES.contains(&t.as_str()),
            "resolved QSV target_usage must come from the validated list, got {t:?}"
        );
        assert!(!t.is_empty(), "resolved QSV target_usage must be non-empty");
    }

    #[test]
    fn amf_default_preset_stays_speed() {
        // AMF was already tuned to `speed` (v2.6.4) for iGPU contention; this is
        // a regression guard so it is never bumped back to quality/balanced.
        let s = ObsAmfSettings::default();
        assert_eq!(
            s.preset, "speed",
            "AMF default preset must stay speed for low GPU contention"
        );
        assert!(
            constants::encoding::AMF_PRESETS.contains(&s.preset.as_str()),
            "AMF preset must be a recognised value, got {:?}",
            s.preset
        );
    }

    #[test]
    fn x264_default_preset_stays_fast() {
        // x264 is the CPU fallback (720p-capped elsewhere); it must stay on a
        // fast preset so software encoding stays light. Regression guard.
        let s = ObsX264Settings::default();
        assert_eq!(
            s.preset, "veryfast",
            "x264 default preset must stay veryfast so the CPU fallback is light"
        );
        assert!(
            constants::encoding::X264_PRESETS.contains(&s.preset.as_str()),
            "x264 preset must be a recognised value, got {:?}",
            s.preset
        );
    }
}

// ---------------------------------------------------------------------------
// R46 consent-gate tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod consent_tests {
    //! R46 consent-gate tests.
    //!
    //! These cover the **config-layer** of the gate: a fresh config must
    //! report `NotGranted` and any derived `ConsentGuard` must refuse to
    //! let a recording entry point proceed. After the user accepts at the
    //! current version, the same check must pass. A bumped version must
    //! invalidate the old consent and re-prompt.
    use super::*;

    #[test]
    fn fresh_config_has_no_consent() {
        let cfg = Config::default();
        assert!(
            cfg.credentials.consent_given_at_version.is_none(),
            "a fresh config must have no stored consent version"
        );
        assert!(
            !cfg.credentials.has_consented,
            "a fresh config must have has_consented == false"
        );
    }

    #[test]
    fn fresh_config_recording_entry_point_errs() {
        let cfg = Config::default();
        let current = Version::parse("2.5.5").unwrap();
        let status = cfg.credentials.consent_status(&current);
        assert_eq!(status, ConsentStatus::NotGranted);

        let guard = ConsentGuard::new(status);
        // The recording entry point (input capture / OBS start) calls
        // `require_granted` — it MUST return Err here.
        let res = guard.require_granted();
        assert!(
            res.is_err(),
            "recording entry point must error before consent is recorded"
        );
    }

    #[test]
    fn recorded_consent_at_current_version_passes() {
        let mut cfg = Config::default();
        let current = Version::parse("2.5.5").unwrap();
        cfg.credentials.record_consent(current.clone());

        assert_eq!(
            cfg.credentials.consent_status(&current),
            ConsentStatus::Granted
        );
        let guard = ConsentGuard::new(cfg.credentials.consent_status(&current));
        assert!(
            guard.require_granted().is_ok(),
            "recording entry point must succeed once consent is recorded"
        );
        assert!(guard.is_granted());
    }

    #[test]
    fn bumped_version_invalidates_prior_consent() {
        let mut cfg = Config::default();
        cfg.credentials
            .record_consent(Version::parse("2.5.4").unwrap());

        // Now the binary bumps to 2.5.5 — the stored consent is stale.
        let current = Version::parse("2.5.5").unwrap();
        assert_eq!(
            cfg.credentials.consent_status(&current),
            ConsentStatus::VersionMismatch
        );
        let guard = ConsentGuard::new(cfg.credentials.consent_status(&current));
        assert!(
            guard.require_granted().is_err(),
            "bumped binary version must force re-consent"
        );
    }

    #[test]
    fn logout_clears_consent_version() {
        let mut cfg = Config::default();
        cfg.credentials
            .record_consent(Version::parse("2.5.5").unwrap());
        assert!(cfg.credentials.consent_given_at_version.is_some());

        cfg.credentials.logout();
        assert!(
            cfg.credentials.consent_given_at_version.is_none(),
            "logout must clear consent so the next user re-consents"
        );
        assert!(!cfg.credentials.has_consented);
    }

    #[test]
    fn serde_round_trip_preserves_consent_version() {
        let mut cfg = Config::default();
        cfg.credentials
            .record_consent(Version::parse("2.5.5").unwrap());

        let json = serde_json::to_string(&cfg).expect("serialize");
        let parsed: Config = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            parsed.credentials.consent_given_at_version,
            Some(Version::parse("2.5.5").unwrap())
        );
    }

    #[test]
    fn current_pkg_version_parses() {
        // Asserts the build-time contract: Cargo.toml version parses as semver.
        // If this ever panics, the workspace version literal is broken.
        let _ = current_pkg_version();
    }

    #[test]
    fn consent_disclosure_version_parses() {
        // Build-time contract: CONSENT_DISCLOSURE_VERSION parses as semver.
        // If this panics, the constant literal is broken.
        let _ = consent_disclosure_version();
    }

    #[test]
    fn consent_guard_from_config_respects_stored_version() {
        let mut cfg = Config::default();
        // Without consent: guard refuses.
        assert!(!consent_guard_from_config(&cfg).is_granted());

        // With consent at the active disclosure version: guard permits. The
        // guard keys on the disclosure version, so this is the value the UI
        // records on Accept.
        cfg.credentials.record_consent(consent_disclosure_version());
        assert!(consent_guard_from_config(&cfg).is_granted());
    }

    #[test]
    fn patch_bump_does_not_invalidate_consent() {
        // R46 regression: consent must survive an app patch/minor bump. The
        // gate compares against the DISCLOSURE version, which does NOT move
        // when CARGO_PKG_VERSION bumps. A tester hit the old bug (2.6.0 ->
        // 2.6.3 bumps silently invalidated consent, so the recorder booted but
        // never recorded).
        //
        // Self-contained replay of that scenario: the user consented under the
        // disclosure shipped at 2.6.0, then the app patched forward. As long as
        // the stored consent equals the active *disclosure* version, the status
        // is Granted regardless of how far the binary version has moved.
        let disclosure = Version::parse("2.6.0").unwrap();
        let mut cfg = Config::default();
        cfg.credentials.record_consent(disclosure.clone());

        // The binary is now several patches ahead (2.6.3), but the gate keys on
        // the disclosure version, so consent stands.
        assert_eq!(
            cfg.credentials.consent_status(&disclosure),
            ConsentStatus::Granted,
            "an app patch/minor bump must NOT invalidate consent — only a \
             disclosure-text change (CONSENT_DISCLOSURE_VERSION bump) may"
        );

        // And end-to-end through the real entry point against the live
        // disclosure constant: consenting at the active disclosure version
        // yields a granted guard.
        let mut cfg2 = Config::default();
        cfg2.credentials
            .record_consent(consent_disclosure_version());
        assert!(consent_guard_from_config(&cfg2).is_granted());
    }

    #[test]
    fn changed_disclosure_version_forces_reconsent() {
        // The legal gate is preserved: if the user accepted an OLDER disclosure
        // than the active one, they must re-consent.
        let mut cfg = Config::default();
        cfg.credentials
            .record_consent(Version::parse("0.0.1").unwrap());
        assert_eq!(
            cfg.credentials
                .consent_status(&consent_disclosure_version()),
            ConsentStatus::VersionMismatch,
            "a stored consent at a different disclosure version must re-prompt"
        );
        assert!(!consent_guard_from_config(&cfg).is_granted());
    }
}
