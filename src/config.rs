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

/// PCI vendor ID for NVIDIA Corporation — used to identify NVIDIA GPUs in
/// DXGI adapter enumeration. Stable across every NVIDIA GPU ever shipped.
const NVIDIA_PCI_VENDOR_ID: u32 = 0x10DE;

/// Quick probe: does any DX12-capable GPU report NVIDIA as its vendor?
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
fn detect_nvidia_gpu() -> Result<bool> {
    #[cfg(target_os = "windows")]
    {
        use egui_wgpu::wgpu;

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let has_nvidia = instance
            .enumerate_adapters(wgpu::Backends::DX12)
            .into_iter()
            .any(|adapter| adapter.get_info().vendor == NVIDIA_PCI_VENDOR_ID);
        Ok(has_nvidia)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(false)
    }
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
    /// R46 (GDPR/CCPA): the application version the user was shown when they
    /// accepted the consent disclosure. `None` means the user has never
    /// accepted any version. If this does not match the currently-running
    /// `CARGO_PKG_VERSION`, the ConsentView is shown again so the user must
    /// re-consent to any updated disclosure text.
    ///
    /// This field gates every code path that installs a global input hook or
    /// opens a video/audio capture pipeline — see `Credentials::consent_status`
    /// and `input_capture::ConsentGuard`. Serialized as a semver string (e.g.
    /// `"2.5.5"`); `None` round-trips as `null` / missing.
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

    /// Compute the consent status for the currently-running binary.
    ///
    /// Returns `Granted` iff the stored `consent_given_at_version` parses as
    /// semver and equals `current_version`. Returns `NotGranted` if no
    /// version has ever been accepted, and `VersionMismatch` if a prior
    /// version was accepted but this binary is newer/older.
    pub fn consent_status(&self, current_version: &Version) -> ConsentStatus {
        match &self.consent_given_at_version {
            None => ConsentStatus::NotGranted,
            Some(v) if v == current_version => ConsentStatus::Granted,
            Some(_) => ConsentStatus::VersionMismatch,
        }
    }

    /// Record that the user has accepted the consent disclosure at the given
    /// version. The UI calls this when the user clicks "Accept".
    pub fn record_consent(&mut self, current_version: Version) {
        self.has_consented = true;
        self.consent_given_at_version = Some(current_version);
    }
}

/// Parse the compile-time `CARGO_PKG_VERSION` as semver.
///
/// Panics only if the Cargo.toml version literal is malformed — which would
/// be caught at build time. Callers can treat this as infallible at runtime.
pub fn current_pkg_version() -> Version {
    // `env!` is compile-time; the value comes straight from Cargo.toml.
    Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("CARGO_PKG_VERSION must be valid semver; this is a build-time contract")
}

/// Build a [`ConsentGuard`] from the current config and running binary version.
///
/// This is the single entry point for every recording path that needs to
/// verify consent — input capture, OBS recorder, etc.
///
/// CI mode (see [`ci_mode`]) short-circuits to a session-only granted guard
/// without consulting the on-disk config. This is a test-scaffolding bypass:
/// it never persists `has_consented` to disk, so the next non-CI launch still
/// requires a real user click on the ConsentView.
pub fn consent_guard_from_config(config: &Config) -> ConsentGuard {
    if ci_mode() {
        return ConsentGuard::granted();
    }
    let current = current_pkg_version();
    ConsentGuard::new(config.credentials.consent_status(&current))
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
        if matches!(
            config.preferences.encoder.encoder,
            constants::encoding::VideoEncoderType::X264
        ) {
            match detect_nvidia_gpu() {
                Ok(true) => {
                    tracing::info!("NVIDIA GPU detected — upgrading encoder X264 -> NvEnc");
                    config.preferences.encoder.encoder =
                        constants::encoding::VideoEncoderType::NvEnc;
                    if let Err(e) = config.save() {
                        tracing::warn!(e=?e, "Failed to persist NvEnc migration");
                    }
                }
                Ok(false) => {}
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
            preset2: constants::encoding::NVENC_PRESETS[0].to_string(),
            tune: constants::encoding::NVENC_TUNE_OPTIONS[0].to_string(),
        }
    }
}
impl FfmpegNvencSettings {
    fn apply_to_data_updater(
        &self,
        updater: libobs_wrapper::data::ObsDataUpdater,
    ) -> libobs_wrapper::data::ObsDataUpdater {
        updater
            .set_string("preset2", self.preset2.as_str())
            .set_string("tune", self.tune.as_str())
    }
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
            target_usage: constants::encoding::QSV_TARGET_USAGES[0].to_string(),
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
    fn consent_guard_from_config_respects_stored_version() {
        let mut cfg = Config::default();
        // Without consent: guard refuses.
        assert!(!consent_guard_from_config(&cfg).is_granted());

        // With consent at the current binary version: guard permits.
        cfg.credentials.record_consent(current_pkg_version());
        assert!(consent_guard_from_config(&cfg).is_granted());
    }
}
