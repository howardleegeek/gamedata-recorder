use color_eyre::eyre::{Context, Result, eyre};
use constants::encoding::VideoEncoderType;
use serde::{Deserialize, Deserializer, Serialize};
use std::{collections::HashMap, fs, path::PathBuf};

/// Quick probe: does any GPU report NVIDIA as its vendor?
/// Uses `sysinfo` (already a dep) to avoid pulling in extra crates.
fn detect_nvidia_gpu() -> Result<bool> {
    // sysinfo doesn't expose GPU info directly on all platforms; on Windows
    // we can shell out to `wmic path win32_VideoController get Name` and
    // match "NVIDIA". This is a best-effort check — false/error just keeps
    // the existing encoder choice.
    #[cfg(target_os = "windows")]
    {
        let output = std::process::Command::new("wmic")
            .args(["path", "win32_VideoController", "get", "Name"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW — no cmd popup
            .output()
            .wrap_err("wmic probe failed")?;
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text.to_lowercase().contains("nvidia"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(false)
    }
}

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

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

/// Per-game configuration settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GameConfig {
    /// Use window capture instead of game capture for this game.
    /// Default: false — monitor capture is used by default, which works for
    /// fullscreen-exclusive games (like GTA V) where window capture would
    /// attach to the wrong HWND (e.g. Rockstar Games Launcher) or capture
    /// a 1-second heartbeat stream instead of the actual game frames.
    /// v2.5.2: flipped to false after session metadata from nucbox test
    /// proved window capture was hooking the launcher, not the game.
    pub use_window_capture: bool,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            use_window_capture: false, // v2.5.2: monitor capture is now the default
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

#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    #[serde(default)]
    pub api_key: String,
    #[serde(default, deserialize_with = "deserialize_string_bool")]
    pub has_consented: bool,
}
impl Credentials {
    pub fn logout(&mut self) {
        self.api_key = String::new();
        self.has_consented = false;
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

        // v2.5.4 migration: scrub per-game `use_window_capture: true` overrides
        // that v2.5.1 wrote silently on every hook timeout. These overrides
        // forced window capture, which made `find_window_for_pid` hook the
        // recorder's OWN UI window (because the function wrongly returned
        // foreground instead of the real game HWND). On a client machine
        // this produced 4 minutes of recording of our own UI. Strip them.
        let games_to_fix: Vec<String> = config
            .preferences
            .games
            .iter()
            .filter(|(_, cfg)| cfg.use_window_capture)
            .map(|(name, _)| name.clone())
            .collect();
        if !games_to_fix.is_empty() {
            tracing::warn!(
                games = ?games_to_fix,
                "Scrubbing legacy per-game use_window_capture=true overrides \
                 (v2.5.4 migration) — these were written by v2.5.1's \
                 auto-flip-on-hook-timeout bug and force self-capture."
            );
            for name in &games_to_fix {
                if let Some(cfg) = config.preferences.games.get_mut(name) {
                    cfg.use_window_capture = false;
                }
            }
            if let Err(e) = config.save() {
                tracing::error!(
                    e=?e,
                    "Failed to persist config after v2.5.4 migration — \
                     will retry on next save"
                );
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
        // Atomic write: write to temp file then rename to prevent corruption
        // if the process crashes or disk becomes full mid-write.
        let temp_path = config_path.with_extension("json.tmp");
        if let Err(e) = fs::write(&temp_path, &json) {
            tracing::error!(
                "Failed to write config to temp file {}: {e}. Disk may be full or read-only.",
                temp_path.display()
            );
            return Err(e.into());
        }
        if let Err(e) = fs::rename(&temp_path, &config_path) {
            tracing::error!(
                "Failed to rename temp config file: {e}. Falling back to direct write."
            );
            // Fallback: direct write (less safe but better than nothing)
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
