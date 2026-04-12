use std::path::PathBuf;
use std::sync::OnceLock;

// if run in dev mode will be relative to project root. When run from .exe will be relative to install directory.
fn get_asset_path(filename: &str) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        tracing::error!("Failed to get current dir: {e}, using fallback");
        PathBuf::from(".")
    });
    cwd.join("assets").join(filename)
}

struct AssetData(OnceLock<Vec<u8>>);
impl AssetData {
    const fn new() -> Self {
        Self(OnceLock::new())
    }

    pub fn get(&'static self, filename: &'static str) -> Option<&'static [u8]> {
        self.0
            .get_or_init(move || match std::fs::read(get_asset_path(filename)) {
                Ok(data) => data,
                Err(e) => {
                    tracing::error!("Failed to load asset {filename}: {e}");
                    Vec::new()
                }
            })
            .as_slice()
            .into()
    }
}

pub fn get_logo_default_bytes() -> Option<&'static [u8]> {
    static DATA: AssetData = AssetData::new();
    DATA.get("owl-logo.png")
}

pub fn get_logo_recording_bytes() -> Option<&'static [u8]> {
    static DATA: AssetData = AssetData::new();
    DATA.get("owl-logo-recording.png")
}

/// Loads an arbitrary audio cue from the assets/cues/ directory.
/// Falls back to default_start.mp3 if the requested cue fails to load.
pub fn load_cue_bytes(filename: &str) -> Option<Vec<u8>> {
    let path = format!("cues/{filename}");
    match std::fs::read(get_asset_path(&path)) {
        Ok(data) => Some(data),
        Err(e) => {
            // Try to fallback to default_start.mp3
            if filename != "default_start.mp3" {
                tracing::warn!("Failed to load {path}: {e}, falling back to default_start.mp3");
                let default_path = "cues/default_start.mp3";
                match std::fs::read(get_asset_path(default_path)) {
                    Ok(data) => Some(data),
                    Err(e2) => {
                        tracing::error!("Failed to load fallback {default_path}: {e2}");
                        None
                    }
                }
            } else {
                tracing::error!("Failed to load {path}: {e}");
                None
            }
        }
    }
}

/// Scans the cues folder and returns a list of available MP3 files
pub fn get_available_cues() -> Vec<String> {
    let cues_path = get_asset_path("cues");

    let mut cues = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&cues_path) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type()
                && file_type.is_file()
                && let Some(filename) = entry.file_name().to_str()
                && (filename.ends_with(".mp3") || filename.ends_with(".ogg"))
            {
                cues.push(filename.to_string());
            }
        }
    }

    // Sort alphabetically for consistent ordering
    cues.sort();
    cues
}

/// Loads icon data from bytes and returns the rgba data and dimensions
pub fn load_icon_data_from_bytes(bytes: &[u8]) -> (Vec<u8>, (u32, u32)) {
    let image = image::load_from_memory(bytes)
        .expect("Failed to load embedded icon")
        .into_rgba8();
    let dimensions = image.dimensions();
    let rgba = image.into_raw();
    (rgba, dimensions)
}
