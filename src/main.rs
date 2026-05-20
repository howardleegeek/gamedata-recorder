#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]
#![deny(clippy::uninlined_format_args)]

mod api;
mod app_state;
mod assets;
mod config;
mod output_types;
mod play_time;
mod record;
mod system;
mod tokio_thread;
mod tray;
mod upload;
mod util;
mod validation;

use crate::app_state::RwLockExt as _;
use crate::util::log_rotation::RotatingFileWriter;
use color_eyre::Result;
use tracing_subscriber::{Layer, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use std::sync::{Arc, atomic::AtomicBool};

use crate::system::ensure_single_instance::ensure_single_instance;

fn main() -> Result<()> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::LibraryLoader::{
            LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, SetDefaultDllDirectories,
        };
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    }

    // --- logging ---
    let log_dir = config::get_persistent_dir()?;
    let log_path = log_dir.join("gamedata-recorder-debug.log");
    let log_file =
        RotatingFileWriter::new(log_dir.clone(), "gamedata-recorder-debug.log".to_string())?;

    let mut env_filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
        .from_env()?;
    for crate_name in ["wgpu_hal", "symphonia_core", "symphonia_bundle_mp3"] {
        if let Ok(directive) = format!("{crate_name}=warn").parse() {
            env_filter = env_filter.add_directive(directive);
        }
    }

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)
                .with_filter(env_filter.clone()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(log_file)
                .with_ansi(false)
                .with_filter(env_filter),
        )
        .init();

    tracing::debug!("Logging initialized, writing to {:?}", log_path);

    if config::ci_mode() {
        let out = config::ci_output_dir_override()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<unset; using config default>".to_string());
        tracing::warn!(
            output = %out,
            "CI MODE ACTIVE — consent auto-granted, whitelist bypassed, \
             this build must NOT ship to end users"
        );
    }

    color_eyre::install()?;

    tracing::debug!("Checking for single instance");
    ensure_single_instance()?;
    tracing::debug!("Single instance check passed");

    // --- channels ---
    let (async_request_tx, async_request_rx) = tokio::sync::mpsc::channel(200);
    let (ui_update_tx, ui_update_rx) = app_state::UiUpdateSender::build();
    let (ui_update_unreliable_tx, ui_update_unreliable_rx) = tokio::sync::broadcast::channel(200);
    let (upload_trigger_tx, upload_trigger_rx) =
        tokio::sync::mpsc::unbounded_channel::<upload::UploadTrigger>();

    let app_state = Arc::new(app_state::AppState::new(
        async_request_tx.clone(),
        ui_update_tx,
        ui_update_unreliable_tx,
        Vec::new(), // no GPU adapters needed without egui
        upload_trigger_tx,
    ));

    // CI mode override
    if let Some(ci_dir) = config::ci_output_dir_override() {
        if let Err(e) = std::fs::create_dir_all(&ci_dir) {
            tracing::warn!(
                error = %e,
                dir = %ci_dir.display(),
                "CI mode: failed to create GAMEDATA_OUTPUT_DIR; recordings may fail"
            );
        }
        let mut config = app_state
            .config
            .write_safe()
            .unwrap_or_else(|e| e.into_inner());
        tracing::info!(
            old = %config.preferences.recording_location.display(),
            new = %ci_dir.display(),
            "CI mode: overriding recording_location"
        );
        config.preferences.recording_location = ci_dir;
    }

    // --- tokio daemon thread ---
    let (stopped_tx, stopped_rx) = tokio::sync::broadcast::channel(1);
    let running = Arc::new(AtomicBool::new(true));

    let daemon_running = running.clone();
    let tokio_thread = std::thread::spawn({
        let app_state = app_state.clone();
        let stopped_tx = stopped_tx.clone();
        let stopped_rx = stopped_rx.resubscribe();
        move || {
            let result = tokio_thread::run(
                app_state.clone(),
                log_path,
                async_request_rx,
                stopped_rx,
                upload_trigger_rx,
            );

            if let Err(e) = result {
                tracing::error!("Error in tokio thread: {e}");
            }

            tracing::info!("Tokio thread shut down, propagating stop signal");
            let _ = stopped_tx.send(());
            app_state
                .ui_update_tx
                .send(app_state::UiUpdate::ForceUpdate)
                .ok();
            tracing::info!("Tokio thread shut down complete");
            daemon_running.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    });

    // --- tray icon ---
    let tray_running = running.clone();
    let tray = tray::Tray::new(
        // Open dashboard → open recordings folder
        {
            let async_request_tx = async_request_tx.clone();
            move || {
                let _ = async_request_tx.try_send(app_state::AsyncRequest::OpenDataDump);
            }
        },
        // Pause → toggle recording
        {
            let app_state = app_state.clone();
            Arc::new(move || {
                let state = app_state.state.read().unwrap();
                let is_recording = state.is_recording();
                drop(state);
                if is_recording {
                    let _ = async_request_tx.try_send(app_state::AsyncRequest::UpdateRecordingState(false));
                } else {
                    let _ = async_request_tx.try_send(app_state::AsyncRequest::UpdateRecordingState(true));
                }
            })
        },
        // Exit
        Arc::new(move || {
            tracing::info!("Tray exit requested");
            let _ = stopped_tx.send(());
            tray_running.store(false, std::sync::atomic::Ordering::Relaxed);
        }),
    )?;

    // --- state-sync loop: poll app_state and update tray icon ---
    let sync_running = running.clone();
    let sync_thread = std::thread::spawn(move || {
        while sync_running.load(std::sync::atomic::Ordering::Relaxed) {
            let state = app_state.state.read().unwrap();
            let recording = state.is_recording();
            let uploading = app_state.upload_in_progress.load(std::sync::atomic::Ordering::Relaxed);
            drop(state);
            tray.update_state(recording, uploading);
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });

    // --- tray event loop (blocks until exit) ---
    tray.run_until_exit(&running);

    tracing::info!("Tray event loop exited, joining threads");
    let _ = sync_thread.join();
    if let Err(e) = tokio_thread.join() {
        tracing::error!("Tokio thread panicked: {e:?}");
    }
    tracing::info!("All threads joined, shutting down");

    Ok(())
}
