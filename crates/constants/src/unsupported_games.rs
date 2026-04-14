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

/// Maximum JSON input size for unsupported games list (10MB)
const MAX_GAMES_JSON_SIZE: usize = 10 * 1024 * 1024;

impl UnsupportedGames {
    pub fn load_from_str(s: &str) -> serde_json::Result<Self> {
        if s.len() > MAX_GAMES_JSON_SIZE {
            return Err(serde::de::Error::custom(
                "Input exceeds maximum allowed size for games JSON",
            ));
        }
        let games: Vec<UnsupportedGame> = serde_json::from_str(s)?;
        Ok(Self { games })
    }

    /// Do not use this unless you're sure you don't need a more up-to-date version.
    pub fn load_from_embedded() -> Self {
        Self::load_from_str(include_str!("unsupported_games.json"))
            .expect("Failed to load unsupported games from embedded data")
    }

    pub fn get(&self, game_exe_without_ext: &str) -> Option<&UnsupportedGame> {
        self.games.iter().find(|g| {
            g.binaries.iter().any(|b| {
                // Case-insensitive comparison without allocation
                game_exe_without_ext.eq_ignore_ascii_case(b)
                    || (game_exe_without_ext.len() > b.len()
                        && game_exe_without_ext.is_char_boundary(b.len())
                        && game_exe_without_ext
                            .get(..b.len())
                            .map_or(false, |prefix| prefix.eq_ignore_ascii_case(b))
                        && game_exe_without_ext
                            .get(b.len()..)
                            .map_or(false, |suffix| suffix.starts_with('_')))
                    || (game_exe_without_ext.len() > b.len()
                        && game_exe_without_ext.is_char_boundary(b.len())
                        && game_exe_without_ext
                            .get(..b.len())
                            .map_or(false, |prefix| prefix.eq_ignore_ascii_case(b))
                        && game_exe_without_ext
                            .get(b.len()..)
                            .map_or(false, |suffix| suffix.starts_with('-')))
                    || (game_exe_without_ext.len()
                        >= b.len().saturating_add("epicgamesstore".len())
                        && game_exe_without_ext.is_char_boundary(b.len())
                        && game_exe_without_ext.is_char_boundary(b.len() + "epicgamesstore".len())
                        && game_exe_without_ext
                            .get(..b.len())
                            .map_or(false, |prefix| prefix.eq_ignore_ascii_case(b))
                        && game_exe_without_ext
                            .get(b.len()..b.len() + "epicgamesstore".len())
                            .map_or(false, |suffix| {
                                suffix.eq_ignore_ascii_case("epicgamesstore")
                            }))
            })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledGame {
    pub name: String,
    pub steam_app_id: u32,
}

/// Maximum number of installed games to detect (prevents memory exhaustion from huge libraries)
const MAX_INSTALLED_GAMES: usize = 10_000;

pub fn detect_installed_games() -> Vec<InstalledGame> {
    let Ok(steam_dir) = steamlocate::SteamDir::locate() else {
        tracing::warn!("Steam installation not found");
        return vec![];
    };

    let Ok(libraries) = steam_dir.libraries() else {
        tracing::warn!("Failed to read Steam libraries");
        return vec![];
    };

    let mut installed = vec![];
    for lib in libraries {
        let Ok(library) = lib else {
            tracing::warn!("Failed to read Steam library");
            continue;
        };
        for app in library.apps() {
            let Ok(app) = app else {
                tracing::warn!("Failed to read Steam app from library");
                continue;
            };
            if installed.len() >= MAX_INSTALLED_GAMES {
                tracing::warn!(
                    "Reached maximum limit of {} installed games, stopping detection",
                    MAX_INSTALLED_GAMES
                );
                return installed;
            }
            if let Some(name) = app.name {
                installed.push(InstalledGame {
                    name,
                    steam_app_id: app.app_id,
                });
            } else {
                tracing::debug!(app_id = app.app_id, "Skipping Steam app with missing name");
            }
        }
    }
    installed
}
