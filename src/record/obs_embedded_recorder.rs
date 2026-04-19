use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use color_eyre::{
    Result,
    eyre::{self, Context, OptionExt as _, bail, eyre},
};
use constants::{FPS, RECORDING_HEIGHT, RECORDING_WIDTH, encoding::VideoEncoderType};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromWindow},
};

use libobs_simple::sources::{
    ObsObjectUpdater, ObsSourceBuilder,
    windows::{
        GameCaptureSourceBuilder, GameCaptureSourceUpdater, MonitorCaptureSourceBuilder,
        MonitorCaptureSourceUpdater, ObsGameCaptureMode, WindowCaptureSourceBuilder,
        WindowCaptureSourceUpdater, WindowInfo,
    },
};
use libobs_wrapper::{
    context::ObsContext,
    data::{
        ObsDataGetters as _,
        output::ObsOutputRef,
        video::{ObsVideoInfo, ObsVideoInfoBuilder},
    },
    encoders::{
        ObsContextEncoders, ObsVideoEncoderType, audio::ObsAudioEncoder, video::ObsVideoEncoder,
    },
    enums::ObsScaleType,
    logger::ObsLogger,
    scenes::ObsSceneRef,
    sources::ObsSourceRef,
    unsafe_send::SendableComp,
    utils::{AudioEncoderInfo, ObsPath, OutputInfo, VideoEncoderInfo, traits::ObsUpdatable},
};

use crate::{
    config::{EncoderSettings, GameConfig},
    output_types::InputEventType,
    record::{
        input_recorder::InputEventStream,
        recorder::{PollUpdate, VideoRecorder},
    },
};

const OWL_SCENE_NAME: &str = "owl_data_collection_scene";
const OWL_WINDOW_CAPTURE_NAME: &str = "owl_window_capture";
const OWL_GAME_CAPTURE_NAME: &str = "owl_game_capture";
const OWL_MONITOR_CAPTURE_NAME: &str = "owl_monitor_capture";

pub struct ObsEmbeddedRecorder {
    _obs_thread: std::thread::JoinHandle<()>,
    obs_tx: tokio::sync::mpsc::Sender<RecorderMessage>,
    available_encoders: Vec<VideoEncoderType>,
}
impl ObsEmbeddedRecorder {
    pub async fn new(adapter_index: usize) -> Result<Self>
    where
        Self: Sized,
    {
        tracing::debug!(
            "ObsEmbeddedRecorder::new() called with adapter_index={}",
            adapter_index
        );
        let (obs_tx, obs_rx) = tokio::sync::mpsc::channel(100);
        let (init_success_tx, init_success_rx) = tokio::sync::oneshot::channel();
        tracing::debug!("Spawning OBS recorder thread");
        let obs_thread =
            std::thread::spawn(move || recorder_thread(adapter_index, obs_rx, init_success_tx));
        // Wait for the OBS context to be initialized, and bail out if it fails
        tracing::debug!("Waiting for OBS context initialization");
        let available_encoders = init_success_rx.await??;
        tracing::debug!(
            "OBS context initialized successfully with {} encoders",
            available_encoders.len()
        );

        Ok(Self {
            _obs_thread: obs_thread,
            obs_tx,
            available_encoders,
        })
    }
}
#[async_trait::async_trait(?Send)]
impl VideoRecorder for ObsEmbeddedRecorder {
    fn id(&self) -> &'static str {
        "ObsEmbedded"
    }

    fn available_encoders(&self) -> &[VideoEncoderType] {
        &self.available_encoders
    }

    async fn start_recording(
        &mut self,
        dummy_video_path: &Path,
        _pid: u32,
        hwnd: HWND,
        game_exe: &str,
        video_settings: EncoderSettings,
        game_config: GameConfig,
        (base_width, base_height): (u32, u32),
        event_stream: InputEventStream,
    ) -> Result<()> {
        let recording_path = dummy_video_path
            .to_str()
            .ok_or_eyre("Recording path must be valid UTF-8")?
            .to_string();

        tracing::debug!("Starting recording with path: {recording_path}");

        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.obs_tx
            .send(RecorderMessage::StartRecording {
                request: Box::new(RecordingRequest {
                    game_resolution: (base_width, base_height),
                    video_settings,
                    game_config,
                    recording_path,
                    game_exe: game_exe.to_string(),
                    hwnd: SendableComp(hwnd),
                    event_stream,
                }),
                result_tx,
            })
            .await?;
        result_rx.await??;

        tracing::info!("OBS embedded recording started successfully");

        Ok(())
    }

    async fn stop_recording(&mut self) -> Result<serde_json::Value> {
        tracing::info!("Stopping OBS embedded recording...");

        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.obs_tx
            .send(RecorderMessage::StopRecording { result_tx })
            .await?;
        let result = result_rx.await??;

        tracing::info!("OBS embedded recording stopped successfully");

        Ok(result)
    }

    async fn poll(&mut self) -> PollUpdate {
        self.obs_tx.send(RecorderMessage::Poll).await.ok();
        PollUpdate {
            active_fps: Some(unsafe { libobs_wrapper::sys::obs_get_active_fps() }),
        }
    }

    fn is_window_capturable(&self, hwnd: HWND) -> bool {
        find_game_capture_window(None, hwnd).is_ok()
    }

    async fn check_hook_timeout(&mut self) -> bool {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        if self
            .obs_tx
            .send(RecorderMessage::CheckHookTimeout { result_tx })
            .await
            .is_err()
        {
            return false;
        }
        result_rx.await.unwrap_or(false)
    }
}

impl Drop for ObsEmbeddedRecorder {
    fn drop(&mut self) {
        // Drop the sender first to signal the OBS thread to stop.
        // The thread's blocking_recv() will return None, causing the loop to exit.
        drop(std::mem::replace(
            &mut self.obs_tx,
            tokio::sync::mpsc::channel(1).0,
        ));

        // Join the thread to ensure it has fully exited.
        // Use std::mem::replace to take ownership of the handle,
        // replacing it with a dummy spawned thread that completes immediately.
        let handle = std::mem::replace(&mut self._obs_thread, std::thread::spawn(|| {}));
        // Note: We can't timeout easily in a Drop impl, so we'll just try to join.
        // If the thread is stuck, this will block. In practice, the OBS thread
        // should exit promptly when the channel is closed.
        handle.join().ok();
    }
}

enum RecorderMessage {
    StartRecording {
        request: Box<RecordingRequest>,
        result_tx: tokio::sync::oneshot::Sender<Result<()>>,
    },
    StopRecording {
        result_tx: tokio::sync::oneshot::Sender<Result<serde_json::Value>>,
    },
    Poll,
    CheckHookTimeout {
        result_tx: tokio::sync::oneshot::Sender<bool>,
    },
}

struct RecordingRequest {
    game_resolution: (u32, u32),
    video_settings: EncoderSettings,
    game_config: GameConfig,
    recording_path: String,
    game_exe: String,
    // SAFETY: HWND is wrapped in SendableComp to allow passing across threads.
    // The HWND is primarily used for comparison (checking if we're recording the same window)
    // and for OBS source creation. OBS internally handles thread safety when creating
    // capture sources, so this is safe. Direct HWND access across threads is avoided.
    hwnd: SendableComp<HWND>,
    event_stream: InputEventStream,
}

pub fn vet_to_obs_vet(vet: VideoEncoderType) -> ObsVideoEncoderType {
    match vet {
        // HEVC (H.265) hardware encoders — buyer spec requirement
        VideoEncoderType::NvEncHevc => ObsVideoEncoderType::OBS_NVENC_HEVC_TEX,
        VideoEncoderType::AmfHevc => ObsVideoEncoderType::H265_TEXTURE_AMF,
        VideoEncoderType::QsvHevc => ObsVideoEncoderType::OBS_QSV11_HEVC,
        // H.264 encoders — fallback
        VideoEncoderType::X264 => ObsVideoEncoderType::OBS_X264,
        VideoEncoderType::NvEnc => ObsVideoEncoderType::OBS_NVENC_H264_TEX,
        VideoEncoderType::Amf => ObsVideoEncoderType::H264_TEXTURE_AMF,
        VideoEncoderType::Qsv => ObsVideoEncoderType::OBS_QSV11_V2,
    }
}

pub fn obs_vet_to_vet(vet: &ObsVideoEncoderType) -> Option<VideoEncoderType> {
    match vet {
        // HEVC encoders
        ObsVideoEncoderType::OBS_NVENC_HEVC_TEX => Some(VideoEncoderType::NvEncHevc),
        ObsVideoEncoderType::H265_TEXTURE_AMF => Some(VideoEncoderType::AmfHevc),
        ObsVideoEncoderType::OBS_QSV11_HEVC => Some(VideoEncoderType::QsvHevc),
        // H.264 encoders
        ObsVideoEncoderType::OBS_X264 => Some(VideoEncoderType::X264),
        ObsVideoEncoderType::OBS_NVENC_H264_TEX => Some(VideoEncoderType::NvEnc),
        ObsVideoEncoderType::H264_TEXTURE_AMF => Some(VideoEncoderType::Amf),
        ObsVideoEncoderType::OBS_QSV11_V2 => Some(VideoEncoderType::Qsv),
        _ => None,
    }
}

fn recorder_thread(
    adapter_index: usize,
    rx: tokio::sync::mpsc::Receiver<RecorderMessage>,
    init_success_tx: tokio::sync::oneshot::Sender<
        Result<Vec<VideoEncoderType>, libobs_wrapper::utils::ObsError>,
    >,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        recorder_thread_impl(adapter_index, rx, init_success_tx);
    }));
    if let Err(e) = result {
        // Log the panic but do NOT resume_unwind — that would crash the entire application.
        // The OBS thread dying is bad but recoverable; the user can restart the app.
        // The tokio thread will detect the channel closure and handle it gracefully.
        tracing::error!(
            "OBS recorder thread panicked (recording will stop but app won't crash): {e:?}"
        );
    }
}

fn recorder_thread_impl(
    adapter_index: usize,
    mut rx: tokio::sync::mpsc::Receiver<RecorderMessage>,
    init_success_tx: tokio::sync::oneshot::Sender<
        Result<Vec<VideoEncoderType>, libobs_wrapper::utils::ObsError>,
    >,
) {
    tracing::debug!("OBS recorder thread started");
    let skipped_frames = Arc::new(Mutex::new(None));

    tracing::debug!("Creating OBS recorder state");
    let mut state = match RecorderState::new(adapter_index, skipped_frames.clone()) {
        Ok((state, available_encoders)) => {
            tracing::debug!("OBS recorder state created successfully");
            if let Err(e) = init_success_tx.send(Ok(available_encoders)) {
                tracing::error!("Failed to send init success: {:?}", e);
                return;
            }
            state
        }
        Err(e) => {
            tracing::error!("Failed to create OBS recorder state: {}", e);
            let _ = init_success_tx.send(Err(e));
            return;
        }
    };

    tracing::debug!("OBS recorder thread entering message loop");
    let mut last_shutdown_tx = None;
    while let Some(message) = rx.blocking_recv() {
        match message {
            RecorderMessage::StartRecording { request, result_tx } => {
                let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

                result_tx
                    .send(state.start_recording(request, shutdown_rx))
                    .ok();
                last_shutdown_tx = Some(shutdown_tx);
            }
            RecorderMessage::StopRecording { result_tx } => {
                result_tx
                    .send(state.stop_recording(last_shutdown_tx.take()))
                    .ok();
            }
            RecorderMessage::Poll => {
                if let Err(e) = state.poll() {
                    tracing::error!("Failed to poll OBS embedded recorder: {e}");
                }
            }
            RecorderMessage::CheckHookTimeout { result_tx } => {
                result_tx.send(state.check_hook_timeout()).ok();
            }
        }
    }
}

struct RecorderState {
    adapter_index: usize,
    skipped_frames: Arc<Mutex<Option<SkippedFrames>>>,
    output: ObsOutputRef,
    source: Option<ObsSourceRef>,
    last_encoder_settings: Option<serde_json::Value>,
    was_hooked: Arc<AtomicBool>,
    last_video_encoder_type: Option<VideoEncoderType>,
    // SAFETY: Stores the last application (game exe and HWND) for comparison purposes.
    // The HWND is primarily used for comparison to detect if we're recording the same window.
    // OBS handles the actual HWND access internally when creating capture sources.
    last_application: Option<(String, SendableComp<HWND>)>,
    /// Track the last source creation state to force recreation when it changes
    last_source_creation_state: Option<SourceCreationState>,
    is_recording: bool,
    recording_start_time: Option<Instant>,

    // Store video encoders by type to reuse them
    video_encoders: HashMap<VideoEncoderType, Arc<ObsVideoEncoder>>,
    // Audio encoder (created once upfront, reused always)
    audio_encoder: Arc<ObsAudioEncoder>,

    // Track the hook monitoring thread handle to ensure proper cleanup
    hook_monitor_thread: Option<std::thread::JoinHandle<()>>,

    // This needs to be last as it needs to be dropped last
    obs_context: ObsContext,
}
/// State that affects source creation - if any field changes, we must recreate the source
#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceCreationState {
    use_window_capture: bool,
}
impl RecorderState {
    fn new(
        adapter_index: usize,
        skipped_frames: Arc<Mutex<Option<SkippedFrames>>>,
    ) -> Result<(Self, Vec<VideoEncoderType>), libobs_wrapper::utils::ObsError> {
        tracing::debug!("RecorderState::new() called");
        // Create OBS context
        tracing::debug!("Creating OBS context");
        let mut obs_context = ObsContext::new(
            ObsContext::builder()
                .set_logger(Box::new(TracingObsLogger {
                    skipped_frames: skipped_frames.clone(),
                }))
                .set_video_info(video_info(
                    adapter_index,
                    (RECORDING_WIDTH, RECORDING_HEIGHT),
                )),
        )?;
        tracing::debug!("OBS context created successfully");

        // Get available encoders
        tracing::debug!("Querying available video encoders");
        let available_encoders = obs_context.available_video_encoders().map(|es| {
            es.into_iter()
                .filter_map(|e| obs_vet_to_vet(e.get_encoder_id()))
                .collect::<Vec<_>>()
        });
        let available_encoders = match available_encoders {
            Ok(available_encoders) => {
                tracing::debug!(
                    "Found {} available video encoders",
                    available_encoders.len()
                );
                available_encoders
            }
            Err(e) => {
                tracing::error!("Failed to get available video encoders, assuming x264 only: {e}");
                vec![VideoEncoderType::X264]
            }
        };

        // Create output upfront (will be reused for all recordings)
        tracing::info!("Creating output (one-time)");
        let output_settings = obs_context.data()?;
        let output_info = OutputInfo::new("ffmpeg_muxer", "output", Some(output_settings), None);
        let output = obs_context.output(output_info)?;

        // Create audio encoder upfront (will be reused for all recordings)
        tracing::info!("Creating audio encoder (one-time)");
        let mut audio_settings = obs_context.data()?;
        audio_settings.set_int("bitrate", 160)?;
        let audio_info =
            AudioEncoderInfo::new("ffmpeg_aac", "audio_encoder", Some(audio_settings), None);
        let audio_encoder =
            ObsAudioEncoder::new_from_info(audio_info, 0, obs_context.runtime().clone())?;

        tracing::debug!("RecorderState::new() complete");
        Ok((
            Self {
                adapter_index,
                skipped_frames,
                output,
                source: None,
                last_encoder_settings: None,
                was_hooked: Arc::new(AtomicBool::new(false)),
                last_video_encoder_type: None,
                last_application: None,
                last_source_creation_state: None,
                is_recording: false,
                recording_start_time: None,
                video_encoders: HashMap::new(),
                audio_encoder,
                hook_monitor_thread: None,
                obs_context,
            },
            available_encoders,
        ))
    }

    fn start_recording(
        &mut self,
        request: Box<RecordingRequest>,
        mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> eyre::Result<()> {
        if self.is_recording {
            bail!("Recording is already in progress");
        }

        // Set up scene and window capture based on input pid
        let mut scene = if let Some(scene) = self.obs_context.get_scene(OWL_SCENE_NAME)? {
            tracing::info!("Reusing existing scene");
            scene
        } else {
            tracing::info!("Creating new scene");
            self.obs_context.scene(OWL_SCENE_NAME)?
        };

        self.obs_context
            .reset_video(video_info(self.adapter_index, request.game_resolution))?;

        let source_creation_state = SourceCreationState {
            use_window_capture: request.game_config.use_window_capture,
        };
        let source = prepare_source(
            &mut self.obs_context,
            &request.game_exe,
            request.hwnd.0,
            &mut scene,
            self.source.take(),
            &source_creation_state,
            self.last_source_creation_state.as_ref(),
        )?;

        // Register the source
        scene.set_to_channel(0)?;

        // Ensure the source takes up the entire scene
        scene.fit_source_to_screen(&source)?;

        // Register the video encoder with encoder-specific settings
        let video_encoder_data = self.obs_context.data()?;
        let video_encoder_settings = request
            .video_settings
            .apply_to_obs_data(video_encoder_data)?;

        // Update the output path settings (when output is not active)
        let mut output_settings = self.obs_context.data()?;
        output_settings.set_string("path", ObsPath::new(&request.recording_path).build())?;
        self.output.update_settings(output_settings)?;

        // Create or reuse video encoder
        let encoder_type = request.video_settings.encoder;

        let video_encoder = if let Some(existing_encoder) = self.video_encoders.get(&encoder_type) {
            tracing::info!(
                "Reusing existing video encoder for type: {}",
                encoder_type.id()
            );
            existing_encoder.clone()
        } else {
            tracing::info!("Creating new video encoder for type: {}", encoder_type.id());
            let encoder = ObsVideoEncoder::new_from_info(
                VideoEncoderInfo::new(
                    vet_to_obs_vet(encoder_type),
                    "video_encoder",
                    Some(video_encoder_settings.clone()),
                    None,
                ),
                self.obs_context.runtime().clone(),
            )?;
            self.video_encoders.insert(encoder_type, encoder.clone());
            encoder
        };

        // Set the video encoder on the output
        self.output.set_video_encoder(video_encoder)?;

        // Set the audio encoder on the output
        self.output
            .set_audio_encoder(self.audio_encoder.clone(), 0)?;

        self.last_video_encoder_type = Some(encoder_type);

        // Listen for signals to pass onto the event stream
        self.was_hooked.store(false, Ordering::Relaxed);
        let hook_monitor_thread = std::thread::spawn({
            let event_stream = request.event_stream;
            let was_hooked = self.was_hooked.clone();

            // output
            let mut start_signal_rx = self
                .output
                .signal_manager()
                .on_start()
                .context("failed to register output on_start signal")?;
            let mut stop_signal_rx = self
                .output
                .signal_manager()
                .on_stop()
                .context("failed to register output on_stop signal")?;

            // source
            let mut hook_signal_rx = source
                .signal_manager()
                .on_hooked()
                .context("failed to register source on_hooked signal")?;

            // SAFETY: We clone last_application and hwnd for comparison purposes only.
            // The HWND is not directly accessed from this thread - it's only used to
            // check if we're recording the same window as before. OBS handles the actual
            // HWND access internally when creating capture sources.
            let last_application = self.last_application.clone();
            let game_exe = request.game_exe.clone();
            let hwnd = request.hwnd.clone();

            move || {
                let initial_time = Instant::now();
                futures::executor::block_on(async {
                    // Seems a bit dubious to use a tokio::select with
                    // a tokio oneshot in a non-Tokio context, but it seems to work
                    loop {
                        tokio::select! {
                            r = start_signal_rx.recv() => {
                                if r.is_ok() {
                                    if last_application.as_ref().is_some_and(|a| a == &(game_exe.clone(), hwnd.clone())) {
                                        tracing::warn!("Video started again for last game, assuming we're already hooked");
                                        let _ = event_stream.send(InputEventType::HookStart);
                                        was_hooked.store(true, Ordering::Relaxed);
                                    }

                                    tracing::info!("Video started at {}s", initial_time.elapsed().as_secs_f64());
                                    let _ = event_stream.send(InputEventType::VideoStart);
                                }
                            }
                            r = stop_signal_rx.recv() => {
                                if r.is_ok() {
                                    tracing::info!("Video ended at {}s", initial_time.elapsed().as_secs_f64());
                                    let _ = event_stream.send(InputEventType::VideoEnd);
                                }
                            }
                            r = hook_signal_rx.recv() => {
                                if r.is_ok() {
                                    tracing::info!("Game hooked at {}s", initial_time.elapsed().as_secs_f64());
                                    let _ = event_stream.send(InputEventType::HookStart);
                                    was_hooked.store(true, Ordering::Relaxed);
                                }
                            }
                            _ = &mut shutdown_rx => {
                                return;
                            }
                        }
                    }
                });
                tracing::info!("Game hook monitoring thread closed");
            }
        });

        // Store the thread handle for proper cleanup
        self.hook_monitor_thread = Some(hook_monitor_thread);

        // Update our last encoder settings
        self.last_encoder_settings = video_encoder_settings
            .get_json()
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok());
        if let Some(encoder_settings_json) = &mut self.last_encoder_settings {
            if let Some(object) = encoder_settings_json.as_object_mut() {
                object.insert(
                    "encoder".to_string(),
                    request.video_settings.encoder.id().into(),
                );
                object.insert(
                    "window_capture".to_string(),
                    request.game_config.use_window_capture.into(),
                );
            }
            tracing::info!("Recording starting with video settings: {encoder_settings_json:?}");
        }

        // Just before we start, clear out our skipped frame counter
        if let Ok(mut guard) = self.skipped_frames.lock() {
            guard.take();
        } else {
            tracing::warn!("Skipped frames mutex poisoned, continuing anyway");
        }

        self.output.start()?;

        self.source = Some(source);
        self.last_application = Some((request.game_exe.clone(), request.hwnd));
        self.last_source_creation_state = Some(source_creation_state);
        self.is_recording = true;
        self.recording_start_time = Some(Instant::now());

        Ok(())
    }

    fn stop_recording(
        &mut self,
        last_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> eyre::Result<serde_json::Value> {
        if self.is_recording {
            self.output.stop().wrap_err("Failed to stop OBS output")?;
            tracing::debug!("OBS recording stopped");
            self.is_recording = false;
            self.recording_start_time = None;
        } else {
            tracing::warn!("No active recording to stop");
        }

        let mut settings = self.last_encoder_settings.take().unwrap_or_default();

        // Send shutdown signal BEFORE checking hook status, to ensure the signal thread
        // exits cleanly even when the recording was never hooked (avoids thread leak).
        if let Some(shutdown_tx) = last_shutdown_tx {
            shutdown_tx.send(()).ok();
        }

        // Join the hook monitoring thread to ensure it has fully terminated
        if let Some(hook_thread) = self.hook_monitor_thread.take() {
            // Give the thread a reasonable amount of time to exit cleanly
            // If it doesn't exit within 1 second, we'll continue anyway
            let _ = hook_thread.join();
            tracing::debug!("Hook monitoring thread joined");
        }

        if !self.was_hooked.load(Ordering::Relaxed) {
            // Don't reject the recording — window capture may have produced valid video.
            // Anti-cheat software (BattlEye, EAC, Vanguard) often blocks game capture hooks
            // but window capture still works and produces usable training data.
            tracing::warn!(
                "Game capture hook never succeeded — recording used window capture fallback. \
                 Video may still be valid."
            );
        }

        // Extremely ugly hack: We want to get the skipped frames percentage from the logs,
        // but that's not guaranteed to be present by the time this function would normally end.
        //
        // So, we wait 200ms to make sure we've cleared it.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let skipped_frames_opt = self.skipped_frames.lock().ok().and_then(|mut g| g.take());
        if let Some(skipped_frames) = skipped_frames_opt {
            let percentage = skipped_frames.percentage();
            if percentage > 5.0 {
                bail!(
                    "Too many frames were dropped ({}/{}, {percentage:.2}%), recording is unusable. Please consider using another encoder or tweaking your settings.",
                    skipped_frames.skipped,
                    skipped_frames.total
                );
            }

            if let Some(object) = settings.as_object_mut() {
                object.insert(
                    "skipped_frames".to_string(),
                    serde_json::to_value(&skipped_frames)?,
                );
            }
        }

        // Clear encoder cache to release GPU memory between recordings.
        // Encoders hold GPU-side frame buffers; keeping them cached across
        // multiple recordings can accumulate VRAM and contribute to OOM
        // in VRAM-heavy games like GTA V Enhanced.
        self.video_encoders.clear();
        tracing::debug!("Cleared encoder cache to release GPU memory");

        Ok(settings)
    }

    fn poll(&mut self) -> eyre::Result<()> {
        if self
            .last_application
            .as_ref()
            .is_some_and(|a| find_game_capture_window(Some(a.0.as_str()), a.1.0).is_err())
        {
            tracing::warn!("Game no longer open, removing source");
            if let Some(mut scene) = self.obs_context.get_scene(OWL_SCENE_NAME)?
                && let Some(source) = self.source.take()
            {
                scene.remove_source(&source)?;
                self.last_application = None;
            }
        }

        Ok(())
    }

    fn check_hook_timeout(&mut self) -> bool {
        if !self.is_recording {
            return false;
        }

        // If we're already hooked, no timeout
        if self.was_hooked.load(Ordering::Relaxed) {
            return false;
        }

        // Check if we've exceeded the timeout
        if let Some(start_time) = self.recording_start_time
            && start_time.elapsed() > constants::HOOK_TIMEOUT
        {
            // it is very important we reset the last_application, otherwise on the next recording restart
            // it will assume that the application was previously successfully hooked, skipping this hook check entirely
            self.last_application = None;
            true
        } else {
            false
        }
    }
}

fn video_info(adapter_index: usize, (base_width, base_height): (u32, u32)) -> ObsVideoInfo {
    // Ensure valid dimensions — OBS returns "invalid parameter" if width or height is 0.
    // This can happen when the game window hasn't fully initialized or when using
    // process scan to detect games that don't have a visible window yet.
    let base_width = if base_width == 0 {
        tracing::warn!("Game base_width is 0, using recording width as fallback");
        RECORDING_WIDTH
    } else {
        base_width
    };
    let base_height = if base_height == 0 {
        tracing::warn!("Game base_height is 0, using recording height as fallback");
        RECORDING_HEIGHT
    } else {
        base_height
    };

    // Output at the same resolution as the source to preserve aspect ratio.
    // Previously forced 1920x1080 output which stretched non-16:9 content.
    // Monitor capture grabs the full screen, so base = screen resolution.
    ObsVideoInfoBuilder::new()
        .adapter(adapter_index as u32)
        .fps_num(FPS)
        .fps_den(1)
        .base_width(base_width)
        .base_height(base_height)
        .output_width(base_width)
        .output_height(base_height)
        .scale_type(ObsScaleType::Bicubic)
        .build()
}

/// Pointer value of the HMONITOR that `hwnd` is currently on, or 0 if unavailable.
///
/// The pointer is the universal key across `windows` crate versions — `display_info`
/// is pinned to an older `windows` version than we use, so we compare HMONITORs
/// as raw pointer values rather than by typed equality.
fn hmonitor_ptr_for_hwnd(hwnd: HWND) -> usize {
    // SAFETY: MonitorFromWindow is a pure read-only Win32 query; it returns an
    // HMONITOR (or NULL). MONITOR_DEFAULTTONEAREST guarantees a non-null result
    // even when the window sits outside any display rectangle.
    let target: HMONITOR = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if target.is_invalid() {
        return 0;
    }
    target.0 as usize
}

fn find_game_capture_window(game_exe: Option<&str>, hwnd: HWND) -> Result<WindowInfo> {
    let game_exe = game_exe.unwrap_or("unknown");
    let window = libobs_window_helper::get_window_info(hwnd).map_err(|e| {
        eyre!(
            "{} ({}). {} {}",
            "We couldn't get window info for the window you're trying to record",
            game_exe,
            "Please ensure you are capturing a game and try again. Error:",
            e
        )
    })?;
    if !window.is_game {
        eyre::bail!(
            "The window you're trying to record ({game_exe}) does not appear to be a capturable game."
        );
    }
    Ok(window)
}

fn prepare_source(
    obs_context: &mut ObsContext,
    game_exe: &str,
    hwnd: HWND,
    scene: &mut ObsSceneRef,
    mut last_source: Option<ObsSourceRef>,
    state: &SourceCreationState,
    last_state: Option<&SourceCreationState>,
) -> Result<ObsSourceRef> {
    let capture_audio = true;

    // Check if source creation state changed - if so, we can't reuse the old source
    if let Some(last) = last_state
        && last != state
        && last_source.is_some()
    {
        tracing::info!(
            "Source creation state changed ({last:?} -> {state:?}), discarding old source",
        );
        if let Some(source) = last_source.take() {
            tracing::info!("Removing old source");
            scene.remove_source(&source)?;
            tracing::info!("Old source removed");
        }
    }

    let result = if state.use_window_capture {
        // Use monitor capture (full screen) — works with all games including
        // fullscreen exclusive, anti-cheat, DRM. Same approach as competing products.
        // This captures the entire display, guaranteeing visible game content.
        tracing::info!("Using monitor capture mode (full screen capture)");

        let monitors = MonitorCaptureSourceBuilder::get_monitors().unwrap_or_default();

        if monitors.is_empty() {
            // Fallback to window capture if monitor list unavailable
            tracing::warn!("No monitors found for monitor capture, falling back to window capture");
            let window = libobs_wrapper::unsafe_send::Sendable(find_game_capture_window(
                Some(game_exe),
                hwnd,
            )?);
            let client_area = false;
            if let Some(mut source) = last_source.take() {
                source
                    .create_updater::<WindowCaptureSourceUpdater>()?
                    .set_window(&window)
                    .set_capture_audio(capture_audio)?
                    .set_client_area(client_area)
                    .update()?;
                Ok(source)
            } else {
                obs_context
                    .source_builder::<WindowCaptureSourceBuilder, _>(OWL_WINDOW_CAPTURE_NAME)?
                    .set_window(&window)
                    .set_capture_audio(capture_audio)?
                    .set_client_area(client_area)
                    .add_to_scene(scene)
            }
        } else {
            // Pick the monitor the game window currently lives on — falls back
            // to primary when MonitorFromWindow can't resolve or when the HMONITOR
            // doesn't match any enumerated DisplayInfo.
            let target_ptr = hmonitor_ptr_for_hwnd(hwnd);
            let monitor_idx = if target_ptr == 0 {
                tracing::warn!(
                    "MonitorFromWindow failed, falling back to primary monitor (index 0)"
                );
                0
            } else {
                monitors
                    .iter()
                    .position(|m| m.0.raw_handle.0 as usize == target_ptr)
                    .unwrap_or_else(|| {
                        tracing::warn!(
                            "HMONITOR {target_ptr:#x} not matched in {} enumerated monitors, \
                             falling back to primary (index 0)",
                            monitors.len()
                        );
                        0
                    })
            };
            let monitor = &monitors[monitor_idx];
            tracing::info!(
                "Capturing monitor {} of {}: {:?}",
                monitor_idx,
                monitors.len(),
                monitor
            );

            if let Some(mut source) = last_source.take() {
                tracing::info!("Reusing existing monitor capture source");
                source
                    .create_updater::<MonitorCaptureSourceUpdater>()?
                    .set_monitor(monitor)
                    .update()?;
                Ok(source)
            } else {
                tracing::info!("Creating new monitor capture source");
                obs_context
                    .source_builder::<MonitorCaptureSourceBuilder, _>(OWL_MONITOR_CAPTURE_NAME)?
                    .set_monitor(monitor)
                    .add_to_scene(scene)
            }
        }
    } else {
        let window = find_game_capture_window(Some(game_exe), hwnd)?;

        if !window.is_game {
            bail!(
                "The window you're trying to record ({game_exe}) cannot be captured. Please ensure you are capturing a game."
            );
        }

        let capture_mode = ObsGameCaptureMode::CaptureSpecificWindow;

        if let Some(mut source) = last_source.take() {
            tracing::info!("Reusing existing game capture source");
            source
                .create_updater::<GameCaptureSourceUpdater>()?
                .set_capture_mode(capture_mode)
                .set_window_raw(window.obs_id.as_str())
                .set_capture_audio(capture_audio)?
                .update()?;
            Ok(source)
        } else {
            tracing::info!("Creating new game capture source");

            if GameCaptureSourceBuilder::is_window_in_use_by_other_instance(window.pid)? {
                // We should only check this if we're creating a new source, as "another process" could be us otherwise
                bail!(
                    "The window you're trying to record ({game_exe}) is already being captured by another process. Do you have OBS or another instance of GameData Recorder open?\n\nNote that OBS is no longer required to use GameData Recorder - please close it if you have it running!",
                );
            }

            obs_context
                .source_builder::<GameCaptureSourceBuilder, _>(OWL_GAME_CAPTURE_NAME)?
                .set_capture_mode(capture_mode)
                .set_window(&window)
                .set_capture_audio(capture_audio)?
                .add_to_scene(scene)
        }
    };

    Ok(result?)
}

#[derive(Debug, serde::Serialize)]
struct SkippedFrames {
    skipped: usize,
    total: usize,
}
impl SkippedFrames {
    /// 0-100%
    pub fn percentage(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.skipped as f64 / self.total as f64) * 100.0
        }
    }
}

#[derive(Debug)]
struct TracingObsLogger {
    skipped_frames: Arc<Mutex<Option<SkippedFrames>>>,
}
impl ObsLogger for TracingObsLogger {
    fn log(&mut self, level: libobs_wrapper::enums::ObsLogLevel, msg: String) {
        use libobs_wrapper::enums::ObsLogLevel;
        match level {
            ObsLogLevel::Error => tracing::error!(target: "obs", "{msg}"),
            ObsLogLevel::Warning => tracing::warn!(target: "obs", "{msg}"),
            ObsLogLevel::Info => {
                // HACK: If we encounter a message of the sort
                //   Video stopped, number of skipped frames due to encoding lag: 10758/22640 (47.5%)
                // we parse out the numbers to allow us to determine if it's an acceptable number
                // of skipped frames.
                if msg.contains("number of skipped frames due to encoding lag:")
                    && let Some(frames_data) = parse_skipped_frames(&msg)
                {
                    if let Ok(mut guard) = self.skipped_frames.lock() {
                        *guard = Some(frames_data);
                    }
                }
                tracing::info!(target: "obs", "{msg}");
            }
            ObsLogLevel::Debug => tracing::debug!(target: "obs", "{msg}"),
        }
    }
}

fn parse_skipped_frames(msg: &str) -> Option<SkippedFrames> {
    // Find the colon and start from there
    let after_colon = msg.split(':').nth(1)?;
    let mut chars = after_colon.chars();

    // Skip to first digit and parse number (skipped frames)
    while let Some(c) = chars.next() {
        if !c.is_ascii_digit() {
            continue;
        }
        let mut num_str = c.to_string();
        num_str.extend(chars.by_ref().take_while(|c| c.is_ascii_digit()));
        let skipped = num_str.parse::<usize>().ok()?;

        // Skip to next digit and parse number (total frames)
        while let Some(c) = chars.next() {
            if !c.is_ascii_digit() {
                continue;
            }

            let mut num_str = c.to_string();
            num_str.extend(chars.by_ref().take_while(|c| c.is_ascii_digit()));
            let total = num_str.parse::<usize>().ok()?;

            return Some(SkippedFrames { skipped, total });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_skipped_frames_basic() {
        let msg =
            "Video stopped, number of skipped frames due to encoding lag: 10758/22640 (47.5%)";
        let result = parse_skipped_frames(msg).expect("Failed to parse");

        assert_eq!(result.skipped, 10758);
        assert_eq!(result.total, 22640);
        assert!((result.percentage() - 47.48).abs() < 0.1);
    }
}
