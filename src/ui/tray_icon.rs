use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use color_eyre::eyre::{self, Context as _};
use tray_icon::{
    MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuId, MenuItem},
};
use winit::window::Window;

use crate::{
    app_state::{UiUpdate, UiUpdateSender},
    assets,
};

pub struct TrayIconState {
    icon: TrayIcon,
    quit_item_id: MenuId,
    open_recordings_item_id: MenuId,

    default_tray_icon_data: tray_icon::Icon,
    recording_tray_icon_data: tray_icon::Icon,
}
impl TrayIconState {
    pub fn new() -> eyre::Result<Self> {
        tracing::debug!("TrayIconState::new() called");
        // tray icon right click menu for quit option
        tracing::debug!("Creating tray menu");
        let open_recordings_item = MenuItem::new("Open Recordings", true, None);
        let open_recordings_item_id = open_recordings_item.id().clone();
        let quit_item = MenuItem::new("Quit", true, None);
        let quit_item_id = quit_item.id().clone();
        let tray_menu = Menu::new();
        let _ = tray_menu.append(&open_recordings_item);
        let _ = tray_menu.append(&quit_item);

        // create tray icon
        tracing::debug!("Loading tray icon data");
        fn create_tray_icon_data_from_bytes(bytes: &[u8]) -> eyre::Result<tray_icon::Icon> {
            let (rgba, (width, height)) = assets::load_icon_data_from_bytes(bytes)
                .ok_or_else(|| eyre::eyre!("Failed to load icon data from bytes"))?;
            Ok(tray_icon::Icon::from_rgba(rgba, width, height)?)
        }
        let make_fallback_tray_icon = || -> eyre::Result<tray_icon::Icon> {
            // 1x1 red pixel RGBA as fallback when assets are missing
            Ok(tray_icon::Icon::from_rgba(vec![255, 0, 0, 255], 1, 1)?)
        };

        let default_tray_icon_data = match assets::get_logo_default_bytes() {
            Some(bytes) => create_tray_icon_data_from_bytes(bytes)
                .unwrap_or_else(|_| make_fallback_tray_icon().unwrap()),
            None => {
                tracing::warn!("Default tray icon asset not found, using fallback");
                make_fallback_tray_icon()?
            }
        };
        let recording_tray_icon_data = match assets::get_logo_recording_bytes() {
            Some(bytes) => create_tray_icon_data_from_bytes(bytes)
                .unwrap_or_else(|_| make_fallback_tray_icon().unwrap()),
            None => {
                tracing::warn!("Recording tray icon asset not found, using fallback");
                make_fallback_tray_icon()?
            }
        };

        tracing::debug!("Building tray icon");
        let tray_icon = TrayIconBuilder::new()
            .with_icon(default_tray_icon_data.clone())
            .with_tooltip("GameData Recorder \u{2014} F9 to record")
            .with_menu(Box::new(tray_menu))
            .build()?;
        tracing::debug!("Tray icon built successfully");

        tracing::debug!("TrayIconState::new() complete");
        Ok(TrayIconState {
            icon: tray_icon,
            quit_item_id,
            open_recordings_item_id,
            default_tray_icon_data,
            recording_tray_icon_data,
        })
    }

    /// Called once the egui context is available
    pub fn post_initialize(
        &self,
        context: egui::Context,
        window: Arc<Window>,
        visible: Arc<AtomicBool>,
        stopped_tx: tokio::sync::broadcast::Sender<()>,
        ui_update_tx: UiUpdateSender,
        async_request_tx: tokio::sync::mpsc::Sender<crate::app_state::AsyncRequest>,
    ) {
        tracing::debug!("TrayIconState::post_initialize() called");
        MenuEvent::set_event_handler({
            let quit_item_id = self.quit_item_id.clone();
            let open_recordings_item_id = self.open_recordings_item_id.clone();
            let window = window.clone();
            let visible = visible.clone();
            let async_request_tx = async_request_tx.clone();
            Some(move |event: MenuEvent| match event.id() {
                id if id == &open_recordings_item_id => {
                    // Send async request to open the recordings folder
                    let _ = async_request_tx.try_send(crate::app_state::AsyncRequest::OpenDataDump);
                }
                id if id == &quit_item_id => {
                    tracing::info!("Tray icon requested shutdown");
                    if let Err(e) = stopped_tx.send(()) {
                        tracing::error!("Failed to send stop signal: {e}");
                    }

                    // Make the window processable so the main loop can
                    // receive the stop signal, but do NOT steal focus from
                    // a running game. set_visible(true) alone is enough to
                    // let the event loop run; focus_window() / set_minimized(false)
                    // would yank the player out of their game.
                    if !visible.load(Ordering::Relaxed) {
                        window.set_visible(true);
                        visible.store(true, Ordering::Relaxed);
                    }

                    ui_update_tx.send(UiUpdate::ForceUpdate).ok();
                }
                _ => {}
            })
        });

        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: MouseButtonState::Down,
                ..
            } = event
            {
                if visible.load(Ordering::Relaxed) {
                    window.set_visible(false);
                    visible.store(false, Ordering::Relaxed);
                } else {
                    // set viewport visible true in case it was minimised to tray via closing the app
                    window.set_visible(true);
                    visible.store(true, Ordering::Relaxed);
                }
                context.request_repaint();
            }
        }));
    }

    pub fn set_icon_recording(&self, recording: bool) {
        self.icon
            .set_icon(Some(if recording {
                self.recording_tray_icon_data.clone()
            } else {
                self.default_tray_icon_data.clone()
            }))
            .ok();
    }
}
