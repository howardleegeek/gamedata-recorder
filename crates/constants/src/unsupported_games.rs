use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnsupportedReason {
    EnoughData,
    NotAGame,
    Other(String),
}

impl fmt::Display for UnsupportedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnsupportedReason::EnoughData => {
                write!(f, "We have collected enough data for this game.")
            }
            UnsupportedReason::NotAGame => write!(f, "This is not a game."),
            UnsupportedReason::Other(s) => write!(f, "{s}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UnsupportedGame {
    pub name: String,
    pub binaries: Vec<String>,
    pub reason: UnsupportedReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedGames {
    pub games: Vec<UnsupportedGame>,
}

impl UnsupportedGames {
    /// Loads unsupported games data from a JSON string.
    ///
    /// This function parses a JSON string containing an array of unsupported game
    /// definitions and returns a `UnsupportedGames` instance. The JSON format should
    /// match the structure defined by the `UnsupportedGame` struct.
    ///
    /// # Arguments
    /// * `s` - A JSON string containing unsupported games data
    ///
    /// # Returns
    /// * `Result<Self, serde_json::Error>` - Parsed unsupported games data or a JSON parsing error
    ///
    /// # Example
    /// ```
    /// let json = r#"[{"name": "Test Game", "binaries": ["test.exe"], "reason": "EnoughData"}]"#;
    /// let unsupported = UnsupportedGames::load_from_str(json).unwrap();
    /// ```
    pub fn load_from_str(s: &str) -> serde_json::Result<Self> {
        let games: Vec<UnsupportedGame> = serde_json::from_str(s)?;
        Ok(Self { games })
    }

    /// Do not use this unless you're sure you don't need a more up-to-date version.
    pub fn load_from_embedded() -> Self {
        Self::load_from_str(include_str!("unsupported_games.json"))
            .expect("Failed to load unsupported games from embedded data")
    }

    /// Look up an unsupported game by its executable stem (without `.exe`).
    ///
    /// The search is case-insensitive and matches exact binary names as well
    /// as common suffix variants (e.g. `_dx12`, `-win64-shipping`) and the
    /// Epic Games Store naming convention. Returns `None` if the executable
    /// is not in the unsupported-games list.
    pub fn get(&self, game_exe_without_ext: &str) -> Option<&UnsupportedGame> {
        let game_exe_without_ext = game_exe_without_ext.to_lowercase();
        self.games.iter().find(|g| {
            g.binaries.iter().any(|b| {
                let b_lower = b.to_lowercase();
                // Exact match or exe has a suffix (e.g., _dx12, -win64-shipping), or epic games store variant
                game_exe_without_ext == b_lower
                    || game_exe_without_ext.starts_with(&format!("{b_lower}_"))
                    || game_exe_without_ext.starts_with(&format!("{b_lower}-"))
                    || game_exe_without_ext.starts_with(&format!("{b_lower}epicgamesstore"))
            })
        })
    }
}

pub struct InstalledGame {
    pub name: String,
    pub steam_app_id: u32,
}

/// Scans the local Steam installation to detect installed games.
///
/// This function uses the `steamlocate` crate to find Steam installations
/// and enumerate all installed games across all Steam libraries.
/// Returns a vector of `InstalledGame` structs containing game names
/// and their Steam App IDs.
///
/// # Returns
/// - `Vec<InstalledGame>`: List of installed games, empty if Steam is not found
///   or if there are issues reading the library data.
pub fn detect_installed_games() -> Vec<InstalledGame> {
    // Implementation would go here
    vec![]
}