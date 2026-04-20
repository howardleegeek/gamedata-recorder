use crate::config::{GameConfig, Preferences};
use constants::unsupported_games::{InstalledGame, UnsupportedGames, detect_installed_games};
use egui::{
    Align, Align2, Button, Color32, Context, CursorIcon, Frame, Label, Layout, RichText,
    ScrollArea, Sense, Ui, Vec2, Window, vec2,
};

const FONTSIZE: f32 = 13.0;
const DEFAULT_WIDTH: f32 = 500.0;
const DEFAULT_HEIGHT: f32 = 600.0;

#[derive(Default)]
pub struct GamesWindowState {
    pub open: bool,
    pub installed_list: egui_virtual_list::VirtualList,
    /// Currently open game settings window (stores the game name and primary exe)
    pub game_settings_open: Option<GameSettingsTarget>,
}

/// Identifies which game's settings window is open
#[derive(Clone)]
pub struct GameSettingsTarget {
    pub game_name: String,
    pub binaries: Vec<String>,
}

pub fn window(
    ctx: &Context,
    state: &mut GamesWindowState,
    unsupported_games: &UnsupportedGames,
    preferences: &mut Preferences,
    last_recordable_game: Option<&str>,
) {
    // Always render the game settings window if it's open
    game_settings_window(ctx, &mut state.game_settings_open, preferences);

    if !state.open {
        return;
    }

    let installed = detect_installed_games();

    // Filter out games whose names match entries in UnsupportedGames (case-insensitive)
    let supported_installed: Vec<_> = installed
        .into_iter()
        .filter(|game| {
            !unsupported_games
                .games
                .iter()
                .any(|ug| ug.name.to_lowercase() == game.name.to_lowercase())
        })
        .collect();

    let mut should_close = false;

    egui::Window::new("Games")
        .default_size([DEFAULT_WIDTH, DEFAULT_HEIGHT])
        .resizable(true)
        .open(&mut state.open)
        .show(ctx, |ui| {
            // Show a settings button for the last recordable game
            if let Some(exe_name) = last_recordable_game {
                let exe_without_ext = std::path::Path::new(exe_name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(exe_name);

                let has_custom_settings = preferences.games.contains_key(exe_without_ext);
                let settings_label = if has_custom_settings {
                    format!("Configure {exe_name} (custom settings active)")
                } else {
                    format!("Configure {exe_name}")
                };

                if ui.button(&settings_label).clicked() {
                    state.game_settings_open = Some(GameSettingsTarget {
                        game_name: exe_name.to_string(),
                        binaries: vec![exe_without_ext.to_lowercase()],
                    });
                }

                ui.separator();
            }

            ScrollArea::vertical().show(ui, |ui| {
                if supported_installed.is_empty() {
                    ui.label("No supported installed Steam games found.");
                } else {
                    state.installed_list.ui_custom_layout(
                        ui,
                        supported_installed.len(),
                        |ui, index| {
                            if let Some(game) = supported_installed.get(index) {
                                if game_entry(ui, game) {
                                    should_close = true;
                                }
                                1
                            } else {
                                0
                            }
                        },
                    );
                }
            });
        });

    if should_close {
        state.open = false;
    }
}

/// Returns true if the game was launched (to close the window).
fn game_entry(ui: &mut Ui, game: &InstalledGame) -> bool {
    let mut launched = false;

    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(4.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // Game name (clickable - opens Steam store page)
                let game_response = ui
                    .add(
                        Label::new(
                            RichText::new(&game.name)
                                .size(FONTSIZE)
                                .color(ui.visuals().text_color())
                                .underline(),
                        )
                        .sense(Sense::click()),
                    )
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text("Open Steam store page");
                if game_response.clicked() {
                    let url = format!("https://store.steampowered.com/app/{}/", game.steam_app_id);
                    opener::open_browser(&url).ok();
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let response = ui
                        .add_sized(
                            vec2(60.0, 20.0),
                            Button::new(
                                RichText::new("Launch")
                                    .size(FONTSIZE * 0.85)
                                    .color(Color32::WHITE),
                            )
                            .fill(Color32::from_rgb(60, 120, 180)),
                        )
                        .on_hover_text("Launch game via Steam");
                    if response.clicked() {
                        let steam_launch_url = format!("steam://rungameid/{}", game.steam_app_id);
                        opener::open(&steam_launch_url).ok();
                        launched = true;
                    }
                });
            });
        });

    launched
}

fn game_settings_window(
    ctx: &Context,
    game_settings_open: &mut Option<GameSettingsTarget>,
    preferences: &mut Preferences,
) {
    let Some(target) = game_settings_open.clone() else {
        return;
    };

    if target.binaries.is_empty() {
        return;
    }

    let hover_text = concat!(
        "Screen capture records the entire display and works with all games, ",
        "including those with anti-cheat software. This is the default mode.\n\n",
        "Game capture uses hook injection for potentially better performance, ",
        "but may fail with anti-cheat software. Only disable screen capture if ",
        "you're experiencing issues with the default mode."
    );

    let mut keep_open = true;
    Window::new(format!("{} Settings", target.game_name))
        .open(&mut keep_open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                // Get the current config from the primary binary
                let mut config = get_primary_game_config(preferences, &target.binaries);

                let mut changed = false;

                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut config.use_window_capture, "Use Screen Capture (Default)")
                        .changed()
                    {
                        changed = true;
                    }
                    ui.label(
                        RichText::new("(?)")
                            .size(12.0)
                            .color(Color32::from_rgb(150, 150, 150)),
                    )
                    .on_hover_text(hover_text);
                });

                // Sync to all binaries if anything changed
                if changed {
                    sync_game_config(preferences, &target.binaries, &config);
                }

                ui.add_space(8.0);

                // Reset button
                if ui
                    .add_sized(
                        vec2(ui.available_width(), 28.0),
                        Button::new(RichText::new("Reset to Defaults").size(12.0)),
                    )
                    .clicked()
                {
                    // Remove all config entries for this game's binaries
                    for exe in &target.binaries {
                        preferences.games.remove(exe);
                    }
                }
            });
        });

    if !keep_open {
        *game_settings_open = None;
    }
}

/// Get the game config for the primary binary (first in the list).
/// Returns a clone of the config to avoid borrow issues.
fn get_primary_game_config(preferences: &Preferences, binaries: &[String]) -> GameConfig {
    binaries
        .first()
        .and_then(|exe| preferences.games.get(exe))
        .cloned()
        .unwrap_or_default()
}

/// Sync game config across all binaries for a game.
/// Always writes to all binaries to ensure consistency.
fn sync_game_config(preferences: &mut Preferences, binaries: &[String], config: &GameConfig) {
    for exe in binaries {
        preferences.games.insert(exe.clone(), config.clone());
    }
}
