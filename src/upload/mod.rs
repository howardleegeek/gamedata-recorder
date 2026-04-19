use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};

use tokio::sync::mpsc;

use crate::{
    api::ApiClient,
    app_state::{self, AppState, AsyncRequest, UiUpdate, UiUpdateUnreliable},
    record::LocalRecording,
    upload::upload_tar::UploadTarOutput,
};

mod progress_sender;
pub use progress_sender::{FileProgress, ProgressData, ProgressSender};

mod upload_folder;

mod create_tar;

mod upload_tar;

/// A message sent to the upload worker task.
///
/// The worker owns the receiver side of an `UnboundedSender<UploadTrigger>` channel.
/// Enqueueing a trigger is a lock-free `send()` — there is no shared `Vec<T>` / `RwLock<T>`
/// that multiple producers could race on.
///
/// The worker maintains an internal `HashSet<PathBuf>` of session paths that are
/// already pending upload. Dedup happens inside the worker, so producers don't
/// need to check "is this session already queued?" before sending.
#[derive(Debug, Clone)]
pub enum UploadTrigger {
    /// Auto-upload fired after a recording completes. `session_path` is used
    /// as the dedup key so that two rapid stop-recording events for the same
    /// session don't double-enqueue.
    Auto { session_path: PathBuf },
    /// Manual upload fired by the user (e.g. clicking the Upload button).
    /// These always run a full rescan of the recordings directory; no dedup key.
    Manual,
    /// Clear the pending auto-upload dedup set (e.g. user paused uploads or
    /// toggled auto-upload off). The worker drops all queued sessions and
    /// resets the UI queue count to zero.
    Clear,
}

/// Runs the upload worker task. This task owns the receive side of the
/// upload-trigger channel and is the only writer for
/// [`AppState::auto_upload_queue_count`].
///
/// Because the worker is a single task that awaits triggers sequentially,
/// there is no possibility of a duplicate concurrent upload being spawned,
/// and the RMW on `auto_upload_queue_count` that existed in the old design
/// is gone — the worker always knows the exact size of its pending set.
///
/// This task is spawned before any recording can fire a stop event, so the
/// channel is always ready to receive triggers (see `tokio_thread::main`).
pub async fn run_worker(
    app_state: Arc<AppState>,
    api_client: Arc<ApiClient>,
    mut trigger_rx: mpsc::UnboundedReceiver<UploadTrigger>,
) {
    // Dedup set of session paths that have been enqueued for auto-upload but
    // not yet processed. Manual triggers are not deduped (they always run a
    // full rescan).
    let mut pending: HashSet<PathBuf> = HashSet::new();
    // Whether we've seen at least one Manual trigger since the last upload.
    let mut manual_requested = false;

    while let Some(trigger) = trigger_rx.recv().await {
        // Drain any immediately-available backlog to coalesce rapid-fire triggers
        // (e.g. two stop-recording events fired within a few milliseconds).
        match trigger {
            UploadTrigger::Auto { session_path } => {
                pending.insert(session_path);
            }
            UploadTrigger::Manual => {
                manual_requested = true;
            }
            UploadTrigger::Clear => {
                pending.clear();
                manual_requested = false;
            }
        }
        loop {
            match trigger_rx.try_recv() {
                Ok(UploadTrigger::Auto { session_path }) => {
                    pending.insert(session_path);
                }
                Ok(UploadTrigger::Manual) => {
                    manual_requested = true;
                }
                Ok(UploadTrigger::Clear) => {
                    pending.clear();
                    manual_requested = false;
                }
                Err(_) => break,
            }
        }

        // Publish current queue size to UI.
        let queue_size = pending.len();
        app_state
            .auto_upload_queue_count
            .store(queue_size, Ordering::SeqCst);
        app_state
            .ui_update_tx
            .send(UiUpdate::UpdateAutoUploadQueueCount(queue_size))
            .ok();

        // Offline mode: drop auto-upload work silently, but notify the user
        // about a manual request so they know why it didn't start.
        if app_state.offline.mode.load(Ordering::SeqCst) {
            if manual_requested {
                tracing::info!("Offline mode enabled, skipping upload");
                app_state
                    .ui_update_tx
                    .send(UiUpdate::UploadFailed(
                        "Offline mode is enabled. Uploads are disabled.".to_string(),
                    ))
                    .ok();
                manual_requested = false;
            }
            pending.clear();
            app_state.auto_upload_queue_count.store(0, Ordering::SeqCst);
            app_state
                .ui_update_tx
                .send(UiUpdate::UpdateAutoUploadQueueCount(0))
                .ok();
            continue;
        }

        // Take ownership of this batch's state before the actual upload runs,
        // so triggers that arrive *during* the upload are queued for the next
        // iteration rather than being lost. We clear the dedup set here because
        // `start` does its own filesystem scan and will pick up everything that
        // matters; the set's job was just to coalesce rapid-fire enqueues.
        let batch_len = pending.len();
        pending.clear();
        let was_manual = std::mem::take(&mut manual_requested);

        if batch_len == 0 && !was_manual {
            // Nothing to do (e.g. trigger was a duplicate already-processed path).
            continue;
        }

        tracing::info!(
            paths = batch_len,
            manual = was_manual,
            "Upload worker starting batch"
        );

        let recording_location = app_state
            .config
            .read()
            .map(|c| c.preferences.recording_location.clone())
            .unwrap_or_default();

        // `start` manages `upload_in_progress` itself (sets true on entry,
        // false on exit). Because the worker is a single task that awaits
        // `start` to completion before picking up the next trigger, there's
        // no concurrent writer to the flag, so no race is possible.
        start(app_state.clone(), api_client.clone(), recording_location).await;

        // After the batch, the worker loop continues and will immediately
        // pick up any triggers that arrived during the upload.
    }

    tracing::info!("Upload worker shutting down (trigger channel closed)");
}

pub async fn start(
    app_state: Arc<AppState>,
    api_client: Arc<ApiClient>,
    recording_location: PathBuf,
) {
    let reliable_tx = app_state.ui_update_tx.clone();
    let unreliable_tx = app_state.ui_update_unreliable_tx.clone();
    let pause_flag = app_state.upload_pause_flag.clone();

    // Reset pause flag at start of upload
    pause_flag.store(false, std::sync::atomic::Ordering::SeqCst);

    // Mark upload as in progress
    app_state
        .upload_in_progress
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let (api_token, unreliable_connection, delete_uploaded) = {
        let config = app_state.config.read().unwrap_or_else(|e| {
            tracing::error!("Config mutex poisoned: {e}");
            std::sync::RwLockReadGuard::from(std::sync::PoisonError::into_inner(e))
        });
        (
            config.credentials.api_key.clone(),
            config.preferences.unreliable_connection,
            config.preferences.delete_uploaded_files,
        )
    };

    app_state
        .ui_update_unreliable_tx
        .send(UiUpdateUnreliable::UpdateUploadProgress(Some(
            ProgressData::default(),
        )))
        .ok();

    let uploaded_count = match run(
        &recording_location,
        api_client,
        api_token,
        unreliable_connection,
        delete_uploaded,
        reliable_tx.clone(),
        unreliable_tx.clone(),
        app_state.async_request_tx.clone(),
        pause_flag,
    )
    .await
    {
        Ok(count) => count,
        Err(e) => {
            tracing::error!(e=?e, "Error uploading recordings");
            0
        }
    };

    // Mark upload as no longer in progress
    app_state
        .upload_in_progress
        .store(false, std::sync::atomic::Ordering::SeqCst);

    for req in [
        AsyncRequest::LoadUploadStatistics,
        AsyncRequest::load_upload_list_default(),
        AsyncRequest::LoadLocalRecordings,
    ] {
        app_state.async_request_tx.send(req).await.ok();
    }
    unreliable_tx
        .send(UiUpdateUnreliable::UpdateUploadProgress(None))
        .ok();

    // Notify that upload batch completed with the count of recordings processed
    app_state
        .async_request_tx
        .send(AsyncRequest::UploadCompleted { uploaded_count })
        .await
        .ok();
}

/// Separate function to allow for fallibility.
/// Returns the number of recordings successfully uploaded.
#[allow(clippy::too_many_arguments)]
async fn run(
    recording_location: &Path,
    api_client: Arc<ApiClient>,
    api_token: String,
    unreliable_connection: bool,
    delete_uploaded: bool,
    reliable_tx: app_state::UiUpdateSender,
    unreliable_tx: app_state::UiUpdateUnreliableSender,
    async_req_tx: mpsc::Sender<AsyncRequest>,
    pause_flag: Arc<std::sync::atomic::AtomicBool>,
) -> Result<usize, upload_folder::UploadFolderError> {
    // Scan all local recordings and filter to only Paused and Unuploaded
    let recordings_to_upload: Vec<_> = LocalRecording::scan_directory(recording_location)
        .into_iter()
        .filter(|rec| {
            matches!(
                rec,
                LocalRecording::Paused(_) | LocalRecording::Unuploaded { .. }
            )
        })
        .collect();

    let total_files_to_upload = recordings_to_upload.len() as u64;
    let mut files_uploaded = 0u64;

    let mut last_upload_time = std::time::Instant::now();
    let reload_every_n_files = 5;
    let reload_if_at_least_has_passed = std::time::Duration::from_secs(2 * 60);
    for recording in recordings_to_upload {
        // Check if upload has been paused
        if pause_flag.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        let info = recording.info().clone();
        let path = info.folder_path.clone();

        let file_progress = FileProgress {
            current_file: info.folder_name.clone(),
            files_remaining: total_files_to_upload.saturating_sub(files_uploaded),
        };

        let result = upload_folder::upload_folder(
            recording,
            api_client.clone(),
            &api_token,
            unreliable_connection,
            unreliable_tx.clone(),
            pause_flag.clone(),
            file_progress,
        )
        .await;

        let recording_to_delete = match result {
            Ok(UploadTarOutput::Success(recording)) => Some(recording),
            Ok(UploadTarOutput::ServerInvalid(_recording)) => {
                // We intentionally choose not to delete server invalid recordings, so that the user can learn why it was invalidated
                None
            }
            Ok(UploadTarOutput::Paused(_recording)) => {
                // We intentionally choose not to delete paused recordings, as they are still valid and can be resumed
                None
            }
            Err(e) => {
                tracing::error!("Error uploading folder {}: {:?}", path.display(), e);
                reliable_tx.send(UiUpdate::UploadFailed(e.to_string())).ok();

                // If this is a network error, switch to offline mode and stop uploading
                if e.is_network_error() {
                    tracing::warn!("Network error detected, switching to offline mode");
                    async_req_tx
                        .send(AsyncRequest::SetOfflineMode {
                            enabled: true,
                            offline_reason: Some(format!(
                                "Network error detected while uploading {}",
                                path.display()
                            )),
                        })
                        .await
                        .ok();
                    break;
                }

                continue;
            }
        };

        files_uploaded += 1;

        // delete the uploaded recording directory if the preference is enabled
        if delete_uploaded && let Some(uploaded_recording) = recording_to_delete {
            let path = path.display();
            match uploaded_recording.delete(&api_client, &api_token).await {
                Ok(_) => {
                    tracing::info!("Deleted uploaded directory: {path}");
                }
                Err(e) => {
                    tracing::error!("Failed to delete uploaded directory {path}: {e:?}");
                }
            }
        }

        let should_reload = if files_uploaded.is_multiple_of(reload_every_n_files) {
            tracing::info!(
                "{} files uploaded, reloading upload stats and local recordings",
                files_uploaded
            );
            true
        } else if last_upload_time.elapsed() > reload_if_at_least_has_passed {
            tracing::info!(
                "{} seconds since last upload, reloading upload stats and local recordings",
                last_upload_time.elapsed().as_secs()
            );
            true
        } else {
            false
        };

        if should_reload {
            for req in [
                AsyncRequest::LoadUploadStatistics,
                AsyncRequest::load_upload_list_default(),
                AsyncRequest::LoadLocalRecordings,
            ] {
                async_req_tx.send(req).await.ok();
            }
        }
        last_upload_time = std::time::Instant::now();
    }

    Ok(files_uploaded as usize)
}
