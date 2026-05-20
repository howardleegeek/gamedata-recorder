//! System-tray icon for the gamedata-recorder.
//!
//! Three visual states:
//! - **gray** (default / idle)
//! - **red**  (recording)
//! - **blue** (uploading)
//!
//! Right-click menu: **Open dashboard** | **Pause** | **Exit**

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use color_eyre::eyre::{self, Context as _};
use tray_icon::{
    TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};

use crate::assets;

// ---------------------------------------------------------------------------
// Icon data — generated at runtime from PNG assets (or fallback 1×1 pixels)
// ---------------------------------------------------------------------------

/// 16×16 single-channel icons rendered as RGBA.
fn make_icon(r: u8, g: u8, b: u8) -> tray_icon::Icon {
    let size = 16u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for _ in 0..(size * size) {
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    tray_icon::Icon::from_rgba(rgba, size, size).expect("valid icon")
}

fn load_icon_from_asset(bytes: &[u8]) -> Option<tray_icon::Icon> {
    let (rgba, (w, h)) = assets::load_icon_data_from_bytes(bytes)?;
    tray_icon::Icon::from_rgba(rgba, w, h).ok()
}

fn load_or_fallback(bytes_opt: Option<&[u8]>, r: u8, g: u8, b: u8) -> tray_icon::Icon {
    bytes_opt
        .and_then(load_icon_from_asset)
        .unwrap_or_else(|| make_icon(r, g, b))
}

// ---------------------------------------------------------------------------
// Tray state
// ---------------------------------------------------------------------------

pub struct Tray {
    icon: TrayIcon,
    pause_item_id: MenuId,
    exit_item_id: MenuId,
    dashboard_item_id: MenuId,

    idle_icon: tray_icon::Icon,
    recording_icon: tray_icon::Icon,
    uploading_icon: tray_icon::Icon,
}

impl Tray {
    pub fn new(
        on_dashboard: impl Fn() + Send + 'static,
        on_pause: Arc<dyn Fn() + Send + Sync>,
        on_exit: Arc<dyn Fn() + Send + Sync>,
    ) -> eyre::Result<Self> {
        // --- menu items ---
        let dashboard_item = MenuItem::new("Open dashboard", true, None);
        let dashboard_item_id = dashboard_item.id().clone();

        let pause_item = CheckMenuItem::new("Pause", true, false, None);
        let pause_item_id = pause_item.id().clone();

        let exit_item = MenuItem::new("Exit", true, None);
        let exit_item_id = exit_item.id().clone();

        let tray_menu = Menu::new();
        let _ = tray_menu.append(&dashboard_item);
        let _ = tray_menu.append(&PredefinedMenuItem::separator(None));
        let _ = tray_menu.append(&pause_item);
        let _ = tray_menu.append(&PredefinedMenuItem::separator(None));
        let _ = tray_menu.append(&exit_item);

        // --- icon data ---
        let idle_icon = load_or_fallback(assets::get_logo_default_bytes(), 128, 128, 128);
        let recording_icon = load_or_fallback(assets::get_logo_recording_bytes(), 220, 38, 38);
        let uploading_icon = make_icon(59, 130, 246); // blue

        // --- build tray icon ---
        let icon = TrayIconBuilder::new()
            .with_icon(idle_icon.clone())
            .with_tooltip("GameData Recorder — idle")
            .with_menu(Box::new(tray_menu))
            .build()
            .or_else(|primary_err| {
                tracing::warn!(
                    "Tray icon build with menu failed ({primary_err}); retrying minimal"
                );
                TrayIconBuilder::new()
                    .with_icon(idle_icon.clone())
                    .build()
                    .map_err(|fallback_err| {
                        tracing::error!(
                            "Tray icon fallback build also failed: {fallback_err}. \
                             OS session may have no desktop/message pump."
                        );
                        primary_err
                    })
            })?;

        // --- menu event handler ---
        {
            let pause_item_id = pause_item_id.clone();
            let exit_item_id = exit_item_id.clone();
            let dashboard_item_id = dashboard_item_id.clone();
            let on_pause = on_pause.clone();
            let on_exit = on_exit.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                match event.id() {
                    id if id == &dashboard_item_id => {
                        tracing::info!("Tray: Open dashboard clicked");
                        on_dashboard();
                    }
                    id if id == &pause_item_id => {
                        tracing::info!("Tray: Pause toggled");
                        on_pause();
                    }
                    id if id == &exit_item_id => {
                        tracing::info!("Tray: Exit clicked");
                        on_exit();
                    }
                    _ => {}
                }
            }));
        }

        // --- left-click toggles show/hide (no-op in headless mode) ---
        TrayIconEvent::set_event_handler(Some(|_event| {}));

        Ok(Tray {
            icon,
            pause_item_id,
            exit_item_id,
            dashboard_item_id,
            idle_icon,
            recording_icon,
            uploading_icon,
        })
    }

    /// Update the tray icon to reflect the current daemon state.
    pub fn update_state(&self, recording: bool, uploading: bool) {
        let (icon_data, tooltip) = if uploading {
            (&self.uploading_icon, "GameData Recorder — uploading")
        } else if recording {
            (&self.recording_icon, "GameData Recorder — recording")
        } else {
            (&self.idle_icon, "GameData Recorder — idle")
        };

        let _ = self.icon.set_icon(Some(icon_data.clone()));
        let _ = self.icon.set_tooltip(tooltip);
    }

    /// Set the Pause menu item checked state.
    pub fn set_paused(&self, paused: bool) {
        if let Some(menu) = self.icon.menu() {
            if let Some(item) = menu.get(&self.pause_item_id) {
                if let tray_icon::menu::MenuItemKind::Check(c) = item {
                    let _ = c.set_checked(paused);
                }
            }
        }
    }

    /// Block the current thread and pump tray events until `running` becomes false.
    /// This is the main event loop replacement for egui's run loop.
    pub fn run_until_exit(&self, running: &Arc<AtomicBool>) {
        while running.load(Ordering::Relaxed) {
            // tray-icon uses a background thread for events; we just sleep
            // and periodically update state.  On macOS this also keeps the
            // NSApp run-loop alive.
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}
