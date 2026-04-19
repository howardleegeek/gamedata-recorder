use crate::{
    api::ApiClient,
    app_state::{
        AppState, AsyncRequest, ForegroundedGame, GitHubRelease, ListeningForNewHotkey,
        RecordingStatus, UiUpdate,
    },
    assets::load_cue_bytes,
    play_time::PlayTimeTransition,
    record::LocalRecording,
    system::keycode::name_to_virtual_keycode,
    // error_message_box removed — never show popups during recording
    upload,
    util::version::is_version_newer,
};
use backoff::{ExponentialBackoff, backoff::Backoff};
use std::{
    collections::HashMap,
    io::Cursor,
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::{Datelike, Local, Timelike};

use color_eyre::{Result, eyre::Context};

use constants::{
    GH_ORG, GH_REPO, MAX_FOOTAGE, MAX_IDLE_DURATION, unsupported_games::UnsupportedGames,
};
use game_process::does_process_exist;
use input_capture::{Event, InputCapture};
use rodio::{Decoder, Sink, Source};
use tokio::{sync::oneshot, time::MissedTickBehavior};
use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::GetForegroundWindow};

use crate::{
    record::{Recorder, get_recording_base_resolution},
    system::raw_input_debouncer::EventDebouncer,
};

pub fn run(
    app_state: Arc<AppState>,
    log_path: PathBuf,
    async_request_rx: tokio::sync::mpsc::Receiver<AsyncRequest>,
    stopped_rx: tokio::sync::broadcast::Receiver<()>,
    upload_trigger_rx: tokio::sync::mpsc::UnboundedReceiver<upload::UploadTrigger>,
) -> Result<()> {
    tracing::debug!("Creating tokio runtime");
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to create tokio runtime: {e}"))?;
    runtime.block_on(main(
        app_state,
        log_path,
        async_request_rx,
        stopped_rx,
        upload_trigger_rx,
    ))
}

async fn main(
    app_state: Arc<AppState>,
    log_path: PathBuf,
    mut async_request_rx: tokio::sync::mpsc::Receiver<AsyncRequest>,
    mut stopped_rx: tokio::sync::broadcast::Receiver<()>,
    upload_trigger_rx: tokio::sync::mpsc::UnboundedReceiver<upload::UploadTrigger>,
) -> Result<()> {
    tracing::debug!("Tokio async main started");
    tracing::debug!("Initializing audio stream");
    // Audio is optional — recording works without audio device.
    // Headless servers, VMs, and remote desktops often lack audio output.
    let stream_handle = match rodio::OutputStreamBuilder::open_default_stream() {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!(
                "No audio device available: {e}. Recording will work but audio cues are disabled."
            );
            None
        }
    };

    tracing::debug!("Initializing recorder");
    let recorder = Recorder::new(
        Box::new({
            let app_state = app_state.clone();
            move || {
                let base = app_state
                    .config
                    .read()
                    .unwrap()
                    .preferences
                    .recording_location
                    .clone();
                base.join(generate_session_dir_name())
            }
        }),
        app_state.clone(),
    )
    .await?;

    // Reset our encoder to x264 if the previously-set encoder is no longer available,
    // and update the available video encoders in the UI.
    {
        let encoders = recorder.available_video_encoders();

        {
            let mut config = app_state.config.write().unwrap();
            if !encoders.contains(&config.preferences.encoder.encoder) {
                tracing::warn!("Currently-set encoder is no longer available, resetting to x264");
                config.preferences.encoder.encoder = constants::encoding::VideoEncoderType::X264;
            }
        }

        app_state
            .ui_update_tx
            .send(UiUpdate::UpdateAvailableVideoEncoders(encoders.to_vec()))
            .ok();
    }

    tracing::info!("recorder initialized");
    // I initially tried to move this into `Recorder`, so that it could be passed down to
    // the relevant methods, but this caused the Windows event loop to hang.
    //
    // Absolutely no idea why, but I'm willing to accept this as-is for now.
    //
    // R46 consent gate: block here until the UI has recorded user acceptance
    // of the current-version disclosure. `InputCapture::new` registers global
    // Windows Raw Input hooks — we cannot install those hooks before consent.
    // The UI runs on a separate thread so the ConsentView can render and the
    // user can click Accept while this loop polls. `ctrlc_rx`/`stopped_rx` are
    // checked so shutting down pre-consent still exits cleanly.
    tracing::debug!("Waiting for user consent before installing input hooks");
    let consent_guard = wait_for_consent(&app_state, &mut stopped_rx).await;
    let consent_guard = match consent_guard {
        Some(g) => g,
        None => {
            tracing::info!("App shut down before consent was granted; skipping input capture init");
            return Ok(());
        }
    };

    tracing::debug!("Initializing input capture");
    let (input_capture, mut input_rx) = InputCapture::new(&consent_guard)?;
    tracing::debug!("Input capture initialized");

    let mut ctrlc_rx = wait_for_ctrl_c();

    let mut perform_checks = tokio::time::interval(Duration::from_secs(1));
    perform_checks.set_missed_tick_behavior(MissedTickBehavior::Delay);

    tracing::debug!("Initializing event debouncer");
    let mut debouncer = EventDebouncer::new();

    tracing::debug!("Initializing API client");
    let api_client = Arc::new(ApiClient::new());
    let mut valid_api_key_and_user_id: Option<(String, String)> = None;

    // Spawn the upload worker task *before* any recording can fire a stop event.
    // The worker owns the upload-trigger receiver and is the single writer
    // of `auto_upload_queue_count`, eliminating the RMW race that existed in
    // the previous design where multiple stop events could each do
    // `load() -> +1 -> store()` on the same counter.
    tracing::debug!("Spawning upload worker");
    tokio::spawn(upload::run_worker(
        app_state.clone(),
        api_client.clone(),
        upload_trigger_rx,
    ));

    let mut state = State {
        recording_state: RecordingState::Idle,
        recorder,
        input_capture,
        sink: stream_handle.as_ref().map(|h| Sink::connect_new(h.mixer())),
        app_state: app_state.clone(),
        cue_cache: HashMap::new(),
        last_active: Instant::now(),
        actively_recording_window: None,
        last_auto_record_attempt: None,
        user_stopped_game_exe: None,
    };

    // Offline backoff state
    let mut offline_backoff: Option<ExponentialBackoff> = None;
    let mut offline_backoff_handle: Option<tokio::task::JoinHandle<()>> = None;

    // Initial async requests to GitHub/server
    tracing::debug!("Spawning startup requests task");
    tokio::spawn(startup_requests(app_state.clone()));
    tracing::debug!("Tokio thread initialization complete, entering main loop");

    loop {
        tokio::select! {
            r = &mut ctrlc_rx => {
                match r {
                    Ok(_) => break,
                    Err(e) => {
                        tracing::error!("Ctrl-C signal handler error: {e}");
                        break;
                    }
                }
            },
            r = stopped_rx.recv() => {
                match r {
                    Ok(_) => {
                        // might seem redundant but sometimes there's an unreproducible bug where if the MainApp isn't
                        // performing repaints it won't receive the shut down signal until user interacts with the window
                        app_state.ui_update_tx.send(UiUpdate::ForceUpdate).ok();
                        break;
                    }
                    Err(e) => {
                        tracing::error!("Stopped signal handler error: {e}");
                        break;
                    }
                }
            },
            e = input_rx.recv() => {
                let e = match e {
                    Some(e) => e,
                    None => {
                        tracing::error!("Raw input reader closed unexpectedly");
                        break;
                    }
                };
                if !debouncer.debounce(e) {
                    continue;
                }

                // Snapshot (Acquire) is safe — we only branch on the tag.
                // The actual write, when we have a key, goes through a CAS
                // inside `capture_key()` so we can't clobber a concurrent
                // UI cancel (stop_listening) or a racing capture.
                match app_state.listening_for_new_hotkey.load() {
                    ListeningForNewHotkey::Listening { .. } => {
                        if let Some(key) = e.key_press_keycode() {
                            // If the UI cancelled the rebind between our load()
                            // and this CAS, capture_key() returns false and we
                            // simply drop the key — the user will have to try
                            // again, which is the correct behaviour.
                            let _ = app_state.listening_for_new_hotkey.capture_key(key);
                        }
                    },
                    ListeningForNewHotkey::NotListening => {
                        state.on_input(e).await;
                    },
                    // Captured — already holding a key waiting for the UI
                    // thread to consume it. Do not forward to the recorder
                    // (the user is binding, not playing), do not overwrite.
                    ListeningForNewHotkey::Captured { .. } => {},
                }
            },
            e = async_request_rx.recv() => {
                let e = match e {
                    Some(e) => e,
                    None => {
                        tracing::error!("Async request reader closed unexpectedly");
                        break;
                    }
                };
                match e {
                    AsyncRequest::ValidateApiKey { api_key } => {
                        let response = api_client.validate_api_key(&api_key).await;
                        tracing::info!("Received response from API key validation: {response:?}");

                        match response {
                            Err(e) if e.is_network_error() => {
                                // Network error or server unavailable (502/503/504) - switch to offline mode
                                tracing::warn!("API server unavailable, switching to offline mode: {e}");
                                app_state.async_request_tx.send(AsyncRequest::SetOfflineMode { enabled: true, offline_reason: Some(e.to_string()) }).await.ok();
                            }
                            Err(e) => {
                                // API key validation failed - don't switch to offline mode
                                tracing::warn!("API key validation failed: {e}");
                                app_state
                                    .ui_update_tx
                                    .send(UiUpdate::UpdateUserId(Err(e.to_string())))
                                    .ok();
                            }
                            Ok(user_id) => {
                                valid_api_key_and_user_id = Some((api_key.clone(), user_id.clone()));
                                app_state
                                    .ui_update_tx
                                    .send(UiUpdate::UpdateUserId(Ok(user_id)))
                                    .ok();

                                app_state.async_request_tx.send(AsyncRequest::LoadUploadStatistics).await.ok();
                                app_state.async_request_tx.send(AsyncRequest::load_upload_list_default()).await.ok();
                            }
                        }
                        // no matter if offline or online, local recordings should be loaded
                        app_state.async_request_tx.send(AsyncRequest::LoadLocalRecordings).await.ok();
                    }
                    AsyncRequest::UploadData => {
                        // Forward to the upload worker. The worker serializes all
                        // uploads, dedups auto-upload enqueues, and handles
                        // offline-mode rejection — so this branch is a simple
                        // channel send with no atomics or CAS dance.
                        if app_state
                            .upload_trigger_tx
                            .send(upload::UploadTrigger::Manual)
                            .is_err()
                        {
                            tracing::error!(
                                "Upload worker channel closed; cannot trigger manual upload"
                            );
                        }
                    }
                    AsyncRequest::PauseUpload => {
                        app_state.upload_pause_flag.store(true, Ordering::SeqCst);
                        // Clear the auto-upload queue when pausing. The worker
                        // owns the queue; we just send it a Clear trigger.
                        let prev_queue_count = app_state
                            .auto_upload_queue_count
                            .load(Ordering::SeqCst);
                        tracing::info!(
                            "Upload pause requested, auto-upload queue cleared (was {prev_queue_count} recordings)"
                        );
                        app_state
                            .upload_trigger_tx
                            .send(upload::UploadTrigger::Clear)
                            .ok();
                    }
                    AsyncRequest::OpenDataDump => {
                        let recording_location = app_state
                            .config
                            .read()
                            .unwrap()
                            .preferences
                            .recording_location
                            .clone();
                        if !recording_location.exists() {
                            let _ = std::fs::create_dir_all(&recording_location);
                        }
                        let absolute_path = std::fs::canonicalize(&recording_location)
                            .unwrap_or(recording_location);
                        opener::open(&absolute_path).ok();
                    }
                    AsyncRequest::OpenLog => {
                        opener::reveal(&log_path).ok();
                    }
                    AsyncRequest::OpenFolder(path) => {
                        opener::open(&path).ok();
                    }
                    AsyncRequest::UpdateUnsupportedGames(new_games) => {
                        let mut unsupported_games = app_state.unsupported_games.write().unwrap();
                        let old_game_count = unsupported_games.games.len();
                        *unsupported_games = new_games;
                        tracing::info!(
                            "Updated unsupported games: {old_game_count} -> {} total",
                            unsupported_games.games.len(),
                        );
                    }
                    AsyncRequest::LoadUploadStatistics => {
                        if app_state.offline.mode.load(Ordering::SeqCst) {
                            tracing::info!("Offline mode enabled, skipping upload statistics load");
                        } else {
                            match valid_api_key_and_user_id.clone() {
                                Some((api_key, user_id)) => {
                                    let filters = app_state.upload_filters.read().unwrap();
                                    let start_date = filters.start_date;
                                    let end_date = filters.end_date;
                                    drop(filters);
                                    tokio::spawn({
                                        let app_state = app_state.clone();
                                        let api_client = api_client.clone();
                                        async move {
                                            let stats = match api_client.get_user_upload_statistics(&api_key, &user_id, start_date, end_date).await {
                                                Ok(stats) => stats,
                                                Err(e) => {
                                                    tracing::error!(e=?e, "Failed to get user upload statistics");
                                                    return;
                                                }
                                            };
                                            tracing::info!(stats=?stats, "Loaded upload statistics");
                                            app_state.ui_update_tx.send(UiUpdate::UpdateUserUploadStatistics(stats)).ok();
                                        }
                                    });
                                }
                                None => {
                                    tracing::error!("API key and user ID not found, skipping upload statistics load");
                                }
                            }
                        }
                    }
                    AsyncRequest::LoadUploadList { limit, offset } => {
                        if app_state.offline.mode.load(Ordering::SeqCst) {
                            tracing::info!("Offline mode enabled, skipping upload list load");
                        } else {
                            match valid_api_key_and_user_id.clone() {
                                Some((api_key, user_id)) => {
                                    let filters = app_state.upload_filters.read().unwrap();
                                    let start_date = filters.start_date;
                                    let end_date = filters.end_date;
                                    drop(filters);
                                    tokio::spawn({
                                        let app_state = app_state.clone();
                                        let api_client = api_client.clone();
                                        async move {
                                            let (uploads, limit, offset) = match api_client.get_user_upload_list(&api_key, &user_id, limit, offset, start_date, end_date).await {
                                                Ok(res) => res,
                                                Err(e) => {
                                                    tracing::error!(e=?e, "Failed to get user upload list");
                                                    return;
                                                }
                                            };
                                            tracing::info!(count=uploads.len(), "Loaded upload list");
                                            app_state.ui_update_tx.send(UiUpdate::UpdateUserUploadList { uploads, limit, offset }).ok();
                                        }
                                    });
                                }
                                None => {
                                    tracing::error!("API key and user ID not found, skipping upload list load");
                                }
                            }
                        }
                    }
                    AsyncRequest::LoadLocalRecordings => {
                        let recording_location = app_state
                            .config
                            .read()
                            .unwrap()
                            .preferences
                            .recording_location
                            .clone();
                        tokio::spawn({
                            let app_state = app_state.clone();
                            async move {
                                let local_recordings = tokio::task::spawn_blocking({
                                    let recording_location = recording_location.clone();
                                    move || LocalRecording::scan_directory(&recording_location)
                                }).await.unwrap_or_default();

                                tracing::info!("Found {} local recordings", local_recordings.len());
                                if let Err(e) = app_state
                                    .ui_update_tx
                                    .send(UiUpdate::UpdateLocalRecordings(local_recordings))
                                {
                                    tracing::error!("Failed to send UpdateLocalRecordings to UI: {}", e);
                                }
                            }
                        });
                    }
                    AsyncRequest::DeleteAllInvalidRecordings => {
                        // Check if we have an API key for server cleanup
                        let api_key_opt = valid_api_key_and_user_id.as_ref().map(|(k, _)| k.clone());
                        let has_api_key = api_key_opt.is_some();
                        if !has_api_key {
                            tracing::info!("No API key available - will delete invalid recordings locally only");
                        }

                        tokio::spawn({
                            let app_state = app_state.clone();
                            let api_client = api_client.clone();
                            let recording_location = app_state
                                .config
                                .read()
                                .unwrap()
                                .preferences
                                .recording_location
                                .clone();
                            async move {
                                // Get current list of local recordings
                                let local_recordings = tokio::task::spawn_blocking({
                                    let recording_location = recording_location.clone();
                                    move || LocalRecording::scan_directory(&recording_location)
                                }).await.unwrap_or_default();

                                // Filter only invalid recordings
                                let invalid_recordings: Vec<_> = local_recordings
                                    .into_iter()
                                    .filter(|r| matches!(r, LocalRecording::Invalid { .. }))
                                    .collect();

                                if invalid_recordings.is_empty() {
                                    tracing::info!("No invalid recordings to delete");
                                    return;
                                }

                                let total_count = invalid_recordings.len();
                                tracing::info!("Deleting {} invalid recordings", total_count);

                                // Delete all invalid recording folders
                                let mut errors = vec![];
                                for recording in invalid_recordings {
                                    let info = recording.info().clone();
                                    let delete_result = if let Some(ref api_key) = api_key_opt {
                                        // Delete with server cleanup
                                        recording.delete(&api_client, api_key).await
                                    } else {
                                        // Delete locally only
                                        recording.delete_without_abort_sync()
                                    };
                                    if let Err(e) = delete_result {
                                        tracing::error!("Failed to delete {}: {:?}", info, e);
                                        errors.push(info.folder_name);
                                    } else {
                                        tracing::info!("Deleted invalid recording: {}", info);
                                    }
                                }

                                if errors.is_empty() {
                                    tracing::info!("Successfully deleted all {total_count} invalid recordings");
                                } else {
                                    tracing::warn!("Failed to delete {} recordings: {:?}", errors.len(), errors);
                                }


                                app_state.async_request_tx.send(AsyncRequest::LoadLocalRecordings).await.ok();
                            }
                        });
                    }
                    AsyncRequest::DeleteAllUploadedLocalRecordings => {
                        // Check if we have an API key for server cleanup
                        let api_key_opt = valid_api_key_and_user_id.as_ref().map(|(k, _)| k.clone());
                        let has_api_key = api_key_opt.is_some();
                        if !has_api_key {
                            tracing::info!("No API key available - will delete uploaded recordings locally only");
                        }

                        tokio::spawn({
                            let app_state = app_state.clone();
                            let api_client = api_client.clone();
                            let recording_location = app_state
                                .config
                                .read()
                                .unwrap()
                                .preferences
                                .recording_location
                                .clone();
                            async move {
                                // Get current list of local recordings
                                let mut local_recordings = tokio::task::spawn_blocking({
                                    let recording_location = recording_location.clone();
                                    move || LocalRecording::scan_directory(&recording_location)
                                }).await.unwrap_or_default();

                                local_recordings.retain(|r| matches!(r, LocalRecording::Uploaded { .. }));

                                if local_recordings.is_empty() {
                                    tracing::info!("No uploaded recordings to delete");
                                    return;
                                }

                                let total_count = local_recordings.len();
                                tracing::info!("Deleting {total_count} uploaded recordings");

                                // Delete all uploaded recording folders
                                let mut errors = vec![];
                                for recording in local_recordings {
                                    let info = recording.info().clone();
                                    let delete_result = if let Some(ref api_key) = api_key_opt {
                                        // Delete with server cleanup
                                        recording.delete(&api_client, api_key).await
                                    } else {
                                        // Delete locally only
                                        recording.delete_without_abort_sync()
                                    };
                                    if let Err(e) = delete_result {
                                        tracing::error!(e=?e, "Failed to delete uploaded recording: {info}");
                                        errors.push(info.folder_name);
                                    } else {
                                        tracing::info!("Deleted uploaded recording: {info}");
                                    }
                                }

                                if errors.is_empty() {
                                    tracing::info!("Successfully deleted all {total_count} uploaded recordings");
                                } else {
                                    tracing::warn!("Failed to delete {} recordings: {:?}", errors.len(), errors);
                                }

                                app_state.async_request_tx.send(AsyncRequest::LoadLocalRecordings).await.ok();
                            }
                        });
                    }
                    AsyncRequest::DeleteRecording(path) => {
                        let delete_result = if let Some(recording) = LocalRecording::from_path(&path) {
                            // Check if we have an API key for server cleanup
                            if let Some((api_key, _)) = valid_api_key_and_user_id.as_ref() {
                                // Delete with server cleanup (abort multipart uploads if needed)
                                match recording.delete(&api_client, api_key).await {
                                    Ok(_) => {
                                        tracing::info!("Deleted recording with server cleanup: {}", path.display());
                                        Ok(())
                                    }
                                    Err(e) => {
                                        tracing::error!(e=?e, "Failed to delete recording: {}", path.display());
                                        Err(format!("Failed to delete recording: {}", e))
                                    }
                                }
                            } else {
                                // No API key - just delete locally without server cleanup
                                tracing::info!("Deleting recording locally (no API key for server cleanup): {}", path.display());
                                match recording.delete_without_abort_sync() {
                                    Ok(_) => {
                                        tracing::info!("Deleted recording locally: {}", path.display());
                                        Ok(())
                                    }
                                    Err(e) => {
                                        tracing::error!(e=?e, "Failed to delete recording locally: {}", path.display());
                                        Err(format!("Failed to delete recording locally: {}", e))
                                    }
                                }
                            }
                        } else {
                            let error_msg = format!("Cannot delete non-recording folder: {}", path.display());
                            tracing::error!("{}", error_msg);
                            Err(error_msg)
                        };

                        // Send LoadLocalRecordings to refresh UI
                        if let Err(e) = app_state.async_request_tx.send(AsyncRequest::LoadLocalRecordings).await {
                            tracing::error!("Failed to send LoadLocalRecordings after delete: {}", e);
                        }

                        // Show error to user if deletion failed
                        if let Err(error_msg) = delete_result {
                            app_state
                                .ui_update_tx
                                .send(UiUpdate::UploadFailed(error_msg))
                                .ok();
                        }

                        // Also send ForceUpdate to ensure UI repaints
                        if let Err(e) = app_state.ui_update_tx.send(UiUpdate::ForceUpdate) {
                            tracing::error!("Failed to send ForceUpdate after delete: {}", e);
                        }
                    }
                    AsyncRequest::MoveRecordingsFolder { from, to } => {
                        tokio::spawn(move_recordings_folder(app_state.clone(), from, to));
                    }
                    AsyncRequest::PickRecordingFolder { current_location } => {
                        tokio::spawn(pick_recording_folder(app_state.clone(), current_location));
                    }
                    AsyncRequest::PlayCue { cue } => {
                        if let Some(ref sink) = state.sink {
                            play_cue(sink, &app_state, &cue, &mut state.cue_cache, |s| s);
                        }
                    }
                    AsyncRequest::UploadCompleted { uploaded_count } => {
                        // The upload worker is the single writer for
                        // `auto_upload_queue_count` and self-schedules the next
                        // batch via its trigger channel, so there is nothing to
                        // do here beyond logging. The worker picks up any
                        // triggers that arrived during the last batch on its
                        // next loop iteration.
                        tracing::info!(
                            "Upload batch completed: {} recordings uploaded",
                            uploaded_count
                        );
                    }
                    AsyncRequest::ClearAutoUploadQueue => {
                        let prev_count = app_state
                            .auto_upload_queue_count
                            .load(Ordering::SeqCst);
                        tracing::info!(
                            "Auto-upload queue cleared (was {} recordings)",
                            prev_count
                        );
                        app_state
                            .upload_trigger_tx
                            .send(upload::UploadTrigger::Clear)
                            .ok();
                    }
                    AsyncRequest::SetOfflineMode { enabled, offline_reason } => {
                        tracing::info!("Setting offline mode to {}", enabled);
                        app_state.offline.mode.store(enabled, Ordering::SeqCst);

                        match (enabled, &offline_reason) {
                            (true, Some(reason)) => {
                                tracing::info!("Offline mode enabled: {}", reason);
                                app_state.ui_update_tx.send(UiUpdate::UpdateUserId(Ok(format!("Offline ({reason})")))).ok();
                                // trigger backoff attempts since offline mode enabled with error
                                app_state.async_request_tx.send(AsyncRequest::OfflineBackoffAttempt).await.ok();
                            },
                            (true, None) => {
                                tracing::info!("Offline mode enabled by user without error");
                                app_state.ui_update_tx.send(UiUpdate::UpdateUserId(Ok("Offline".to_string()))).ok();
                            },
                            (false, _) => {
                                tracing::info!("Offline mode disabled, going online");
                                let api_key = app_state.config.read().unwrap().credentials.api_key.clone();
                                app_state.ui_update_tx.send(UiUpdate::UpdateUserId(Ok("Authenticating...".to_string()))).ok();
                                app_state.async_request_tx.send(AsyncRequest::CancelOfflineBackoff).await.ok();
                                app_state.async_request_tx.send(AsyncRequest::ValidateApiKey { api_key }).await.ok();
                                // Load data now that we're online
                                app_state.async_request_tx.send(AsyncRequest::LoadUploadStatistics).await.ok();
                                app_state.async_request_tx.send(AsyncRequest::load_upload_list_default()).await.ok();
                                app_state.async_request_tx.send(AsyncRequest::LoadLocalRecordings).await.ok();
                            },
                        }
                    }
                    AsyncRequest::CancelOfflineBackoff => {
                        tracing::info!("Cancelling offline backoff retry loop");
                        if let Some(handle) = offline_backoff_handle.take() {
                            handle.abort();
                        }
                        offline_backoff = None;
                        app_state.offline.backoff_active.store(false, Ordering::SeqCst);
                        app_state.offline.retry_count.store(0, Ordering::SeqCst);
                        app_state.offline.next_retry_time.store(0, Ordering::SeqCst);
                    }
                    AsyncRequest::OfflineBackoffAttempt => {
                        let backoff_active = app_state.offline.backoff_active.load(Ordering::SeqCst);
                        let offline_mode = app_state.offline.mode.load(Ordering::SeqCst);

                        match (backoff_active, offline_mode) {
                            // Not offline - nothing to do
                            (_, false) => {}

                            // Offline but backoff not started - initialize backoff and schedule first retry
                            (false, true) => {
                                tracing::info!("Starting offline backoff retry loop");

                                // Create new backoff with ~2.5 min initial, doubling, max 60 min
                                // but never stops since max_elapsed_time is None. At max every hour
                                // it will retry.
                                let mut backoff = ExponentialBackoff {
                                    initial_interval: Duration::from_secs(150),
                                    current_interval: Duration::from_secs(150), // Must match initial_interval
                                    max_interval: Duration::from_secs(3600),
                                    max_elapsed_time: None,
                                    multiplier: 2.0,
                                    randomization_factor: 0.1,
                                    ..Default::default()
                                };

                                // Get first interval and schedule retry
                                if let Some(delay) = backoff.next_backoff() {
                                    let next_retry_time = SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs()
                                        + delay.as_secs();

                                    app_state.offline.backoff_active.store(true, Ordering::SeqCst);
                                    app_state.offline.next_retry_time.store(next_retry_time, Ordering::SeqCst);
                                    app_state.offline.retry_count.store(0, Ordering::SeqCst);

                                    offline_backoff = Some(backoff);

                                    // Cancel any existing handle
                                    if let Some(handle) = offline_backoff_handle.take() {
                                        handle.abort();
                                    }

                                    // Schedule the retry
                                    tracing::info!("Scheduling offline retry in {:?}", delay);
                                    offline_backoff_handle = Some(tokio::spawn({
                                        let tx = app_state.async_request_tx.clone();
                                        async move {
                                            tokio::time::sleep(delay).await;
                                            tx.send(AsyncRequest::OfflineBackoffAttempt).await.ok();
                                        }
                                    }));
                                }
                            }

                            // Backoff active and still offline - attempt API validation
                            (true, true) => {
                                let retry_count = app_state.offline.retry_count.load(Ordering::SeqCst);
                                tracing::info!("Offline backoff retry #{} - attempting API validation", retry_count + 1);
                                let api_key = app_state.config.read().unwrap().credentials.api_key.clone();
                                // Attempt validation
                                let response = api_client.validate_api_key(&api_key).await;
                                match response {
                                    Ok(user_id) => {
                                        // Successful server response, cancel backoff and go online
                                        tracing::info!("Offline backoff retry succeeded, going online");
                                        app_state.offline.mode.store(false, Ordering::SeqCst);
                                        valid_api_key_and_user_id = Some((api_key.clone(), user_id.clone()));
                                        app_state.ui_update_tx.send(UiUpdate::UpdateUserId(Ok(user_id))).ok();

                                        // Cancel backoff
                                        app_state.async_request_tx.send(AsyncRequest::SetOfflineMode { enabled: false, offline_reason: None }).await.ok();
                                    }
                                    Err(e) if e.is_network_error() => {
                                        // Still offline, schedule next retry
                                        tracing::warn!("Offline backoff retry #{} failed (network error): {}", retry_count + 1, e);

                                        let new_retry_count = retry_count + 1;
                                        app_state.offline.retry_count.store(new_retry_count, Ordering::SeqCst);

                                        // Get next backoff delay (None if max_elapsed_time exceeded)
                                        let next_delay = offline_backoff.as_mut().and_then(|b| b.next_backoff());

                                        if let Some(delay) = next_delay {
                                            let next_retry_time = SystemTime::now()
                                                .duration_since(UNIX_EPOCH)
                                                .unwrap()
                                                .as_secs()
                                                + delay.as_secs();
                                            app_state.offline.next_retry_time.store(next_retry_time, Ordering::SeqCst);

                                            // Schedule next retry
                                            tracing::info!("Scheduling next offline retry in {:?}", delay);
                                            offline_backoff_handle = Some(tokio::spawn({
                                                let tx = app_state.async_request_tx.clone();
                                                async move {
                                                    tokio::time::sleep(delay).await;
                                                    tx.send(AsyncRequest::OfflineBackoffAttempt).await.ok();
                                                }
                                            }));
                                        } else {
                                            // Backoff exhausted (max_elapsed_time reached) - stop retrying
                                            // This should never happen since we set max_elapsed_time to None, but just
                                            // in case in the future we change that behaviour we don't get footgunned
                                            tracing::warn!("Offline backoff exhausted, stopping retries");
                                            offline_backoff = None;
                                            app_state.offline.backoff_active.store(false, Ordering::SeqCst);
                                            app_state.offline.retry_count.store(0, Ordering::SeqCst);
                                            app_state.offline.next_retry_time.store(0, Ordering::SeqCst);
                                        }
                                    }
                                    Err(e) => {
                                        // Non-network error (e.g., invalid API key) - stop backoff
                                        tracing::warn!("Offline backoff retry got non-network error, stopping: {}", e);

                                        // Cancel backoff but stay offline
                                        if let Some(handle) = offline_backoff_handle.take() {
                                            handle.abort();
                                        }
                                        offline_backoff = None;
                                        app_state.offline.backoff_active.store(false, Ordering::SeqCst);
                                        app_state.offline.retry_count.store(0, Ordering::SeqCst);
                                        app_state.offline.next_retry_time.store(0, Ordering::SeqCst);

                                        app_state.ui_update_tx.send(UiUpdate::UpdateUserId(Err(e.to_string()))).ok();
                                    }
                                }
                            }
                        }
                    }
                }
            },
            _ = perform_checks.tick() => {
                // Update play-time tracking
                if let (Ok(mut play_time), Ok(state)) = (app_state.play_time_state.write(), app_state.state.read()) {
                    play_time.tick(&state);
                }

                // Flush pending input events to disk
                if let Err(e) = state.recorder.flush_input_events().await {
                    tracing::error!(e=?e, "Failed to flush input events");
                }
                // Check foregrounded game
                let foregrounded = match app_state.unsupported_games.read() {
                    Ok(unsupported) => get_foregrounded_game(&unsupported, &state.recorder),
                    Err(_) => {
                        tracing::error!("unsupported_games RwLock poisoned, skipping foreground check");
                        None
                    }
                };

                // Clear user_stopped_game_exe if foreground game changed
                if let Some(ref fg) = foregrounded {
                    if let Some(ref exe_name) = fg.exe_name {
                        if state.user_stopped_game_exe.as_ref() != Some(exe_name) {
                            // Game changed, clear the suppression
                            if state.user_stopped_game_exe.is_some() {
                                tracing::debug!(
                                    old_game = ?state.user_stopped_game_exe,
                                    new_game = ?exe_name,
                                    "Clearing user_stopped_game_exe: game changed"
                                );
                                state.user_stopped_game_exe = None;
                            }
                        }
                    }
                }

                if let Some(ref fg) = foregrounded
                    && fg.is_recordable()
                    && fg.exe_name.is_some()
                {
                    *app_state.last_recordable_game.write().unwrap() = fg.exe_name.clone();
                }
                *app_state.last_foregrounded_game.write().unwrap() = foregrounded;
                // Tick state machine
                if let Some((to_state, task)) = state.tick().await {
                    if let Err(e) = state.handle_transition(to_state).await {
                        tracing::error!(e=?e, "Failed to {task}");
                    }
                }
                // Periodically force the UI to rerender so that it will process events, even if not visible
                app_state.ui_update_tx.send(UiUpdate::ForceUpdate).ok();
            },
        }
    }

    if let Err(e) = state.recorder.stop(&state.input_capture).await {
        tracing::error!(e=?e, "Failed to stop recording on shutdown");
    }
    Ok(())
}

/// State machine-esque representation of the recording state. This is only accessible from tokio_thread.
/// We want to somehow be able to manipulate the recording state with appropriate transitions, however its
/// not trivial to handle diff function signatures for on_input, tick, etc. for every state. This would indicate
/// that RecordingState should be a struct for each state, but that's disgustingly overcomplicated and would mean match
/// statements in the tokio thread itself to match the correct function signatures anyway, which defeats the purpose.
/// Updates the game configuration to use window capture mode and persists it to disk
fn enable_window_capture_for_game(app_state: &AppState, game_exe: &str) -> Result<()> {
    let exe_without_ext = std::path::Path::new(game_exe)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| game_exe.to_string())
        .to_lowercase();

    let mut config = app_state.config.write().unwrap();
    let game_config = config
        .preferences
        .games
        .entry(exe_without_ext.clone())
        .or_default();

    // v2.5.2: DO NOT auto-enable window capture on hook timeout.
    // Session data from nucbox proved window capture attaches to wrong HWND
    // (e.g. Rockstar Games Launcher instead of the GTA5.exe game window),
    // producing 1-FPS recordings of the launcher UI. Monitor capture is
    // safer: it records whatever is on the monitor regardless of which
    // HWND is foreground. Keep the current capture mode as-is.
    {
        tracing::warn!(
            game = exe_without_ext,
            "Game capture hook timed out — staying on current capture mode (no auto-switch to window capture)"
        );
        // Silence the unused variables so we can keep the surrounding block
        // compiling identically while we remove the mutation.
        let _ = &game_config;
        let _ = &config;
    }

    Ok(())
}

/// This then indicates that we should move all the variables into RecordingState, but thats not possible with enums we would
/// have to split it into a struct and the enum portion. This seems the cleanest possible, and we would have
/// on_input/tick() as non-arg accepting fns (or like maybe 1 arg for the tracing str reason, something consistent),
/// then match statements within the fn itself to handle the diff states.
#[derive(Clone, PartialEq, Debug)]
enum RecordingState {
    /// Waiting for user to start recording
    Idle,
    /// In process of recording
    Recording,
    /// Recording paused due to idle or unfocused window, and will restart
    /// upon both input & window focus detected. Stores the PID of the paused
    /// application to detect if it closes while paused.
    Paused { pid: game_process::Pid },
}
struct State {
    recording_state: RecordingState,
    recorder: Recorder,
    input_capture: InputCapture,
    sink: Option<Sink>,
    app_state: Arc<AppState>,
    cue_cache: HashMap<String, Vec<u8>>,
    last_active: Instant,
    actively_recording_window: Option<HWND>,
    /// Cooldown for auto-record: prevents rapid start/stop churn on unhookable games
    last_auto_record_attempt: Option<Instant>,
    /// Suppresses auto-record for this game after user manually stopped it.
    /// Cleared when the foreground game changes.
    user_stopped_game_exe: Option<String>,
}
impl State {
    async fn on_input(&mut self, e: Event) {
        let (start_key, stop_key) = match self.app_state.config.read() {
            Ok(cfg) => (
                name_to_virtual_keycode(cfg.preferences.start_recording_key()),
                name_to_virtual_keycode(cfg.preferences.stop_recording_key()),
            ),
            Err(_) => {
                tracing::error!("Config RwLock poisoned in on_input, using F9 defaults");
                (name_to_virtual_keycode("F9"), name_to_virtual_keycode("F9"))
            }
        };
        if let Err(e) = self.recorder.seen_input(e).await {
            tracing::error!(e=?e, "Failed to seen input");
        }
        self.last_active = Instant::now();
        if let Err(e) = match (&self.recording_state, e.key_press_keycode()) {
            (RecordingState::Idle, key) if key == start_key => {
                if self.app_state.is_out_of_date.load(Ordering::SeqCst) {
                    // Don't block recording with a modal dialog — it steals game focus
                    // and causes the user to be kicked back to desktop.
                    // Just log the warning and allow recording to continue.
                    tracing::warn!(
                        "Running an outdated version of GameData Recorder. \
                         Consider updating to the latest version."
                    );
                }
                self.handle_transition(RecordingState::Recording).await
            }
            (RecordingState::Recording | RecordingState::Paused { .. }, key) if key == stop_key => {
                // Remember which game the user manually stopped so auto-record
                // won't immediately restart it (cleared when foreground game changes)
                let fg = self
                    .app_state
                    .last_foregrounded_game
                    .read()
                    .unwrap()
                    .clone();
                if let Some(ref game) = fg {
                    self.user_stopped_game_exe = game.exe_name.clone();
                }
                self.handle_transition(RecordingState::Idle).await
            }
            (RecordingState::Paused { .. }, _) => {
                // key_press_keycode returned None, meaning some other input event that isn't keypress was detected,
                // then check that window is also focused before resuming recording
                if self
                    .actively_recording_window
                    .is_some_and(is_window_focused)
                {
                    tracing::info!("Input detected for focused window, restarting recording");
                    self.handle_transition(RecordingState::Recording).await
                } else {
                    return;
                }
            }
            _ => return,
        } {
            tracing::error!(e=?e, "Failed to handle recording state transition on input");
        }
    }

    async fn tick(&mut self) -> Option<(RecordingState, &'static str)> {
        if let RecordingState::Recording = self.recording_state {
            let Some(recording) = self.recorder.recording() else {
                tracing::error!("Expected recording to exist in Recording state, but found None");
                return None;
            };

            // Extract game name early to avoid borrow issues later
            let game_name = recording.game_exe().to_string();

            let state_request: Option<(RecordingState, &str)> = if !does_process_exist(
                recording.pid(),
            )
            .unwrap_or_default()
            {
                // game closed
                tracing::info!(
                    pid = recording.pid().0,
                    "Game process no longer exists, stopping recording"
                );
                Some((RecordingState::Idle, "stop recording on game process exit"))
            } else if self.last_active.elapsed() > MAX_IDLE_DURATION {
                // idle timeout
                tracing::info!(
                    "No input detected for {} seconds, stopping recording",
                    MAX_IDLE_DURATION.as_secs()
                );
                Some((
                    RecordingState::Paused {
                        pid: recording.pid(),
                    },
                    "stop recording on idle timeout",
                ))
            } else if recording.elapsed() > MAX_FOOTAGE {
                // restart recording once max duration met
                tracing::info!(
                    "Recording duration exceeded {} s, restarting recording",
                    MAX_FOOTAGE.as_secs()
                );
                Some((
                    RecordingState::Recording,
                    "restart recording on recording duration exceeded",
                ))
            } else if self
                .actively_recording_window
                .is_some_and(|window| !is_window_focused(window))
            {
                // Window lost focus — but don't pause if the game process is still running.
                // Games like GTA V switch between launcher (PlayGTAV.exe) and game (GTA5.exe)
                // processes during loading, which causes focus changes. Pausing here would
                // stop recording right when the actual game starts.
                // Only pause if the game process has actually exited.
                if !does_process_exist(recording.pid()).unwrap_or_default() {
                    tracing::info!(
                        "Game process {} exited, pausing recording",
                        recording.pid().0
                    );
                    Some((
                        RecordingState::Paused {
                            pid: recording.pid(),
                        },
                        "pause recording on game process exit",
                    ))
                } else {
                    tracing::debug!(
                        "Window lost focus but game process {} still running, continuing recording",
                        recording.pid().0
                    );
                    None // Keep recording — game is still alive
                }
            } else if let Ok(current_resolution) = get_recording_base_resolution(recording.hwnd())
                && current_resolution != recording.game_resolution()
            {
                // Check if the window resolution has changed and restart the recording
                tracing::info!(
                    old_resolution=?recording.game_resolution(),
                    new_resolution=?current_resolution,
                    "Window resolution changed, restarting recording"
                );
                Some((
                    RecordingState::Recording,
                    "restart recording on window resolution changed",
                ))
            } else if self.recorder.check_hook_timeout().await {
                // OBS failed to hook the application - attempt automatic fallback to window capture
                tracing::error!(
                    "OBS failed to hook application after {} seconds, attempting fallback to window capture",
                    constants::HOOK_TIMEOUT.as_secs()
                );

                // Get the current game exe
                let game_exe = match self.recorder.current_game_exe() {
                    Some(exe) => exe,
                    None => {
                        tracing::error!(
                            "No active recording when hook timeout detected for {}. \
                             Will retry with window capture on next detection cycle.",
                            game_name
                        );
                        return Some((RecordingState::Idle, "stop recording on hook timeout"));
                    }
                };

                // Check if we should attempt fallback (only if window capture is not already enabled)
                let exe_without_ext = std::path::Path::new(&game_exe)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| game_exe.clone())
                    .to_lowercase();

                let should_fallback = {
                    let config = self.app_state.config.read().unwrap();
                    let game_config = config.preferences.games.get(&exe_without_ext);
                    game_config.map_or(true, |gc| !gc.use_window_capture)
                };

                if !should_fallback {
                    tracing::warn!(
                        game = exe_without_ext,
                        "Window capture already enabled but hook still timed out. \
                         Continuing recording anyway — video may still contain valid frames."
                    );
                    // Don't stop recording — let it continue. Window capture often works
                    // even when the hook reports failure, producing usable video data.
                    return None;
                }

                tracing::info!(
                    game = game_exe,
                    "Hook timeout detected, attempting automatic fallback to window capture mode"
                );

                // Update config to enable window capture
                if let Err(e) = enable_window_capture_for_game(&self.app_state, &game_exe) {
                    tracing::error!(e=?e, "Failed to update game config for window capture");
                }

                // Log the fallback — no blocking message box, just continue recording.
                // Modal dialogs block the recording pipeline and steal game window focus,
                // causing the exact problem users reported (game unfocused, recording stalls).
                tracing::info!(
                    game = game_exe,
                    "Automatically switched to window capture mode. \
                     Game capture failed to hook (likely anti-cheat), but window capture should work. \
                     Preference saved for future recordings."
                );

                // Stop current recording and restart with window capture
                tracing::info!(
                    game = game_exe,
                    "Stopping current recording to restart with window capture mode"
                );
                if let Err(e) = self.recorder.stop(&self.input_capture).await {
                    tracing::error!(e=?e, "Failed to stop recording before fallback");
                }

                // Restart recording with window capture enabled
                tracing::info!(
                    game = game_exe,
                    "Restarting recording with window capture mode"
                );
                Some((
                    RecordingState::Recording,
                    "restart recording with window capture",
                ))
            } else {
                None
            };
            if let Some((to_state, task)) = state_request
                && let Err(e) = self.handle_transition(to_state).await
            {
                tracing::error!(e=?e, "Failed to {task}");
            }
        } else if let RecordingState::Paused { pid } = self.recording_state {
            // Check if the paused application has closed
            if !does_process_exist(pid).unwrap_or_default() {
                tracing::info!(
                    pid = pid.0,
                    "Paused game process no longer exists, transitioning to idle"
                );
                if let Err(e) = self.handle_transition(RecordingState::Idle).await {
                    tracing::error!(e=?e, "Failed to transition from paused to idle on process exit");
                }
            }
        }

        // AUTO-RECORD: If idle and a recordable game is in the foreground, start recording automatically.
        // This is the core "zero configuration" experience — user just plays games, recording happens.
        // Cooldown: wait 30 seconds after a failed attempt to avoid rapid start/stop churn
        // (e.g., unhookable games would otherwise create invalid folders every second).
        if self.recording_state == RecordingState::Idle {
            let cooldown_elapsed = self
                .last_auto_record_attempt
                .map(|t| t.elapsed() > std::time::Duration::from_secs(30))
                .unwrap_or(true);

            if cooldown_elapsed {
                let fg = self
                    .app_state
                    .last_foregrounded_game
                    .read()
                    .unwrap()
                    .clone();

                // Check if user manually stopped this game (suppress auto-record)
                let user_stopped_this_game = if let Some(ref game) = fg {
                    if let Some(ref exe_name) = game.exe_name {
                        self.user_stopped_game_exe.as_ref() == Some(exe_name)
                    } else {
                        false
                    }
                } else {
                    false
                };

                if user_stopped_this_game {
                    // User manually stopped this game, don't auto-record until they switch games
                    tracing::debug!(
                        game = ?fg.as_ref().and_then(|g| g.exe_name.clone()),
                        "Skipping auto-record: user manually stopped this game"
                    );
                } else if let Some(ref game) = fg
                    && game.is_recordable()
                    && game.exe_name.is_some()
                    && !self.app_state.is_out_of_date.load(Ordering::SeqCst)
                {
                    self.last_auto_record_attempt = Some(std::time::Instant::now());
                    tracing::info!(
                        game = ?game.exe_name,
                        "Recordable game detected in foreground, auto-starting recording"
                    );
                    if let Err(e) = self.handle_transition(RecordingState::Recording).await {
                        tracing::error!(e=?e, "Failed to auto-start recording, cooldown 30s");
                    }
                }
            }
        }

        // Remember to poll the recorder for its own internal work
        self.recorder.poll().await;
        None
    }

    async fn handle_transition(&mut self, to_state: RecordingState) -> Result<()> {
        tracing::info!(
            "Recording state changing: {:?} -> {:?}",
            self.recording_state,
            to_state
        );

        self.recording_state = match (&self.recording_state, to_state) {
            (RecordingState::Idle | RecordingState::Paused { .. }, RecordingState::Recording) => {
                // Start recording from Idle or Paused state
                // Acquire both locks atomically to avoid race condition
                let (honk, unsupported_games) = {
                    let config = self.app_state.config.read().map_err(|_| {
                        color_eyre::eyre::eyre!("Config RwLock poisoned during recording start")
                    })?;
                    let unsupported_games =
                        self.app_state.unsupported_games.read().map_err(|_| {
                            color_eyre::eyre::eyre!(
                                "Unsupported games RwLock poisoned during recording start"
                            )
                        })?;
                    (config.preferences.honk, unsupported_games.clone())
                };
                start_recording_safely(
                    &mut self.recorder,
                    &self.input_capture,
                    &unsupported_games,
                    self.sink.as_ref().map(|s| (s, honk, &*self.app_state)),
                    &mut self.cue_cache,
                )
                .await?;
                self.actively_recording_window =
                    self.recorder.recording().as_ref().map(|r| r.hwnd());
                tracing::info!(
                    "Recording started with HWND {:?}",
                    self.actively_recording_window
                );
                self.last_active = Instant::now();
                // Notify play time tracker of recording start
                self.app_state
                    .play_time_state
                    .write()
                    .unwrap()
                    .handle_transition(PlayTimeTransition {
                        is_recording: true,
                        due_to_idle: false,
                    });
                RecordingState::Recording
            }
            (RecordingState::Recording, RecordingState::Idle) => {
                // Stop recording and return to Idle
                let honk = self.app_state.config.read().unwrap().preferences.honk;
                let session_path = stop_recording_with_notification(
                    &mut self.recorder,
                    &self.input_capture,
                    self.sink.as_ref().map(|s| (s, honk, &*self.app_state)),
                    &mut self.cue_cache,
                )
                .await?;
                // Notify play time tracker of recording stop
                self.app_state
                    .play_time_state
                    .write()
                    .unwrap()
                    .handle_transition(PlayTimeTransition {
                        is_recording: false,
                        due_to_idle: false,
                    });
                // Trigger auto-upload if enabled
                self.maybe_trigger_auto_upload(session_path).await;
                RecordingState::Idle
            }
            (RecordingState::Recording, RecordingState::Paused { pid }) => {
                // Pause recording (due to idle or unfocused window)
                // Check if this was due to idle timeout before we stop
                let due_to_idle = self.last_active.elapsed() > MAX_IDLE_DURATION;
                let honk = self.app_state.config.read().unwrap().preferences.honk;
                let session_path = stop_recording_with_notification(
                    &mut self.recorder,
                    &self.input_capture,
                    self.sink.as_ref().map(|s| (s, honk, &*self.app_state)),
                    &mut self.cue_cache,
                )
                .await?;
                *self.app_state.state.write().unwrap() = RecordingStatus::Paused;
                // Notify play time tracker of pause (with idle buffer cancellation if due to idle)
                self.app_state
                    .play_time_state
                    .write()
                    .unwrap()
                    .handle_transition(PlayTimeTransition {
                        is_recording: false,
                        due_to_idle,
                    });
                // Trigger auto-upload if enabled (recording was saved)
                self.maybe_trigger_auto_upload(session_path).await;
                RecordingState::Paused { pid }
            }
            (RecordingState::Paused { .. }, RecordingState::Idle) => {
                let honk = self.app_state.config.read().unwrap().preferences.honk;
                // When user stop keys recording while paused, or when the paused app closes
                *self.app_state.state.write().unwrap() = RecordingStatus::Stopped;
                // Play a mild version of the stop recording cue to signal we're done
                let stop_recording_cue = self
                    .app_state
                    .config
                    .read()
                    .unwrap()
                    .preferences
                    .audio_cues
                    .stop_recording
                    .clone();
                if honk {
                    if let Some(ref sink) = self.sink {
                        play_cue(
                            sink,
                            &self.app_state,
                            &stop_recording_cue,
                            &mut self.cue_cache,
                            // TODO: find a better effect / sound for this. I wanted to use a reversed-start cue,
                            // but that doesn't seem to be something that can be easily done with rodio
                            |s| Box::new(s.low_pass(500).amplify(1.5)),
                        );
                    }
                }
                // Notify play time tracker (already paused, just confirming stop)
                self.app_state
                    .play_time_state
                    .write()
                    .unwrap()
                    .handle_transition(PlayTimeTransition {
                        is_recording: false,
                        due_to_idle: false,
                    });
                RecordingState::Idle
            }
            (RecordingState::Recording, RecordingState::Recording) => {
                // Restart the currently active recording
                // Here we intentionally set honk to false, we don't want audio cue to occur
                // on an intended recording restart and confuse the user
                let unsupported_games = self.app_state.unsupported_games.read().unwrap().clone();
                let _ = stop_recording_with_notification(
                    &mut self.recorder,
                    &self.input_capture,
                    self.sink.as_ref().map(|s| (s, false, &*self.app_state)),
                    &mut self.cue_cache,
                )
                .await?;
                start_recording_safely(
                    &mut self.recorder,
                    &self.input_capture,
                    &unsupported_games,
                    self.sink.as_ref().map(|s| (s, false, &*self.app_state)),
                    &mut self.cue_cache,
                )
                .await?;
                self.last_active = Instant::now();
                RecordingState::Recording
            }
            (old_state, new_state) => {
                tracing::error!("Invalid state transition: {old_state:?} -> {new_state:?}");
                return Err(color_eyre::eyre::eyre!(
                    "Invalid state transition: {old_state:?} -> {new_state:?}"
                ));
            }
        };
        Ok(())
    }

    /// Triggers auto-upload if the preference is enabled.
    /// Should be called after a recording is completed/saved. The
    /// `session_path` identifies the session that just finished and is used
    /// by the upload worker's dedup set to drop duplicate concurrent
    /// enqueues (e.g. two rapid stop-recording events).
    ///
    /// This method is branch-free on the hot path: it just does a channel
    /// send. All "is this session already queued?" logic lives in the
    /// upload worker, so no race window exists between the check and the
    /// send.
    async fn maybe_trigger_auto_upload(&self, session_path: Option<PathBuf>) {
        let Ok(config) = self.app_state.config.read() else {
            tracing::error!("Config RwLock poisoned in maybe_trigger_auto_upload, skipping");
            return;
        };
        let auto_upload_enabled = config.preferences.auto_upload_on_completion;
        let has_api_key = !config.credentials.api_key.is_empty();
        let skip_api_key = std::env::var("GAMEDATA_SKIP_API_KEY").is_ok();
        drop(config);

        if !auto_upload_enabled {
            return;
        }

        // Skip auto-upload if no API key (unless explicitly skipped via env var)
        if !has_api_key && !skip_api_key {
            tracing::debug!("Auto-upload skipped: no API key configured");
            return;
        }

        // Build the trigger. If we didn't get a session path (e.g. stop was
        // a no-op because there was no active recording), fall back to a
        // Manual trigger which causes the worker to rescan the recording
        // directory — that's always safe.
        let trigger = match session_path {
            Some(path) => {
                tracing::info!(
                    session=%path.display(),
                    "Auto-upload: enqueueing session"
                );
                upload::UploadTrigger::Auto { session_path: path }
            }
            None => {
                tracing::info!("Auto-upload: enqueueing manual rescan (no session path)");
                upload::UploadTrigger::Manual
            }
        };

        if self.app_state.upload_trigger_tx.send(trigger).is_err() {
            tracing::error!("Auto-upload worker channel closed; dropping auto-upload request");
        }
    }
}

/// Attempts to start the recording.
/// If it fails, it will emit an error and stop the current recording, in whatever state it may be in.
///
/// If `notification_state` is `Some`, it will be used to notify of the recording state change.
/// TODO: refactor the function signature to match the Result<()> pattern used in stop_recording
async fn start_recording_safely(
    recorder: &mut Recorder,
    input_capture: &InputCapture,
    unsupported_games: &UnsupportedGames,
    notification_state: Option<(&Sink, bool, &AppState)>,
    cue_cache: &mut HashMap<String, Vec<u8>>,
) -> Result<()> {
    if let Err(e) = recorder.start(input_capture, unsupported_games).await {
        tracing::error!(e=?e, "Failed to start recording");
        // No message box — never steal game focus
        recorder.stop(input_capture).await.ok();
        Err(e)
    } else {
        if let Some((sink, honk, app_state)) = notification_state {
            notify_of_recording_state_change(sink, honk, app_state, true, cue_cache);
        }
        Ok(())
    }
}

/// Stops the recording and plays the stop notification. Returns the session
/// folder path of the recording that was just saved (if any), so the caller
/// can use it as a dedup key when enqueueing the session for auto-upload.
async fn stop_recording_with_notification(
    recorder: &mut Recorder,
    input_capture: &InputCapture,
    notification_state: Option<(&Sink, bool, &AppState)>,
    cue_cache: &mut HashMap<String, Vec<u8>>,
) -> Result<Option<PathBuf>> {
    let session_path = recorder.stop(input_capture).await?;
    if let Some((sink, honk, app_state)) = notification_state {
        notify_of_recording_state_change(sink, honk, app_state, false, cue_cache);
        // refresh the uploads
        app_state
            .async_request_tx
            .send(AsyncRequest::LoadLocalRecordings)
            .await
            .ok();
    }
    Ok(session_path)
}

fn notify_of_recording_state_change(
    sink: &Sink,
    should_play_sound: bool,
    app_state: &AppState,
    is_recording: bool,
    cue_cache: &mut HashMap<String, Vec<u8>>,
) {
    app_state
        .ui_update_tx
        .send(UiUpdate::UpdateRecordingState(is_recording))
        .ok();
    if should_play_sound {
        // Get selected cue filenames
        let cue_filename = {
            let cfg = app_state.config.read().unwrap();
            if is_recording {
                cfg.preferences.audio_cues.start_recording.clone()
            } else {
                cfg.preferences.audio_cues.stop_recording.clone()
            }
        };
        play_cue(sink, app_state, &cue_filename, cue_cache, |s| s);
    }
}

fn play_cue(
    sink: &Sink,
    app_state: &AppState,
    filename: &str,
    cue_cache: &mut HashMap<String, Vec<u8>>,
    source_transformer: impl FnOnce(
        Box<dyn Source + Send + 'static>,
    ) -> Box<dyn Source + Send + 'static>,
) {
    // Apply configured honk volume (0-255 -> 0.0-1.0)
    let volume =
        (app_state.config.read().unwrap().preferences.honk_volume as f32 / 255.0).clamp(0.0, 1.0);

    sink.set_volume(volume);

    // Load the selected cue file with a per-thread cache
    let cue_bytes = match cue_cache
        .entry(filename.to_string())
        .or_insert_with(|| load_cue_bytes(filename).unwrap_or_default())
        .clone()
    {
        bytes if !bytes.is_empty() => bytes,
        _ => {
            tracing::warn!("Cue file not available: {filename}");
            return;
        }
    };
    let source = match Decoder::new_mp3(Cursor::new(cue_bytes)) {
        Ok(source) => source,
        Err(e) => {
            tracing::error!(e=?e, "Failed to decode recording notification sound");
            return;
        }
    };
    let source = source_transformer(Box::new(source));

    // Stop any currently playing audio and clear the queue, then play new audio cue immediately
    sink.stop();
    sink.append(source);
    sink.play();
}

fn wait_for_ctrl_c() -> oneshot::Receiver<()> {
    let (ctrl_c_tx, ctrl_c_rx) = oneshot::channel();

    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(_) => {
                let _ = ctrl_c_tx.send(());
            }
            Err(e) => {
                tracing::error!("Failed to listen for Ctrl+C signal: {e}");
            }
        }
    });
    ctrl_c_rx
}

fn is_window_focused(hwnd: HWND) -> bool {
    unsafe { GetForegroundWindow() == hwnd }
}

fn get_foregrounded_game(
    unsupported_games: &UnsupportedGames,
    recorder: &Recorder,
) -> Option<ForegroundedGame> {
    let (exe_name, _, hwnd) = crate::record::get_foregrounded_game().ok().flatten()?;

    let exe_without_ext = std::path::Path::new(&exe_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| exe_name.clone())
        .to_lowercase();

    let unsupported_reason = if let Some(unsupported) = unsupported_games.get(&exe_without_ext) {
        Some(unsupported.reason.to_string())
    } else if !recorder.is_window_capturable(hwnd) {
        Some(
            "Recorder cannot capture this window. Try running GameData Recorder in admin mode."
                .to_string(),
        )
    } else {
        None
    };

    Some(ForegroundedGame {
        exe_name: Some(exe_name),
        unsupported_reason,
    })
}

async fn pick_recording_folder(app_state: Arc<AppState>, current_location: PathBuf) {
    let mut dialog = rfd::AsyncFileDialog::new();
    if current_location.exists() {
        dialog = dialog.set_directory(&current_location);
    };

    if let Some(picked) = dialog.pick_folder().await {
        // Send the result back to the UI
        app_state
            .ui_update_tx
            .send(UiUpdate::FolderPickerResult {
                old_path: current_location,
                new_path: picked.path().into(),
            })
            .ok();
    }
}

async fn move_recordings_folder(app_state: Arc<AppState>, from: PathBuf, to: PathBuf) {
    // Check if the directories are the same
    if from == to {
        tracing::info!("Source and destination are the same, skipping move operation");
        return;
    }

    tracing::info!(
        "Moving recordings from {} to {}",
        from.display(),
        to.display()
    );

    // Ensure the destination directory exists
    if let Err(e) = tokio::fs::create_dir_all(&to).await {
        tracing::error!(
            "Failed to create destination directory {}: {:?}",
            to.display(),
            e
        );
        tracing::error!(
            "Move operation failed: Failed to create destination directory: {}",
            e
        );
        return;
    }

    // Read all entries in the source directory
    let mut entries = match tokio::fs::read_dir(&from).await {
        Ok(entries) => entries,
        Err(e) => {
            tracing::error!(
                "Failed to read source directory {}: {:?}",
                from.display(),
                e
            );
            tracing::error!(
                "Move operation failed: Failed to read source directory: {}",
                e
            );
            return;
        }
    };

    let mut moved_count = 0;
    let mut errors = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let source_path = entry.path();
        let file_name = match source_path.file_name() {
            Some(name) => name,
            None => continue,
        };

        let dest_path = to.join(file_name);

        // Move the file or directory. rename() fails across filesystem boundaries
        // (e.g. C: -> D:), so fall back to copy+delete if it fails with CrossesDevices
        // or any other error.
        match tokio::fs::rename(&source_path, &dest_path).await {
            Ok(()) => {
                moved_count += 1;
            }
            Err(_) if source_path.is_file() => {
                // Fallback: copy + delete for cross-device moves
                match tokio::fs::copy(&source_path, &dest_path).await {
                    Ok(_) => {
                        tokio::fs::remove_file(&source_path).await.ok();
                        moved_count += 1;
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to copy {} to {}: {:?}",
                            source_path.display(),
                            dest_path.display(),
                            e
                        );
                        errors.push(file_name.to_string_lossy().to_string());
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    "Failed to move {} to {}: {:?}",
                    source_path.display(),
                    dest_path.display(),
                    e
                );
                errors.push(file_name.to_string_lossy().to_string());
            }
        }
    }

    if errors.is_empty() {
        tracing::info!("Successfully moved {} recordings", moved_count);
        tracing::info!("Move operation completed: {} items moved", moved_count);
    } else {
        tracing::warn!(
            "Moved {} recordings, but failed to move {} items: {:?}",
            moved_count,
            errors.len(),
            errors
        );
        tracing::error!(
            "Move operation completed with errors: Failed to move {} items",
            errors.len()
        );
    }

    // Refresh the local recordings list
    let recording_location = app_state
        .config
        .read()
        .unwrap()
        .preferences
        .recording_location
        .clone();

    let local_recordings =
        tokio::task::spawn_blocking(move || LocalRecording::scan_directory(&recording_location))
            .await
            .unwrap_or_default();

    app_state
        .ui_update_tx
        .send(UiUpdate::UpdateLocalRecordings(local_recordings))
        .ok();
}

async fn startup_requests(app_state: Arc<AppState>) {
    if cfg!(debug_assertions) {
        tracing::info!("Skipping fetch of unsupported games in dev/debug build");
    } else {
        tokio::spawn({
            let async_request_tx = app_state.async_request_tx.clone();
            async move {
                match get_unsupported_games().await {
                    Ok(games) => {
                        async_request_tx
                            .send(AsyncRequest::UpdateUnsupportedGames(games))
                            .await
                            .ok();
                    }
                    Err(e) => {
                        tracing::error!(e=?e, "Failed to get unsupported games from GitHub");
                    }
                }
            }
        });
    }

    tokio::spawn(async move {
        if let Err(e) = check_for_updates(app_state).await {
            tracing::error!(e=?e, "Failed to check for updates");
        }
    });
}

async fn get_unsupported_games() -> Result<UnsupportedGames> {
    let text = reqwest::get(format!("https://raw.githubusercontent.com/{GH_ORG}/{GH_REPO}/refs/heads/main/crates/constants/src/unsupported_games.json"))
        .await
        .context("Failed to request unsupported games from GitHub")?
        .text()
        .await
        .context("Failed to get text of unsupported games from GitHub")?;

    UnsupportedGames::load_from_str(&text).context("Failed to parse unsupported games from GitHub")
}

async fn check_for_updates(app_state: Arc<AppState>) -> Result<()> {
    #[derive(serde::Deserialize, Debug, Clone)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }

    #[derive(serde::Deserialize, Debug, Clone)]
    struct Release {
        html_url: String,
        published_at: Option<chrono::DateTime<chrono::Utc>>,
        tag_name: String,
        name: String,
        draft: bool,
        prerelease: bool,
        assets: Vec<Asset>,
    }

    let current_version = env!("CARGO_PKG_VERSION");

    let releases = reqwest::Client::builder()
        .build()?
        .get(format!(
            "https://api.github.com/repos/{GH_ORG}/{GH_REPO}/releases"
        ))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(
            "User-Agent",
            format!("GameData Recorder v{current_version}"),
        )
        .send()
        .await
        .context("Failed to get releases from GitHub")?
        .json::<Vec<Release>>()
        .await
        .context("Failed to parse releases from GitHub")?;

    let latest_valid_release = releases.iter().find(|r| {
        !r.draft
        // filter out prereleases that we don't want users to automatically install
        && !r.prerelease
    });
    tracing::info!(latest_valid_release=?latest_valid_release, "Fetched latest valid release");

    if let Some(latest_valid_release) = latest_valid_release.cloned()
        && is_version_newer(current_version, &latest_valid_release.tag_name)
    {
        // Find the Windows installer asset (.exe file)
        let download_url = latest_valid_release
            .assets
            .iter()
            .find(|asset| asset.name.ends_with(".exe"))
            .map(|asset| asset.browser_download_url.clone())
            .unwrap_or(latest_valid_release.html_url.clone());

        app_state
            .ui_update_tx
            .send(UiUpdate::UpdateNewerReleaseAvailable(GitHubRelease {
                name: latest_valid_release.name,
                release_notes_url: latest_valid_release.html_url,
                download_url,
                release_date: latest_valid_release.published_at,
            }))
            .ok();

        app_state.is_out_of_date.store(true, Ordering::SeqCst);
    }

    Ok(())
}

/// Generates a unique session directory name.
///
/// Format: `session_YYYYMMDD_HHMMSS_<8hex>` where `<8hex>` is the first 8
/// characters of a UUIDv4. The random suffix prevents collisions when two
/// recordings start within the same 1-second window (e.g. during restart
/// loops), which otherwise caused silent overwrites.
///
/// The suffix is optional in the parse-back path — older session folders
/// (pre-2.5.6) without a suffix remain readable.
fn generate_session_dir_name() -> String {
    let now = Local::now();
    // Take 8 hex chars of a UUIDv4 (~32 bits of entropy). For two concurrent
    // recordings within the same second that's a collision probability of
    // ~1 in 4 billion per pair, vs. ~100% at 1s resolution.
    let uuid = uuid::Uuid::new_v4();
    let suffix: String = uuid.simple().to_string().chars().take(8).collect();
    format!(
        "session_{:04}{:02}{:02}_{:02}{:02}{:02}_{}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        suffix,
    )
}

/// R46 consent gate: poll `app_state.config` until consent is granted for the
/// currently-running binary version, or the app is shutting down. Returns
/// `Some(ConsentGuard)` when the user has accepted, or `None` if we observed
/// a shutdown signal before consent was recorded.
///
/// Poll-based because the ConsentView writes `has_consented` +
/// `consent_given_at_version` through the normal `copy_out_local_config`
/// pathway — there is no direct channel for "consent was granted". A 100ms
/// poll is fine: it's a one-shot startup wait, not a hot path.
async fn wait_for_consent(
    app_state: &Arc<AppState>,
    stopped_rx: &mut tokio::sync::broadcast::Receiver<()>,
) -> Option<input_capture::ConsentGuard> {
    loop {
        let guard = {
            let config = app_state.config.read().unwrap();
            crate::config::consent_guard_from_config(&config)
        };
        if guard.is_granted() {
            return Some(guard);
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                // re-check
            }
            _ = stopped_rx.recv() => {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod session_dir_name_tests {
    use super::generate_session_dir_name;
    use std::collections::HashSet;

    /// Rapid-fire generation must never produce duplicate folder names,
    /// even when the wall-clock second hasn't advanced between calls.
    #[test]
    fn rapid_generation_produces_distinct_names() {
        const N: usize = 1000;
        let mut seen = HashSet::with_capacity(N);
        for _ in 0..N {
            let name = generate_session_dir_name();
            assert!(
                seen.insert(name.clone()),
                "duplicate session dir name in tight loop: {name}"
            );
        }
    }

    /// Sanity-check the format: session_YYYYMMDD_HHMMSS_<suffix>.
    #[test]
    fn format_has_timestamp_and_suffix() {
        let name = generate_session_dir_name();
        let parts: Vec<&str> = name.split('_').collect();
        assert_eq!(
            parts.len(),
            4,
            "expected 4 underscore-separated parts, got {parts:?}"
        );
        assert_eq!(parts[0], "session");
        assert_eq!(parts[1].len(), 8, "date part YYYYMMDD");
        assert_eq!(parts[2].len(), 6, "time part HHMMSS");
        assert_eq!(parts[3].len(), 8, "8-hex suffix");
        assert!(
            parts[1].chars().all(|c| c.is_ascii_digit()),
            "date must be digits"
        );
        assert!(
            parts[2].chars().all(|c| c.is_ascii_digit()),
            "time must be digits"
        );
        assert!(
            parts[3].chars().all(|c| c.is_ascii_hexdigit()),
            "suffix must be hex"
        );
    }
}
