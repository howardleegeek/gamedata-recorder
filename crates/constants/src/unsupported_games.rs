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
    pub fn load_from_str(s: &str) -> serde_json::Result<Self> {
        let games: Vec<UnsupportedGame> = serde_json::from_str(s)?;
        Ok(Self { games })
    }

    /// Do not use this unless you're sure you don't need a more up-to-date version.
    pub fn load_from_embedded() -> Self {
        Self::load_from_str(include_str!("unsupported_games.json"))
            .expect("Failed to load unsupported games from embedded data")
    }

    pub fn get(&self, game_exe_without_ext: &str) -> Option<&UnsupportedGame> {
        let game_exe_without_ext = game_exe_without_ext.to_lowercase();
        self.games.iter().find(|g| {
            g.binaries.iter().any(|b| {
                let b_lower = b.to_lowercase();
                // Exact match or exe has a suffix (e.g., _dx12, -win64-shipping), or epic games store variant
                game_exe_without_ext == b_lower
                    || (game_exe_without_ext.starts_with(&b_lower)
                        && game_exe_without_ext[b_lower.len()..].starts_with('_'))
                    || (game_exe_without_ext.starts_with(&b_lower)
                        && game_exe_without_ext[b_lower.len()..].starts_with('-'))
                    || (game_exe_without_ext.starts_with(&b_lower)
                        && game_exe_without_ext[b_lower.len()..].starts_with("epicgamesstore"))
            })
        })
    }
}

pub struct InstalledGame {
    pub name: String,
    pub steam_app_id: u32,
}

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
                tracing::warn!("Failed to read app");
                continue;
            };
            if let Some(name) = app.name {
                installed.push(InstalledGame {
                    name,
                    steam_app_id: app.app_id,
                });
            }
        }
    }
    installed
}
