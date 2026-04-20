#![cfg_attr(
    all(target_os = "windows", not(debug_assertions),),
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
mod ui;
mod upload;
mod util;
mod validation;

use crate::util::log_rotation::RotatingFileWriter;
use color_eyre::Result;
use egui_wgpu::wgpu;
use tracing_subscriber::{Layer, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use std::sync::Arc;

use crate::system::ensure_single_instance::ensure_single_instance;

fn main() -> Result<()> {
    // Security hardening: restrict DLL search to System32 BEFORE any other Win32 call.
    // This prevents DLL-hijack / side-loading attacks where a malicious DLL dropped in
    // the app's own directory (or CWD) would otherwise be preferred over the genuine
    // system DLL. Must run first — once any Win32 API has been called, the loader's
    // per-process search list may already be fixed.
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::LibraryLoader::{
            LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
        };
        // Best-effort: if this fails (extremely unlikely on supported Windows),
        // there is nothing useful we can do before logging is up. Swallow the
        // result so the app still launches; the fallback DLL search order is
        // still safer than a crash-at-startup. The windows-rs 0.62 binding
        // wants the typed `LOAD_LIBRARY_FLAGS` constant directly, not `.0`.
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }

    // Set up logging, including to file with rotation (20MB max, 3 files)
    let log_dir = config::get_persistent_dir()?;
    let log_path = log_dir.join("gamedata-recorder-debug.log");
    let log_file =
        RotatingFileWriter::new(log_dir.clone(), "gamedata-recorder-debug.log".to_string())?;

    let mut env_filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
        .from_env()?;
    for crate_name in [
        "wgpu_hal",
        "symphonia_core",
        "symphonia_bundle_mp3",
        "egui_window_glfw_passthrough",
        "egui_overlay",
        "egui_render_glow",
    ] {
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

    tracing::info!(
        "GameData Recorder v{} ({})",
        env!("CARGO_PKG_VERSION"),
        git_version::git_version!()
    );

    // CI mode banner. Emitted as `warn` so it's hard to miss in logs — every
    // safety gate (consent, whitelist) is bypassed when this is active and
    // anyone reading the log needs to know that.
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

    // Ensure only one instance is running
    tracing::debug!("Checking for single instance");
    ensure_single_instance()?;
    tracing::debug!("Single instance check passed");

    tracing::debug!("Creating WGPU instance and enumerating adapters");
    let wgpu_instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter_infos = wgpu_instance
        .enumerate_adapters(wgpu::Backends::DX12)
        .into_iter()
        .map(|a| a.get_info())
        .collect::<Vec<_>>();
    tracing::info!("Available adapters: {adapter_infos:?}");

    tracing::debug!("Creating communication channels");
    let (async_request_tx, async_request_rx) = tokio::sync::mpsc::channel(200);
    let (ui_update_tx, ui_update_rx) = app_state::UiUpdateSender::build();
    // A broadcast channel is used as older entries will be dropped if the channel is full.
    let (ui_update_unreliable_tx, ui_update_unreliable_rx) = tokio::sync::broadcast::channel(200);
    // Upload trigger channel: unbounded so that stop-recording callers never block.
    // The upload worker task owns the receiver and drains it with dedup.
    let (upload_trigger_tx, upload_trigger_rx) =
        tokio::sync::mpsc::unbounded_channel::<upload::UploadTrigger>();
    tracing::debug!("Initializing app state");
    let app_state = Arc::new(app_state::AppState::new(
        async_request_tx,
        ui_update_tx,
        ui_update_unreliable_tx,
        adapter_infos,
        upload_trigger_tx,
    ));
    tracing::debug!("App state initialized");

    // CI mode: redirect recordings to GAMEDATA_OUTPUT_DIR (in-memory only;
    // never persisted to disk). All downstream readers go through
    // `app_state.config.preferences.recording_location`, so a single mutation
    // here propagates to recorder, upload scanner, and UI without touching any
    // read site.
    if let Some(ci_dir) = config::ci_output_dir_override() {
        if let Err(e) = std::fs::create_dir_all(&ci_dir) {
            tracing::warn!(
                error = %e,
                dir = %ci_dir.display(),
                "CI mode: failed to create GAMEDATA_OUTPUT_DIR; recordings may fail"
            );
        }
        let mut config = app_state.config.write().unwrap();
        tracing::info!(
            old = %config.preferences.recording_location.display(),
            new = %ci_dir.display(),
            "CI mode: overriding recording_location"
        );
        config.preferences.recording_location = ci_dir;
        // NB: no `config.save()` — the override is session-only.
    }

    // launch tokio (which hosts the recorder) on seperate thread
    tracing::debug!("Spawning tokio thread");
    let (stopped_tx, stopped_rx) = tokio::sync::broadcast::channel(1);
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

            // note: this is usually the ctrl+c shut down path, but its a known bug that if the app is minimized to tray,
            // killing it via ctrl+c will not kill the app immediately, the MainApp will not receive the stop signal until
            // you click on the tray icon to re-open it, triggering the main loop repaint to run. Killing it via tray icon quit
            // works as we just force the app to reopen for a split second to trigger refresh, but no clean way to implement this
            // from here, so we just have to live with it for now.
            tracing::info!("Tokio thread shut down, propagating stop signal");
            match stopped_tx.send(()) {
                Ok(_) => {}
                Err(e) => tracing::error!("Failed to send stop signal: {}", e),
            };
            app_state
                .ui_update_tx
                .send(app_state::UiUpdate::ForceUpdate)
                .ok();
            tracing::info!("Tokio thread shut down complete");
        }
    });

    tracing::debug!("Starting UI");
    ui::start(
        wgpu_instance,
        app_state,
        ui_update_rx,
        ui_update_unreliable_rx,
        stopped_tx,
        stopped_rx,
    )?;
    tracing::info!("UI thread shut down, joining tokio thread");
    if let Err(e) = tokio_thread.join() {
        tracing::error!("Tokio thread panicked: {e:?}");
    }
    tracing::info!("Tokio thread joined, shutting down");

    Ok(())
}
