//! rc16.3 — adaptive capture tier chain (safety net for rc16.2).
//!
//! # What this module does
//!
//! rc16.2 flipped the Auto default from WGC to Monitor (Display Capture)
//! because WGC silently produces pure-black MP4s on OpenGL/GLFW windows
//! (Minecraft Java on bingd's RTX 4060). Display Capture works for *most*
//! rigs, but it isn't universal:
//!
//! * DRM games (Netflix, some Adobe tools) **specifically block** Desktop
//!   Duplication — Display Capture gets a black-or-frozen surface there
//!   and only the in-process WGC / GameHook path can see the protected
//!   content.
//! * HDR-on / Auto-HDR monitors return HDR-encoded BT.2020 buffers to
//!   Desktop Duplication, which the H.264/H.265 encoder packs as
//!   SDR-shaped pixels and the resulting MP4 looks "washed out" or, on
//!   some drivers, ends up as a black frame stream (the encoder sees an
//!   out-of-range surface and clips).
//! * Multi-monitor rigs where the game runs on display 2 but Desktop
//!   Duplication is initialized on display 1 — the monitor-by-cursor
//!   fallback added in rc16.2 patches the common case, but races on some
//!   driver/refresh-rate combinations.
//!
//! In short: **no single default mode works on every rig**. The recorder
//! needs to self-diagnose which tier works on the current machine and
//! cache the result.
//!
//! # How it works
//!
//! When `OYSTER_ADAPTIVE_CAPTURE=1`, the recorder consults a per-rig cache
//! at `%APPDATA%/Oyster Recorder/capture_mode.json` before each recording.
//!
//! 1. **Cache miss** → use the default tier chain head (`T1::Monitor`).
//!    After ~5 seconds of recording, probe the partial MP4 with
//!    `ffmpeg`'s `signalstats` filter. If the captured frames classify as
//!    `Black` or `Static`, advance to the next tier and try again. Cache
//!    the first `Healthy` tier as the validated mode for this rig.
//! 2. **Cache hit** → use the cached tier directly, skipping the probe
//!    for fast startup. If the cached tier silently produces a black
//!    recording at the end of the session, the cache is invalidated and
//!    the next session re-probes from `T1` again.
//!
//! # Scope of the rc16.3 wire-up
//!
//! **The rc16.3 wire-up is intentionally minimal.** The probe is run as a
//! background tokio task spawned after `Recording::start` returns; it
//! observes the live MP4, classifies the frame stream, and *updates the
//! cache*. It does **not** stop the current recording and restart with
//! the next tier — mid-session tier-swap would require invasive lifecycle
//! changes (`Recorder` would need to own the swap loop, `Recording` would
//! need to be re-entrant on the video side without losing input-writer
//! state, libobs source teardown would have to interleave with audio
//! source attachment, etc.) and we agreed to keep this safety-net commit
//! additive.
//!
//! The trade-off: a tester whose first session lands on a bad tier still
//! gets a black recording for that one session, but the probe detects it
//! and the cache forces the **next** session to skip directly to the next
//! tier. After at most three sessions (one per tier in the worst case),
//! every rig converges on a working tier.
//!
//! When Howard chooses to invest the lifecycle work later, the
//! `decide()`, `next_tier()`, and `TierDecision` building blocks here are
//! ready to drive a mid-session retry loop in `Recorder::start`.
//!
//! # Tier chain
//!
//! Default order (most-permissive first):
//! ```text
//! T1: Monitor   (DXGI Desktop Duplication; works for most native games)
//! T2: Wgc       (Windows.Graphics.Capture; needed for DRM titles)
//! T3: GameHook  (libobs game-capture hook; for anti-WGC anti-cheats)
//! ```
//!
//! Each retry costs ~7 seconds (5s probe window + ~2s OBS teardown +
//! re-init), so the worst case is a 14-21 second delay before the user
//! gets a working recording on a misbehaving rig. After the first
//! successful recording, cache hit drops the cost to zero.
//!
//! # Default OFF
//!
//! This is **safety-net** work for rc16.3. It does NOT ship enabled until
//! we've confirmed rc16.2's `Monitor`-as-default actually fails on
//! bingd's machine. Until then, the env var is unset and every code path
//! in this module is dormant.

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::Duration,
};

use color_eyre::eyre::{Result, eyre};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::EffectiveCaptureMode;

// ---------------------------------------------------------------------------
// Public env var + feature gate
// ---------------------------------------------------------------------------

/// rc16.3 — enable the adaptive tier chain. When unset (or set to anything
/// other than the values listed below), the recorder behaves identically
/// to rc16.2: a single hard-coded `Monitor` default with no probing.
///
/// Accepted values (case-insensitive, surrounding whitespace ignored):
///   `1` | `true` | `on` | `yes`  → enable adaptive tier chain
///   anything else or unset       → disabled (legacy rc16.2 behaviour)
///
/// We intentionally keep the gating positive (`opt-in`) so that the
/// "single mode default" we've validated in rc16.2 is what every tester
/// runs by default. Howard flips this on per-rig when rc16.2 fails.
pub const OYSTER_ADAPTIVE_CAPTURE_ENV: &str = "OYSTER_ADAPTIVE_CAPTURE";

/// Optional override of the cache-file location. Tests use this to point
/// at a tmp file; production code leaves it unset and the cache lives at
/// the per-user default below.
pub const OYSTER_ADAPTIVE_CACHE_PATH_ENV: &str = "OYSTER_ADAPTIVE_CACHE_PATH";

/// Returns `true` if the adaptive tier chain is enabled for this process.
/// Read once per recording so deployment scripts can flip it between
/// sessions without restarting the daemon.
pub fn is_enabled() -> bool {
    match std::env::var(OYSTER_ADAPTIVE_CAPTURE_ENV) {
        Ok(v) => parse_enabled(&v),
        Err(_) => false,
    }
}

/// Pure parser for the env var. Extracted so unit tests can exercise
/// every accepted/rejected form without poking the process environment
/// (which would race against `cargo test`'s parallel runner).
fn parse_enabled(raw: &str) -> bool {
    let normalized = raw.trim().to_ascii_lowercase();
    matches!(normalized.as_str(), "1" | "true" | "on" | "yes")
}

// ---------------------------------------------------------------------------
// Tier chain
// ---------------------------------------------------------------------------

/// Default tier order, applied when the cache has no entry for the
/// current rig. Ordered from "most likely to work on a random rig" to
/// "last resort":
///
/// 1. `Monitor` — DXGI Desktop Duplication. Renderer-agnostic
///    (D3D11/12/Vulkan/OpenGL/GLFW all work). Doesn't see DRM content.
/// 2. `Wgc` — Windows.Graphics.Capture. Per-window swapchain reader.
///    Good for native DirectX games and the only path that can capture
///    HDR-on monitors correctly. **Reads black for OpenGL/GLFW** —
///    which is the rc16.2 bug, so we put this *after* `Monitor`.
/// 3. `GameHook` — libobs in-process DLL injection. Required for games
///    that disable both DXGI duplication and WGC under anti-cheat
///    (older CS:GO, Roblox under hyperion).
///
/// We do **not** include a 4th BitBlt tier — libobs' window-capture path
/// uses BitBlt internally under `Wgc` when the platform predates 1903,
/// and the recorder targets Win10 1903+ anyway. Adding a 4th tier just
/// extends the worst-case startup latency without helping any production
/// rig we've seen.
pub const TIER_CHAIN: &[EffectiveCaptureMode] = &[
    EffectiveCaptureMode::Monitor,
    EffectiveCaptureMode::Wgc,
    EffectiveCaptureMode::GameHook,
];

/// Advance to the next tier in the chain, or return `None` if we've
/// exhausted the chain (the rig is genuinely broken — there's nothing
/// left to fall through to).
pub fn next_tier(current: EffectiveCaptureMode) -> Option<EffectiveCaptureMode> {
    let pos = TIER_CHAIN.iter().position(|t| *t == current)?;
    TIER_CHAIN.get(pos + 1).copied()
}

// ---------------------------------------------------------------------------
// Probe — frame statistics over the live MP4
// ---------------------------------------------------------------------------

/// Classification of a captured frame stream. Used to decide whether to
/// keep the current tier or fall through to the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeResult {
    /// The frames look like real game capture: above-black brightness and
    /// at least some motion or hash variance. Tier validated — cache it.
    Healthy,
    /// Brightness is fine but every sampled frame is identical. Usually
    /// means we attached to a screensaver, a paused window, or an
    /// out-of-process surface that isn't actually animating. Treat the
    /// same as `Black` for tier-fallthrough purposes — if the user's
    /// game is on-screen we'd see motion.
    Static,
    /// Mean luma is at or below `BLACK_LUMA_THRESHOLD` and inter-frame
    /// diff is zero. This is the bingd failure mode — WGC attached to an
    /// OpenGL swapchain it can't read, producing YUV(16,16,16) =
    /// mathematical black at full 30 fps.
    Black,
    /// ffprobe couldn't read the file (still being written, ffmpeg
    /// missing from PATH, etc.). Treated as `Healthy` for fall-through
    /// purposes — we never want a probe failure to trigger a tier change
    /// it can't justify. The session continues on the current tier.
    Indeterminate,
}

/// `signalstats.YAVG` is the BT.601 luma channel mean for the sampled
/// frame, in [0, 255]. A frame encoded from a pure-black source has
/// Y = 16 after the studio-range YUV remap (BT.601 maps 0 → 16, not 0).
/// We use 18 as the threshold so a fully-quantized black sneaks below
/// while real game frames (~40+) stay above.
const BLACK_LUMA_THRESHOLD: f64 = 18.0;

/// Motion threshold: `signalstats.YDIF` measures the mean absolute
/// difference in Y between consecutive frames, in [0, 255]. Any value
/// above this indicates real frame-to-frame motion. Static-image
/// captures sit at exactly 0; even a slow-panning game scene rarely
/// drops below 3 for any sampled segment.
const MOTION_THRESHOLD: f64 = 3.0;

/// How long to wait before probing. ffmpeg-mux flushes lazily, so the
/// MP4 may not have a parseable header until ~3-4 seconds in. 5 seconds
/// is the empirical sweet spot — long enough to have flushed frames,
/// short enough that the worst-case "all three tiers fail" total
/// startup time is ~21 seconds, which still beats the previous
/// "5-minute black recording is silently uploaded" outcome.
pub const PROBE_DELAY: Duration = Duration::from_secs(5);

/// How many seconds of the partial MP4 to probe. We feed ffmpeg
/// `-ss 0 -t 3` so it reads from start through 3 seconds of frames.
/// Longer doesn't help — frame statistics stabilize within a second of
/// game-on-screen footage.
pub const PROBE_DURATION_SECS: u64 = 3;

/// Timeout on the ffmpeg subprocess. ffmpeg sometimes hangs trying to
/// read the moov atom of a still-open MP4 — we cap it so a hung probe
/// doesn't stall the recording-start path forever.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Run the signalstats probe against a partial MP4. Returns the
/// classification used by the tier-chain caller to decide whether to
/// accept this tier or fall through.
///
/// This function is async because it shells out to ffmpeg via
/// `tokio::process::Command` — the same pattern `video_metadata.rs`
/// uses for ffprobe. We don't take `&self` so callers can spawn it
/// without lifetime gymnastics.
pub async fn probe_capture_health(video_path: &Path) -> ProbeResult {
    // Bail out before spawning anything if the file doesn't exist yet.
    // OBS creates the file the moment ffmpeg-mux opens its output, so a
    // missing file means the recording start path is still in
    // prepare_source — probing is meaningless until at least one frame
    // has been written.
    match tokio::fs::metadata(video_path).await {
        Ok(m) if m.len() == 0 => {
            tracing::warn!(
                path = %video_path.display(),
                "Adaptive probe: MP4 is zero bytes, treating as Indeterminate"
            );
            return ProbeResult::Indeterminate;
        }
        Err(e) => {
            tracing::warn!(
                path = %video_path.display(),
                error = ?e,
                "Adaptive probe: cannot stat MP4, treating as Indeterminate"
            );
            return ProbeResult::Indeterminate;
        }
        Ok(_) => {}
    }

    let path_str = video_path.to_string_lossy().into_owned();

    // Build the ffmpeg invocation. We use ffmpeg (not ffprobe) because:
    // 1. ffprobe doesn't have the `signalstats` filter wired up to the
    //    `-show_frames` JSON output — we'd have to parse stderr anyway.
    // 2. ffmpeg + `signalstats,metadata=print` writes one tag set per
    //    frame to stderr, which is trivial to scrape line-by-line.
    //
    // `-fflags +igndts` lets ffmpeg read in-progress fragmented MP4s
    // that haven't written their moov atom yet. Some libobs ffmpeg-mux
    // configurations only finalize moov at output.stop() — without this
    // flag, ffmpeg refuses with "moov atom not found".
    let mut command = tokio::process::Command::new("ffmpeg");
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "info",
            // Permissive parsing of in-progress MP4s.
            "-fflags",
            "+igndts+genpts",
            "-err_detect",
            "ignore_err",
            // Read at most PROBE_DURATION_SECS seconds from start.
            "-ss",
            "0",
            "-t",
            &PROBE_DURATION_SECS.to_string(),
            "-i",
            &path_str,
            // signalstats emits YAVG / YDIF / YHIGH etc. per frame.
            // metadata=print routes them to stderr in a parseable form.
            "-vf",
            "signalstats,metadata=print:file=-",
            "-an",
            "-f",
            "null",
            "-",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Spawn and apply the timeout. tokio::process doesn't have a
    // first-class timeout, so we tokio::select! against a sleep and
    // kill the child on expiry. We're deliberately careful to drop the
    // child (which kills the process group on Windows) rather than
    // letting a hung ffmpeg outlive the recording.
    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = ?e,
                "Adaptive probe: failed to spawn ffmpeg ({}). PATH may be missing ffmpeg — \
                 treating as Indeterminate so the session continues",
                e
            );
            return ProbeResult::Indeterminate;
        }
    };

    let output = match tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!(error = ?e, "Adaptive probe: ffmpeg wait failed");
            return ProbeResult::Indeterminate;
        }
        Err(_elapsed) => {
            tracing::warn!(
                "Adaptive probe: ffmpeg exceeded {}s timeout — treating as Indeterminate",
                PROBE_TIMEOUT.as_secs()
            );
            return ProbeResult::Indeterminate;
        }
    };

    // Even on non-zero exit code we try to parse the stderr — ffmpeg
    // often exits 1 when reading an in-progress MP4 but still emits
    // valid signalstats lines for the frames it managed to decode.
    let stderr_text = String::from_utf8_lossy(&output.stderr);
    classify_signalstats_stream(&stderr_text)
}

/// Pure-function classifier. Parses the `lavfi.signalstats.*` tags that
/// `signalstats,metadata=print:file=-` writes to stderr and reduces them
/// to a `ProbeResult`.
///
/// Extracted from `probe_capture_health` so unit tests can exercise the
/// classification logic without shelling out to ffmpeg.
pub fn classify_signalstats_stream(stderr_text: &str) -> ProbeResult {
    let mut yavg_samples: Vec<f64> = Vec::new();
    let mut ydif_samples: Vec<f64> = Vec::new();
    let mut yhigh_samples: Vec<f64> = Vec::new();

    for line in stderr_text.lines() {
        if let Some(rest) = extract_signalstats_value(line, "YAVG") {
            yavg_samples.push(rest);
        } else if let Some(rest) = extract_signalstats_value(line, "YDIF") {
            ydif_samples.push(rest);
        } else if let Some(rest) = extract_signalstats_value(line, "YHIGH") {
            yhigh_samples.push(rest);
        }
    }

    if yavg_samples.is_empty() {
        tracing::warn!(
            "Adaptive probe: ffmpeg produced no signalstats samples — treating as Indeterminate"
        );
        return ProbeResult::Indeterminate;
    }

    let yavg_mean = yavg_samples.iter().sum::<f64>() / (yavg_samples.len() as f64);
    let ydif_max = ydif_samples.iter().copied().fold(0.0_f64, f64::max);
    let yhigh_max = yhigh_samples.iter().copied().fold(0.0_f64, f64::max);

    tracing::info!(
        sample_count = yavg_samples.len(),
        %yavg_mean,
        %ydif_max,
        %yhigh_max,
        "Adaptive probe: signalstats summary"
    );

    // BLACK: mean luma is at or below the studio-range pure-black floor
    // AND there's no motion. We require BOTH so a paused dark-themed
    // game (e.g. Hollow Knight at night) doesn't get misclassified as
    // black.
    if yavg_mean <= BLACK_LUMA_THRESHOLD && ydif_max <= 0.5 {
        return ProbeResult::Black;
    }

    // STATIC: brightness is OK but no frame-to-frame motion at all.
    // Indicates we attached to a frozen surface — common when WGC reads
    // the GL swap chain backbuffer and gets a stale snapshot from the
    // moment of attachment.
    if ydif_max < MOTION_THRESHOLD && yavg_samples.len() > 1 {
        // YHIGH should vary across frames in a real game. If it doesn't,
        // we're locked on one image.
        let yhigh_min = yhigh_samples.iter().copied().fold(f64::INFINITY, f64::min);
        let yhigh_spread = (yhigh_max - yhigh_min).abs();
        if yhigh_spread < 0.5 {
            return ProbeResult::Static;
        }
    }

    ProbeResult::Healthy
}

/// Parse a line like
/// `[Parsed_metadata_1 @ 0x...] lavfi.signalstats.YAVG=124.5`
/// into the floating-point value, returning `None` if the line doesn't
/// match this shape or the tag name doesn't match.
fn extract_signalstats_value(line: &str, tag: &str) -> Option<f64> {
    // Cheap pre-check: the tag is always preceded by `signalstats.`.
    let needle = format!("signalstats.{tag}=");
    let idx = line.find(&needle)?;
    let after = &line[idx + needle.len()..];
    // The value runs until end-of-line or whitespace.
    let end = after
        .find(|c: char| c.is_whitespace())
        .unwrap_or(after.len());
    after[..end].parse::<f64>().ok()
}

// ---------------------------------------------------------------------------
// Per-rig cache
// ---------------------------------------------------------------------------

/// Stable identifier for the current hardware + OS combo. We hash GPU
/// name, OS version, and monitor count rather than using the raw fields
/// so the cache file doesn't double as a hardware fingerprint dump on
/// disk. SHA-256 truncated to 16 hex chars (64 bits) is plenty for cache
/// keying — collisions would just mean the cache is occasionally wrong
/// for one rig pair, not a correctness bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RigFingerprint(String);

impl RigFingerprint {
    /// Compute the fingerprint from raw inputs. Caller supplies the
    /// GPU name (e.g. "NVIDIA GeForce RTX 4060") and OS version
    /// (e.g. "Windows 11 22H2") and monitor count.
    pub fn compute(gpu_name: &str, os_version: &str, monitor_count: u32) -> Self {
        let mut hasher = Sha256::new();
        // Lowercase to make the hash stable across cosmetic case
        // differences in driver-reported strings (NVIDIA capitalizes
        // inconsistently across driver versions).
        hasher.update(gpu_name.to_ascii_lowercase().as_bytes());
        hasher.update(b"|");
        hasher.update(os_version.to_ascii_lowercase().as_bytes());
        hasher.update(b"|");
        hasher.update(monitor_count.to_string().as_bytes());
        let digest = hasher.finalize();
        // 16 hex chars = 64 bits of fingerprint. Plenty for a per-rig
        // cache; we're not deduping users.
        let hex = format!("{:x}", digest);
        Self(hex[..16].to_owned())
    }

    /// Compute the fingerprint for the host this process is running on.
    /// Self-contained — no caller needs to thread adapter info through.
    /// Failures fall back to a stable "unknown" value so the cache key
    /// is still computable on rigs where wgpu enumeration fails.
    pub fn current() -> Self {
        let gpu_name = current_gpu_name().unwrap_or_else(|| "unknown-gpu".to_owned());
        let os_version = sysinfo::System::long_os_version()
            .or_else(sysinfo::System::os_version)
            .unwrap_or_else(|| "unknown-os".to_owned());
        let monitor_count = current_monitor_count();
        Self::compute(&gpu_name, &os_version, monitor_count)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Best-effort lookup of the primary GPU's name. We pick the first
/// discrete GPU if available (matches `Recorder::new`'s adapter index
/// logic) so the fingerprint matches the GPU the recording is using.
fn current_gpu_name() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        use egui_wgpu::wgpu;
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapters: Vec<_> = instance
            .enumerate_adapters(wgpu::Backends::DX12)
            .into_iter()
            .collect();
        // Prefer discrete GPU; fall back to first adapter.
        adapters
            .iter()
            .find(|a| a.get_info().device_type == wgpu::DeviceType::DiscreteGpu)
            .or_else(|| adapters.first())
            .map(|a| a.get_info().name.clone())
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Count of attached monitors. Falls back to 1 on platforms where the
/// query fails — the fingerprint will still match itself across runs.
fn current_monitor_count() -> u32 {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CMONITORS};
        // SAFETY: GetSystemMetrics is a plain Win32 call with no
        // mutable state.
        let n = unsafe { GetSystemMetrics(SM_CMONITORS) };
        if n > 0 { n as u32 } else { 1 }
    }
    #[cfg(not(target_os = "windows"))]
    {
        1
    }
}

/// Serializable capture-mode tag for the cache file. Mirrors
/// `EffectiveCaptureMode` but with explicit `serde` tags so the on-disk
/// format is stable even if the enum is reordered or extended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachedTier {
    Monitor,
    Wgc,
    GameHook,
}

impl From<EffectiveCaptureMode> for CachedTier {
    fn from(value: EffectiveCaptureMode) -> Self {
        match value {
            EffectiveCaptureMode::Monitor => Self::Monitor,
            EffectiveCaptureMode::Wgc => Self::Wgc,
            EffectiveCaptureMode::GameHook => Self::GameHook,
        }
    }
}

impl From<CachedTier> for EffectiveCaptureMode {
    fn from(value: CachedTier) -> Self {
        match value {
            CachedTier::Monitor => Self::Monitor,
            CachedTier::Wgc => Self::Wgc,
            CachedTier::GameHook => Self::GameHook,
        }
    }
}

/// One cache entry per rig fingerprint. `consecutive_uses` exists so
/// future revalidation logic can re-probe every Nth session even if the
/// cached tier is still nominally working — driver updates can silently
/// break a previously-good tier, and we'd rather catch that than upload
/// a black recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub tier: CachedTier,
    /// ISO-8601 UTC timestamp of last successful validation. Used by the
    /// "stale cache" check — entries older than `STALE_AFTER_DAYS` are
    /// re-probed even on cache hit.
    pub validated_at_iso: String,
    #[serde(default)]
    pub consecutive_uses: u32,
}

impl CacheEntry {
    pub fn new(tier: EffectiveCaptureMode) -> Self {
        Self {
            tier: tier.into(),
            validated_at_iso: chrono::Utc::now().to_rfc3339(),
            consecutive_uses: 0,
        }
    }

    pub fn is_stale(&self, max_age_days: i64) -> bool {
        let Ok(then) = chrono::DateTime::parse_from_rfc3339(&self.validated_at_iso) else {
            // Unparseable timestamp → treat as stale, force a re-probe.
            // Better safe than upload-black-frames.
            return true;
        };
        let age = chrono::Utc::now().signed_duration_since(then.with_timezone(&chrono::Utc));
        age.num_days() > max_age_days
    }
}

/// On-disk cache file. Schema-versioned so future shape changes can
/// migrate without crashing on an old file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureModeCache {
    /// Bumped when the JSON shape changes. v1 = the current shape.
    pub schema_version: u32,
    pub entries: std::collections::HashMap<String, CacheEntry>,
}

impl Default for CaptureModeCache {
    fn default() -> Self {
        Self {
            schema_version: 1,
            entries: std::collections::HashMap::new(),
        }
    }
}

/// Cache entries older than this are re-probed on the next session.
/// 30 days catches the common "driver updated last week and broke
/// monitor capture" case without churning the probe every session.
pub const STALE_AFTER_DAYS: i64 = 30;

impl CaptureModeCache {
    /// Resolve the on-disk cache path. Respects the test override env
    /// var so unit tests don't clobber the production cache file.
    pub fn cache_path() -> Result<PathBuf> {
        if let Ok(override_path) = std::env::var(OYSTER_ADAPTIVE_CACHE_PATH_ENV) {
            return Ok(PathBuf::from(override_path));
        }
        let dir = dirs::data_local_dir()
            .ok_or_else(|| eyre!("Could not resolve LocalAppData directory for adaptive cache"))?
            .join("Oyster Recorder");
        Ok(dir.join("capture_mode.json"))
    }

    /// Load the cache from disk, or return an empty cache if no file
    /// exists yet / the file is unreadable. We never propagate I/O
    /// errors from this path — a corrupt cache is a recoverable
    /// inconvenience (worst case: one extra probe), not a recording
    /// failure.
    pub fn load() -> Self {
        let Ok(path) = Self::cache_path() else {
            return Self::default();
        };
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(c) if c.schema_version == 1 => c,
                Ok(other) => {
                    tracing::warn!(
                        version = other.schema_version,
                        path = %path.display(),
                        "Adaptive cache has unknown schema version, ignoring"
                    );
                    Self::default()
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = ?e,
                        "Adaptive cache is corrupt, starting fresh"
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = ?e,
                    "Adaptive cache read failed, starting fresh"
                );
                Self::default()
            }
        }
    }

    /// Persist to disk. Failures are logged but not surfaced — caching
    /// is a perf optimization, not a correctness gate.
    pub fn save(&self) {
        let Ok(path) = Self::cache_path() else {
            tracing::warn!("Adaptive cache: no cache path available, skipping save");
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                tracing::warn!(
                    path = %parent.display(),
                    error = ?e,
                    "Adaptive cache: failed to create directory, skipping save"
                );
                return;
            }
        }
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(e) = fs::write(&path, bytes) {
                    tracing::warn!(
                        path = %path.display(),
                        error = ?e,
                        "Adaptive cache: write failed"
                    );
                } else {
                    tracing::debug!(path = %path.display(), "Adaptive cache saved");
                }
            }
            Err(e) => {
                tracing::warn!(error = ?e, "Adaptive cache: serialization failed");
            }
        }
    }

    /// Look up the cached tier for a rig. Returns `None` if the entry
    /// is missing or stale (>30d since last validation).
    pub fn lookup(&self, fingerprint: &RigFingerprint) -> Option<EffectiveCaptureMode> {
        let entry = self.entries.get(fingerprint.as_str())?;
        if entry.is_stale(STALE_AFTER_DAYS) {
            tracing::info!(
                rig = %fingerprint.as_str(),
                validated_at = %entry.validated_at_iso,
                "Adaptive cache: entry stale, will re-probe"
            );
            return None;
        }
        Some(entry.tier.into())
    }

    /// Record a validated tier for a rig. Bumps `consecutive_uses` if
    /// the same tier is being re-confirmed, resets it to 0 if the rig
    /// has switched tiers.
    pub fn record(&mut self, fingerprint: &RigFingerprint, tier: EffectiveCaptureMode) {
        let cached_tier: CachedTier = tier.into();
        let entry = self
            .entries
            .entry(fingerprint.as_str().to_owned())
            .and_modify(|e| {
                if e.tier == cached_tier {
                    e.consecutive_uses = e.consecutive_uses.saturating_add(1);
                } else {
                    e.tier = cached_tier;
                    e.consecutive_uses = 0;
                }
                e.validated_at_iso = chrono::Utc::now().to_rfc3339();
            })
            .or_insert_with(|| CacheEntry::new(tier));
        tracing::info!(
            rig = %fingerprint.as_str(),
            tier = ?tier,
            consecutive_uses = entry.consecutive_uses,
            "Adaptive cache: recorded successful tier"
        );
    }

    /// Invalidate a cache entry — called when a cached tier produces a
    /// black recording (cache was wrong, driver updated, etc.). The
    /// next session re-probes from the top of the chain.
    pub fn invalidate(&mut self, fingerprint: &RigFingerprint) {
        if self.entries.remove(fingerprint.as_str()).is_some() {
            tracing::warn!(
                rig = %fingerprint.as_str(),
                "Adaptive cache: invalidated entry (cached tier produced bad recording)"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tier-chain orchestration helper
// ---------------------------------------------------------------------------

/// The probe-and-decide step the recorder runs after starting a tier.
/// Returns the action the caller should take:
pub enum TierDecision {
    /// Probe result was `Healthy` — keep this tier and cache it.
    KeepAndCache,
    /// Probe result was `Black` or `Static` — fall through to this
    /// next tier.
    Advance(EffectiveCaptureMode),
    /// No further tiers available. The recorder should accept whatever
    /// it has and log a loud warning; this is "the rig is broken" state.
    Exhausted,
    /// Probe was inconclusive — keep the current tier but don't cache
    /// it (so the next session re-probes).
    KeepWithoutCaching,
}

/// Pure decision function. Takes a probe result and the current tier,
/// returns what to do next. Extracted from any I/O so unit tests can
/// exercise the full state machine without spawning ffmpeg.
pub fn decide(probe: ProbeResult, current: EffectiveCaptureMode) -> TierDecision {
    match probe {
        ProbeResult::Healthy => TierDecision::KeepAndCache,
        ProbeResult::Black | ProbeResult::Static => match next_tier(current) {
            Some(next) => TierDecision::Advance(next),
            None => TierDecision::Exhausted,
        },
        ProbeResult::Indeterminate => TierDecision::KeepWithoutCaching,
    }
}

/// PRD-100 Audit I-2 — fullscreen-aware variant of `decide()`.
///
/// **The bug** this fixes: NVIDIA RTX 4060 + an F11 exclusive-fullscreen
/// game + Monitor (DXGI Desktop Duplication) tier produces a pure-black
/// MP4. DXGI Desktop Duplication cannot read the dedicated swapchain
/// that exclusive-fullscreen games own — only the windowed compositor
/// surface, which the GPU never paints into when the app has stolen the
/// monitor. The existing `decide()` would advance Monitor → Wgc → only
/// then to GameHook, costing two extra probe cycles (~10s each) before
/// landing on the only tier that can actually inject into an
/// exclusive-fullscreen game.
///
/// **The shortcut**: when the caller knows the foreground window is
/// running exclusive-fullscreen *and* the Monitor probe just classified
/// as Black, we already have enough information to skip Wgc entirely.
/// Wgc on an exclusive-fullscreen game has the exact same problem
/// Monitor does — both read the desktop compositor, neither sees the
/// dedicated swapchain. Going directly to GameHook saves one full
/// probe-cycle and one bad-recording session per affected rig.
///
/// The hint is **best-effort** — `detect_fullscreen_exclusive()` errs on
/// the side of false-positives over false-negatives (a false positive
/// just steers us toward GameHook one tier early, which is harmless;
/// a false negative re-creates the original bug). When the hint is
/// `false`, this function behaves identically to `decide()`, so
/// non-fullscreen sessions are unaffected.
pub fn decide_with_fullscreen_hint(
    probe: ProbeResult,
    current: EffectiveCaptureMode,
    fullscreen_exclusive: bool,
) -> TierDecision {
    // The shortcut only kicks in when *all three* conditions hold:
    //   1. Probe says the recording is Black (the diagnostic signal).
    //   2. We're currently on Monitor (the failing tier for fullscreen).
    //   3. We have positive evidence of exclusive-fullscreen.
    // Anything else falls through to the unchanged `decide()` logic so
    // we don't regress any pre-existing flow.
    if fullscreen_exclusive
        && matches!(probe, ProbeResult::Black)
        && matches!(current, EffectiveCaptureMode::Monitor)
    {
        tracing::warn!(
            "Adaptive probe: Monitor tier produced Black frames AND foreground window is \
             exclusive-fullscreen — skipping Wgc tier and advancing directly to GameHook \
             (Audit I-2). Wgc reads the same compositor surface Monitor does, so it would \
             also produce Black on this rig."
        );
        return TierDecision::Advance(EffectiveCaptureMode::GameHook);
    }
    decide(probe, current)
}

// ---------------------------------------------------------------------------
// Fullscreen-exclusive detection (PRD-100 Audit I-2)
// ---------------------------------------------------------------------------

/// Best-effort detection of whether `hwnd_raw` (the foreground game's
/// window handle, cast through `isize` so this function compiles on
/// non-Windows CI without a `windows::Win32::Foundation::HWND` import)
/// is running in exclusive-fullscreen mode.
///
/// **Why isize and not HWND?** `HWND` is a Windows-only type from the
/// `windows` crate; making the helper portable lets the unit tests in
/// this file build on macOS / Linux CI without target-gating every call
/// site. On 64-bit Windows `HWND(*mut c_void)` is bitwise an `isize`,
/// so the caller does `hwnd.0 as isize` and we restore it with
/// `HWND(raw as *mut _)`.
///
/// **The signal we use**: classic Win32 fullscreen-exclusive games
/// (D3D9, D3D11 with `Windowed=FALSE` swapchains, OpenGL via wglSwapBuffers
/// after a `ChangeDisplaySettings` mode-switch) all create a borderless
/// `WS_POPUP` window with no `WS_CAPTION`. This is the same heuristic
/// the original WGC implementation in libobs uses to detect "is this
/// even a regular window?" — it's not a perfect proxy for "owns the
/// dedicated swapchain", but in practice it catches every real
/// exclusive-fullscreen game we've tested against.
///
/// We deliberately accept the false-positive case (a borderless-windowed
/// game that just *looks* like fullscreen) because the consequence is
/// purely "skip Wgc and go to GameHook one tier earlier" — GameHook
/// works on borderless-windowed too. The false-negative case (a real
/// exclusive-fullscreen that has a caption for some reason) just keeps
/// the pre-Audit-I-2 behaviour.
///
/// Returns `false` on non-Windows targets (no exclusive-fullscreen
/// model exists outside Win32) and on any Win32 API failure (read:
/// "we don't know, behave like before").
pub fn detect_fullscreen_exclusive_raw(hwnd_raw: isize) -> bool {
    #[cfg(target_os = "windows")]
    {
        // Null HWND => no window to probe => not fullscreen.
        if hwnd_raw == 0 {
            return false;
        }

        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            GWL_STYLE, GetWindowLongPtrW, WS_CAPTION, WS_POPUP,
        };

        let hwnd = HWND(hwnd_raw as *mut _);

        // Safety: `GetWindowLongPtrW` is a pure read of a window
        // property and is safe to call from any thread, on any HWND
        // value (it returns 0 on invalid handle which we treat as
        // "unknown"). We never mutate the style — `SetWindowLongPtrW`
        // is not called from this path.
        let style_raw = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
        if style_raw == 0 {
            // `GetWindowLongPtrW` returns 0 either when the style is
            // genuinely zero (impossible for a real window) or when
            // the call failed. Either way we have no signal, so
            // default to false (don't trigger the shortcut).
            return false;
        }

        let style = style_raw as u32;
        let has_popup = (style & WS_POPUP.0) != 0;
        let has_caption = (style & WS_CAPTION.0) != 0;

        // The classic exclusive-fullscreen footprint: `WS_POPUP` set,
        // `WS_CAPTION` cleared. Borderless-windowed games hit the
        // same signature, which is fine (see the false-positive
        // discussion in the docstring above).
        has_popup && !has_caption
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = hwnd_raw;
        false
    }
}

/// Windows-typed convenience wrapper around `detect_fullscreen_exclusive_raw`.
/// Use this from any code that already has a `HWND` in scope; it just
/// forwards the raw pointer value to the portable helper.
#[cfg(target_os = "windows")]
pub fn detect_fullscreen_exclusive(hwnd: windows::Win32::Foundation::HWND) -> bool {
    detect_fullscreen_exclusive_raw(hwnd.0 as isize)
}

// ---------------------------------------------------------------------------
// Background probe task — spawned from `Recording::start`
// ---------------------------------------------------------------------------

/// Spawn a fire-and-forget tokio task that waits `PROBE_DELAY`, probes
/// the live MP4 at `video_path`, and updates the per-rig cache based on
/// the result.
///
/// The task is detached from the recording lifecycle on purpose:
/// * If the user hits stop before the probe deadline, the task observes
///   a zero-byte / no-longer-growing file and returns `Indeterminate`,
///   which never touches the cache. This is exactly what we want — we
///   don't want a 5-second recording to falsely invalidate a cached
///   tier.
/// * If the probe takes longer than the recording itself, the task
///   simply finishes after the stop and logs its conclusion. There's no
///   coupling to the active `Recording` struct, so the task is safe
///   even if the recording is dropped while the probe is mid-flight.
///
/// `current_tier` is the mode that was active when this task was
/// spawned. We pass it explicitly rather than re-resolving inside the
/// task so the cache update is paired with the actual tier the user
/// experienced, not whatever the config happens to say at task end.
pub fn spawn_probe_task(video_path: PathBuf, current_tier: EffectiveCaptureMode) {
    // Old API: no fullscreen hint available. Forward to the hwnd-aware
    // variant with a sentinel `0` so the Audit I-2 shortcut stays
    // dormant (the hint helper short-circuits on null hwnd). This
    // preserves the v2.6.0 wire contract for callers in `recording.rs`
    // that don't yet pass the game HWND through.
    spawn_probe_task_with_hwnd(video_path, current_tier, 0);
}

/// PRD-100 Audit I-2 variant of `spawn_probe_task`. Additional `hwnd_raw`
/// argument is the foreground game's window handle cast to `isize`
/// (i.e. `hwnd.0 as isize` on Windows). When the probe lands on Black
/// AND `detect_fullscreen_exclusive_raw(hwnd_raw)` returns true AND the
/// active tier is Monitor, the cache fast-forwards to GameHook instead
/// of Wgc, saving one bad-recording session per affected rig.
///
/// Callers in `recording.rs` may migrate to this variant when ready;
/// the legacy `spawn_probe_task` forwards here with `hwnd_raw = 0` so no
/// behaviour changes for callers that haven't been updated yet.
pub fn spawn_probe_task_with_hwnd(
    video_path: PathBuf,
    current_tier: EffectiveCaptureMode,
    hwnd_raw: isize,
) {
    // Don't even spawn if the env var is off — `is_enabled` is cheap
    // but the caller may not have checked yet.
    if !is_enabled() {
        return;
    }

    // Sample the fullscreen state *now*, before spawning the detached
    // task. The window's style is essentially static for the lifetime
    // of a recording (games don't flip in and out of exclusive mode
    // mid-session), and reading it on the calling thread avoids
    // forcing the detached task to know how to safely cross the HWND
    // pointer over a tokio task boundary.
    let fullscreen_exclusive = detect_fullscreen_exclusive_raw(hwnd_raw);

    tokio::spawn(async move {
        tokio::time::sleep(PROBE_DELAY).await;

        let result = probe_capture_health(&video_path).await;
        let fingerprint = RigFingerprint::current();

        tracing::info!(
            rig = %fingerprint.as_str(),
            tier = ?current_tier,
            probe = ?result,
            fullscreen_exclusive,
            path = %video_path.display(),
            "Adaptive probe: classification complete"
        );

        match decide_with_fullscreen_hint(result, current_tier, fullscreen_exclusive) {
            TierDecision::KeepAndCache => {
                let mut cache = CaptureModeCache::load();
                cache.record(&fingerprint, current_tier);
                cache.save();
            }
            TierDecision::Advance(next) => {
                tracing::warn!(
                    rig = %fingerprint.as_str(),
                    current = ?current_tier,
                    next = ?next,
                    "Adaptive probe: current tier produced bad frames. The cache will steer the \
                     NEXT session to {next:?}. Mid-session swap is not yet implemented (rc16.3 \
                     scope) so this recording will continue as-is."
                );
                let mut cache = CaptureModeCache::load();
                // Record the *next* tier as the cached choice so the
                // next session picks it up directly. We're effectively
                // saying "we now believe this rig wants `next`" — the
                // cache is correct-by-construction even though we
                // haven't confirmed `next` works yet. If `next` also
                // produces black, the next session's probe will
                // continue to advance.
                cache.record(&fingerprint, next);
                cache.save();
            }
            TierDecision::Exhausted => {
                tracing::error!(
                    rig = %fingerprint.as_str(),
                    current = ?current_tier,
                    "Adaptive probe: tier chain exhausted on bad frames. No further tiers \
                     to try — this rig may be genuinely incapable of capture in any mode. \
                     Cache invalidated; the next session re-probes from the top."
                );
                let mut cache = CaptureModeCache::load();
                cache.invalidate(&fingerprint);
                cache.save();
            }
            TierDecision::KeepWithoutCaching => {
                tracing::info!(
                    rig = %fingerprint.as_str(),
                    "Adaptive probe: inconclusive — cache untouched, next session re-probes"
                );
            }
        }
    });
}

/// Look up the cached tier for this rig, if the adaptive feature is
/// enabled and a non-stale entry exists. Returns `None` when the cache
/// shouldn't be consulted (feature off) or when there's no usable entry
/// — callers fall back to the rc16.2 default resolution in that case.
///
/// Designed to be called from `effective_capture_mode` with a priority
/// just below `OYSTER_CAPTURE_MODE`: hard manual override wins, then
/// per-rig cached choice, then the rc16.2 default.
pub fn lookup_cached_tier_for_current_rig() -> Option<EffectiveCaptureMode> {
    if !is_enabled() {
        return None;
    }
    let cache = CaptureModeCache::load();
    let fingerprint = RigFingerprint::current();
    cache.lookup(&fingerprint)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_enabled_accepts_truthy_forms() {
        // Touching the process env var is racy under cargo test's
        // parallel runner, so we test the pure parser directly. The
        // env read itself is just `var(...).ok().map(parse_enabled)`.
        for truthy in ["1", "true", "TRUE", " on ", "yes", "  Yes  "] {
            assert!(
                parse_enabled(truthy),
                "expected {truthy:?} to be parsed as enabled"
            );
        }
    }

    #[test]
    fn parse_enabled_rejects_everything_else() {
        for falsy in ["", "0", "false", "off", "no", "auto", "garbage", "2"] {
            assert!(
                !parse_enabled(falsy),
                "expected {falsy:?} to be parsed as disabled"
            );
        }
    }

    #[test]
    fn tier_chain_first_tier_is_monitor() {
        // rc16.2's choice is preserved as T1.
        assert_eq!(TIER_CHAIN[0], EffectiveCaptureMode::Monitor);
    }

    #[test]
    fn tier_chain_advances_in_order() {
        assert_eq!(
            next_tier(EffectiveCaptureMode::Monitor),
            Some(EffectiveCaptureMode::Wgc)
        );
        assert_eq!(
            next_tier(EffectiveCaptureMode::Wgc),
            Some(EffectiveCaptureMode::GameHook)
        );
        assert_eq!(next_tier(EffectiveCaptureMode::GameHook), None);
    }

    #[test]
    fn classify_recognizes_black_frames() {
        // 30 frames of YAVG=16 (studio-range pure black), no motion.
        let mut text = String::new();
        for _ in 0..30 {
            text.push_str(
                "[Parsed_metadata_1 @ 0x7f] lavfi.signalstats.YAVG=16.0\n\
                 [Parsed_metadata_1 @ 0x7f] lavfi.signalstats.YDIF=0\n\
                 [Parsed_metadata_1 @ 0x7f] lavfi.signalstats.YHIGH=16\n",
            );
        }
        assert_eq!(classify_signalstats_stream(&text), ProbeResult::Black);
    }

    #[test]
    fn classify_recognizes_healthy_game_capture() {
        // Simulate 30 frames of a real game scene: bright, with motion.
        let mut text = String::new();
        for i in 0..30 {
            let yavg = 90.0 + (i as f64) * 0.3;
            let ydif = 8.0 + (i as f64).sin().abs() * 4.0;
            let yhigh = 220.0 + (i as f64) * 0.1;
            text.push_str(&format!(
                "[Parsed_metadata_1 @ 0x7f] lavfi.signalstats.YAVG={yavg}\n\
                 [Parsed_metadata_1 @ 0x7f] lavfi.signalstats.YDIF={ydif}\n\
                 [Parsed_metadata_1 @ 0x7f] lavfi.signalstats.YHIGH={yhigh}\n",
            ));
        }
        assert_eq!(classify_signalstats_stream(&text), ProbeResult::Healthy);
    }

    #[test]
    fn classify_recognizes_static_screen() {
        // 30 frames of identical bright values — no motion, no variance.
        let mut text = String::new();
        for _ in 0..30 {
            text.push_str(
                "[Parsed_metadata_1 @ 0x7f] lavfi.signalstats.YAVG=120.0\n\
                 [Parsed_metadata_1 @ 0x7f] lavfi.signalstats.YDIF=0\n\
                 [Parsed_metadata_1 @ 0x7f] lavfi.signalstats.YHIGH=200.0\n",
            );
        }
        assert_eq!(classify_signalstats_stream(&text), ProbeResult::Static);
    }

    #[test]
    fn classify_handles_no_samples_as_indeterminate() {
        assert_eq!(classify_signalstats_stream(""), ProbeResult::Indeterminate);
        assert_eq!(
            classify_signalstats_stream("ffmpeg: random unrelated output\n"),
            ProbeResult::Indeterminate
        );
    }

    #[test]
    fn extract_signalstats_value_parses_correctly() {
        let line = "[Parsed_metadata_1 @ 0x7f8aa4007ec0] lavfi.signalstats.YAVG=124.5";
        assert_eq!(extract_signalstats_value(line, "YAVG"), Some(124.5));
        // Mismatched tag returns None
        assert_eq!(extract_signalstats_value(line, "YHIGH"), None);
        // Whitespace terminates the value
        let line2 =
            "[Parsed_metadata_1 @ 0x7f] lavfi.signalstats.YDIF=8.0 lavfi.signalstats.YAVG=124";
        assert_eq!(extract_signalstats_value(line2, "YDIF"), Some(8.0));
        assert_eq!(extract_signalstats_value(line2, "YAVG"), Some(124.0));
    }

    #[test]
    fn fingerprint_is_stable() {
        let a = RigFingerprint::compute("NVIDIA GeForce RTX 4060", "Windows 11 22H2", 2);
        let b = RigFingerprint::compute("nvidia geforce rtx 4060", "windows 11 22h2", 2);
        assert_eq!(a, b, "Fingerprint should be case-insensitive");
        assert_eq!(a.as_str().len(), 16);

        let c = RigFingerprint::compute("NVIDIA GeForce RTX 4060", "Windows 11 22H2", 1);
        assert_ne!(a, c, "Monitor count must change the fingerprint");
    }

    #[test]
    fn cache_roundtrip() {
        // Use a tmpfile so we don't touch the user's actual cache.
        let tmp = std::env::temp_dir().join(format!(
            "oyster-adaptive-cache-roundtrip-{}.json",
            std::process::id()
        ));
        // SAFETY: setting an env var here only affects this test
        // thread's view of the cache path. Other tests don't read this.
        unsafe { std::env::set_var(OYSTER_ADAPTIVE_CACHE_PATH_ENV, &tmp) };

        let fp = RigFingerprint::compute("test-gpu", "test-os", 1);
        let mut cache = CaptureModeCache::default();
        cache.record(&fp, EffectiveCaptureMode::Wgc);
        cache.save();

        let reloaded = CaptureModeCache::load();
        assert_eq!(reloaded.lookup(&fp), Some(EffectiveCaptureMode::Wgc));
        assert_eq!(reloaded.schema_version, 1);

        // Re-recording the same tier increments consecutive_uses.
        let mut cache2 = reloaded;
        cache2.record(&fp, EffectiveCaptureMode::Wgc);
        let entry = cache2.entries.get(fp.as_str()).unwrap();
        assert_eq!(entry.consecutive_uses, 1);

        // Switching tiers resets consecutive_uses to 0.
        cache2.record(&fp, EffectiveCaptureMode::Monitor);
        let entry = cache2.entries.get(fp.as_str()).unwrap();
        assert_eq!(entry.tier, CachedTier::Monitor);
        assert_eq!(entry.consecutive_uses, 0);

        // Invalidation drops the entry.
        cache2.invalidate(&fp);
        assert!(cache2.entries.get(fp.as_str()).is_none());

        let _ = std::fs::remove_file(&tmp);
        unsafe { std::env::remove_var(OYSTER_ADAPTIVE_CACHE_PATH_ENV) };
    }

    #[test]
    fn cache_treats_unparseable_timestamp_as_stale() {
        let entry = CacheEntry {
            tier: CachedTier::Monitor,
            validated_at_iso: "not-a-real-timestamp".to_owned(),
            consecutive_uses: 0,
        };
        assert!(entry.is_stale(30));
    }

    #[test]
    fn cache_treats_old_timestamp_as_stale() {
        // 60 days ago.
        let stale = chrono::Utc::now() - chrono::Duration::days(60);
        let entry = CacheEntry {
            tier: CachedTier::Monitor,
            validated_at_iso: stale.to_rfc3339(),
            consecutive_uses: 0,
        };
        assert!(entry.is_stale(30));
        assert!(!entry.is_stale(90));
    }

    #[test]
    fn decide_keeps_healthy_tier() {
        assert!(matches!(
            decide(ProbeResult::Healthy, EffectiveCaptureMode::Monitor),
            TierDecision::KeepAndCache
        ));
    }

    #[test]
    fn decide_advances_on_black() {
        match decide(ProbeResult::Black, EffectiveCaptureMode::Monitor) {
            TierDecision::Advance(next) => assert_eq!(next, EffectiveCaptureMode::Wgc),
            _ => panic!("Expected Advance"),
        }
    }

    #[test]
    fn decide_advances_on_static() {
        match decide(ProbeResult::Static, EffectiveCaptureMode::Wgc) {
            TierDecision::Advance(next) => assert_eq!(next, EffectiveCaptureMode::GameHook),
            _ => panic!("Expected Advance"),
        }
    }

    #[test]
    fn decide_exhausts_after_last_tier() {
        assert!(matches!(
            decide(ProbeResult::Black, EffectiveCaptureMode::GameHook),
            TierDecision::Exhausted
        ));
    }

    #[test]
    fn decide_keeps_without_caching_on_indeterminate() {
        assert!(matches!(
            decide(ProbeResult::Indeterminate, EffectiveCaptureMode::Monitor),
            TierDecision::KeepWithoutCaching
        ));
    }

    // ---------- PRD-100 Audit I-2 — fullscreen-aware decision ----------

    #[test]
    fn fullscreen_hint_jumps_monitor_black_to_gamehook() {
        // The headline case: NVIDIA + exclusive-fullscreen game +
        // Monitor tier produced Black. We must skip Wgc and go
        // straight to GameHook.
        match decide_with_fullscreen_hint(
            ProbeResult::Black,
            EffectiveCaptureMode::Monitor,
            true,
        ) {
            TierDecision::Advance(next) => assert_eq!(next, EffectiveCaptureMode::GameHook),
            TierDecision::KeepAndCache => panic!("expected Advance(GameHook), got KeepAndCache"),
            TierDecision::Exhausted => panic!("expected Advance(GameHook), got Exhausted"),
            TierDecision::KeepWithoutCaching => {
                panic!("expected Advance(GameHook), got KeepWithoutCaching")
            }
        }
    }

    #[test]
    fn fullscreen_hint_no_shortcut_when_hint_false() {
        // Same probe + tier, but no fullscreen signal. Must behave
        // identically to `decide()` — i.e. advance to Wgc, not GameHook.
        match decide_with_fullscreen_hint(
            ProbeResult::Black,
            EffectiveCaptureMode::Monitor,
            false,
        ) {
            TierDecision::Advance(next) => assert_eq!(next, EffectiveCaptureMode::Wgc),
            _ => panic!("expected Advance(Wgc) when fullscreen hint is false"),
        }
    }

    #[test]
    fn fullscreen_hint_no_shortcut_on_static() {
        // The shortcut is gated on `Black` specifically — `Static`
        // (mean luma fine, no motion) means the capture path works
        // but the screen is frozen. That's not the
        // fullscreen-vs-DXGI problem the shortcut diagnoses, so we
        // must follow the regular advancement path.
        match decide_with_fullscreen_hint(
            ProbeResult::Static,
            EffectiveCaptureMode::Monitor,
            true,
        ) {
            TierDecision::Advance(next) => assert_eq!(next, EffectiveCaptureMode::Wgc),
            _ => panic!("expected Advance(Wgc) on Static even with fullscreen hint"),
        }
    }

    #[test]
    fn fullscreen_hint_no_shortcut_when_tier_already_wgc() {
        // If we somehow landed on Wgc as the first tier (cache or
        // env override) and got Black, the shortcut does NOT
        // apply — Wgc's failure mode is unrelated to the Monitor
        // exclusive-fullscreen failure and we just advance one tier
        // (to GameHook, which is the same as decide() would say).
        match decide_with_fullscreen_hint(ProbeResult::Black, EffectiveCaptureMode::Wgc, true) {
            TierDecision::Advance(next) => assert_eq!(next, EffectiveCaptureMode::GameHook),
            _ => panic!("expected Advance(GameHook) from Wgc"),
        }
    }

    #[test]
    fn fullscreen_hint_keeps_healthy() {
        // A healthy probe is a healthy probe regardless of any hint.
        assert!(matches!(
            decide_with_fullscreen_hint(
                ProbeResult::Healthy,
                EffectiveCaptureMode::Monitor,
                true,
            ),
            TierDecision::KeepAndCache
        ));
    }

    #[test]
    fn fullscreen_hint_keeps_indeterminate() {
        // Indeterminate must not flip-flop the tier — we have no
        // signal and should leave the cache untouched.
        assert!(matches!(
            decide_with_fullscreen_hint(
                ProbeResult::Indeterminate,
                EffectiveCaptureMode::Monitor,
                true,
            ),
            TierDecision::KeepWithoutCaching
        ));
    }

    #[test]
    fn fullscreen_detection_null_hwnd_is_not_fullscreen() {
        // 0 is the sentinel value `spawn_probe_task` forwards when
        // no HWND is known. Must read as "not fullscreen" so the
        // shortcut stays dormant in that case.
        assert!(!detect_fullscreen_exclusive_raw(0));
    }
}
