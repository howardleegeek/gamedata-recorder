use color_eyre::eyre::{eyre, Context, Result};
use constants::encoding::VideoEncoderType;
use serde::{Deserialize, Deserializer, Serialize};
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::{collections::HashMap, fs, path::PathBuf};

/// Maximum allowed length for API key to prevent DoS from malicious config files
const MAX_API_KEY_LENGTH: usize = 2048;
/// Maximum allowed length for hotkey strings to prevent DoS from malicious config files
const MAX_HOTKEY_LENGTH: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
// camel case renames are legacy from old existing configs, we want it to be backwards-compatible with previous owl releases that used electron
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(
        default = "default_start_key",
        deserialize_with = "validate_hotkey_length"
    )]
    pub start_recording_key: String,
    #[serde(
        default = "default_stop_key",
        deserialize_with = "validate_hotkey_length"
    )]
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

/// Validates that the audio cue filename doesn't contain path traversal sequences.
/// Returns an error if the filename contains parent directory references (..) or
/// path separators that could escape the intended cues directory.
fn validate_audio_cue_filename<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let filename = String::deserialize(deserializer)?;

    // Reject filenames containing parent directory references or path separators
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return Err(Error::custom(
            "Audio cue filename cannot contain path traversal sequences or directory separators",
        ));
    }

    Ok(filename)
}

/// Audio cue settings for recording events
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioCues {
    #[serde(deserialize_with = "validate_audio_cue_filename")]
    pub start_recording: String,
    #[serde(deserialize_with = "validate_audio_cue_filename")]
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
/// references that could escape the intended directory structure, or if the path
/// is empty or whitespace-only.
fn validate_recording_path<'de, D>(deserializer: D) -> Result<std::path::PathBuf, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let path = PathBuf::deserialize(deserializer)?;

    // Reject empty paths that would be invalid or confusing
    if path.as_os_str().is_empty() {
        return Err(Error::custom("Recording location cannot be empty"));
    }

    // Reject paths that are whitespace-only (can't be meaningfully used)
    if path.to_str().map(|s| s.trim().is_empty()).unwrap_or(false) {
        return Err(Error::custom(
            "Recording location cannot be whitespace-only",
        ));
    }

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

/// Validates API key length to prevent DoS from malicious config files with
/// extremely large strings that could cause memory exhaustion.
fn validate_api_key_length<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let key = String::deserialize(deserializer)?;

    if key.len() > MAX_API_KEY_LENGTH {
        return Err(Error::custom(format!(
            "API key exceeds maximum length of {} characters",
            MAX_API_KEY_LENGTH
        )));
    }

    Ok(key)
}

/// Validates hotkey string length to prevent DoS from malicious config files with
/// extremely large strings that could cause memory exhaustion.
fn validate_hotkey_length<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let key = String::deserialize(deserializer)?;

    if key.len() > MAX_HOTKEY_LENGTH {
        return Err(Error::custom(format!(
            "Hotkey exceeds maximum length of {} characters",
            MAX_HOTKEY_LENGTH
        )));
    }

    Ok(key)
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
    #[serde(default, deserialize_with = "validate_api_key_length")]
    pub api_key: String,
    #[serde(default, deserialize_with = "deserialize_string_bool")]
    pub has_consented: bool,
}
impl Credentials {
    pub fn logout(&mut self) {
        self.api_key = String::new();
        self.has_consented = false;
    }

    /// Sets the API key with validation to prevent DoS from maliciously large strings.
    /// Returns true if the key was set successfully, false if it exceeds maximum length.
    pub fn set_api_key(&mut self, key: String) -> bool {
        if key.len() > MAX_API_KEY_LENGTH {
            tracing::warn!(
                "API key exceeds maximum length of {} characters, rejecting",
                MAX_API_KEY_LENGTH
            );
            return false;
        }
        self.api_key = key;
        true
    }
}

/// The directory in which all persistent config data should be stored.
pub fn get_persistent_dir() -> Result<PathBuf> {
    tracing::debug!("get_persistent_dir() called");
    let dir = dirs::data_dir()
        .ok_or_else(|| eyre!("Could not find user data directory"))?
        .join("GameData Recorder");

    // Create directory with restrictive permissions atomically on Unix to prevent
    // TOCTOU race condition where directory is temporarily accessible with default
    // permissions before we can chmod it. On non-Unix, use standard creation.
    #[cfg(unix)]
    {
        use std::fs::DirBuilder;
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        builder.create(&dir)?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(&dir)?;
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

        // Ensure data is flushed to disk before renaming to prevent data loss
        // on system crash or power failure
        let temp_file =
            fs::File::open(&temp_path).context("Failed to open temporary file for sync")?;
        temp_file
            .sync_all()
            .context("Failed to sync temporary file to disk")?;
        drop(temp_file);

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

/// Validates that the x264 preset value is one of the valid options from
/// X264_PRESETS. Returns an error if the preset is not in the allowed list.
fn validate_x264_preset<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let preset = String::deserialize(deserializer)?;

    if !constants::encoding::X264_PRESETS.contains(&preset.as_str()) {
        return Err(Error::custom(format!(
            "Invalid x264 preset '{}'. Valid options are: {:?}",
            preset,
            constants::encoding::X264_PRESETS
        )));
    }

    Ok(preset)
}

/// Validates that the x264 tune value is one of the valid options from
/// X264_TUNE_OPTIONS. Returns an error if the tune value is not in the allowed list.
fn validate_x264_tune<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let tune = String::deserialize(deserializer)?;

    if !constants::encoding::X264_TUNE_OPTIONS.contains(&tune.as_str()) {
        return Err(Error::custom(format!(
            "Invalid x264 tune '{}'. Valid options are: {:?}",
            tune,
            constants::encoding::X264_TUNE_OPTIONS
        )));
    }

    Ok(tune)
}

/// Validates that the NVENC preset2 value is one of the valid options from
/// NVENC_PRESETS. Returns an error if the preset is not in the allowed list.
fn validate_nvenc_preset2<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let preset = String::deserialize(deserializer)?;

    if !constants::encoding::NVENC_PRESETS.contains(&preset.as_str()) {
        return Err(Error::custom(format!(
            "Invalid NVENC preset '{}'. Valid options are: {:?}",
            preset,
            constants::encoding::NVENC_PRESETS
        )));
    }

    Ok(preset)
}

/// Validates that the NVENC tune value is one of the valid options from
/// NVENC_TUNE_OPTIONS. Returns an error if the tune value is not in the allowed list.
fn validate_nvenc_tune<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let tune = String::deserialize(deserializer)?;

    if !constants::encoding::NVENC_TUNE_OPTIONS.contains(&tune.as_str()) {
        return Err(Error::custom(format!(
            "Invalid NVENC tune '{}'. Valid options are: {:?}",
            tune,
            constants::encoding::NVENC_TUNE_OPTIONS
        )));
    }

    Ok(tune)
}

/// Validates that the QSV target_usage value is one of the valid options from
/// QSV_TARGET_USAGES. Returns an error if the target_usage is not in the allowed list.
fn validate_qsv_target_usage<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let target_usage = String::deserialize(deserializer)?;

    if !constants::encoding::QSV_TARGET_USAGES.contains(&target_usage.as_str()) {
        return Err(Error::custom(format!(
            "Invalid QSV target_usage '{}'. Valid options are: {:?}",
            target_usage,
            constants::encoding::QSV_TARGET_USAGES
        )));
    }

    Ok(target_usage)
}

/// Validates that the AMF preset value is one of the valid options from
/// AMF_PRESETS. Returns an error if the preset is not in the allowed list.
fn validate_amf_preset<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let preset = String::deserialize(deserializer)?;

    if !constants::encoding::AMF_PRESETS.contains(&preset.as_str()) {
        return Err(Error::custom(format!(
            "Invalid AMF preset '{}'. Valid options are: {:?}",
            preset,
            constants::encoding::AMF_PRESETS
        )));
    }

    Ok(preset)
}

/// OBS x264 (CPU) encoder specific settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ObsX264Settings {
    #[serde(deserialize_with = "validate_x264_preset")]
    pub preset: String,
    #[serde(deserialize_with = "validate_x264_tune")]
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
    #[serde(deserialize_with = "validate_nvenc_preset2")]
    pub preset2: String,
    #[serde(deserialize_with = "validate_nvenc_tune")]
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
    #[serde(deserialize_with = "validate_qsv_target_usage")]
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
    #[serde(deserialize_with = "validate_amf_preset")]
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
