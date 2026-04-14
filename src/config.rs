use color_eyre::eyre::{eyre, Context, Result};
use constants::encoding::VideoEncoderType;
use serde::{Deserialize, Deserializer, Serialize};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{collections::HashMap, fs, path::PathBuf};

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
    #[serde(
        default = "default_recording_location",
        deserialize_with = "validate_recording_path"
    )]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct GameConfig {
    /// Use window capture instead of game capture for this game
    pub use_window_capture: bool,
}

/// by default now start and stop recording are mapped to same key
/// f5 instead of f4 so users can alt+f4 properly.
fn default_start_key() -> String {
    "F5".to_string()
}
fn default_stop_key() -> String {
    "F5".to_string()
}
fn default_opacity() -> u8 {
    85
}
fn default_honk_volume() -> u8 {
    255
}
fn default_recording_location() -> std::path::PathBuf {
    std::path::PathBuf::from("./data_dump/games")
}

/// Validates that the recording path doesn't contain path traversal sequences
/// that could allow writing to unintended system locations. Returns an error
/// if the path contains suspicious patterns like absolute paths or parent directory
/// references that could escape the intended directory structure.
fn validate_recording_path<'de, D>(deserializer: D) -> Result<std::path::PathBuf, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let path = PathBuf::deserialize(deserializer)?;

    // Reject absolute paths which could target any system location
    if path.is_absolute() {
        return Err(Error::custom(
            "Recording location must be a relative path, not absolute",
        ));
    }

    // Check for parent directory references that could escape the intended directory
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::custom(
            "Recording location cannot contain parent directory references (..)",
        ));
    }

    Ok(path)
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
}

/// The directory in which all persistent config data should be stored.
pub fn get_persistent_dir() -> Result<PathBuf> {
    tracing::debug!("get_persistent_dir() called");
    let dir = dirs::data_dir()
        .ok_or_else(|| eyre!("Could not find user data directory"))?
        .join("GameData Recorder");
    fs::create_dir_all(&dir)?;
    // Set restrictive permissions on Unix to protect sensitive config data (API keys)
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(0o700);
        fs::set_permissions(&dir, permissions)?;
    }
    tracing::debug!("Persistent dir: {:?}", dir);
    Ok(dir)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Config {
    #[serde(default)]
    pub credentials: Credentials,
    #[serde(default)]
    pub preferences: Preferences,
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
        let contents = fs::read_to_string(&config_path).context("Failed to read config file")?;
        tracing::debug!("Parsing config file");
        let mut config =
            serde_json::from_str::<Config>(&contents).context("Failed to parse config file")?;

        // Ensure hotkeys have default values if not set
        if config.preferences.start_recording_key.is_empty() {
            config.preferences.start_recording_key = default_start_key();
        }
        if config.preferences.stop_recording_key.is_empty() {
            config.preferences.stop_recording_key = default_stop_key();
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

        // Write to a temporary file first, then rename atomically to avoid corruption
        // if the process crashes or system loses power during the write
        let temp_path = config_path.with_extension("tmp");
        fs::write(&temp_path, serde_json::to_string_pretty(&self)?)
            .context("Failed to write temporary config file")?;
        fs::rename(&temp_path, &config_path)
            .context("Failed to rename temporary config file to final location")?;

        Ok(())
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

        // HEVC encoders use "main" profile, H.264 encoders use "high" profile
        let profile = if self.encoder.is_hevc() {
            constants::encoding::HEVC_VIDEO_PROFILE
        } else {
            constants::encoding::H264_VIDEO_PROFILE
        };

        updater = updater
            .set_int("bitrate", constants::encoding::BITRATE)
            .set_string("rate_control", constants::encoding::RATE_CONTROL)
            .set_string("profile", profile)
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
