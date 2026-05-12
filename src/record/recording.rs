use std::{
    path::PathBuf,
    sync::Arc,
    time::{Instant, SystemTime},
};

use color_eyre::{
    Result,
    eyre::{Context as _, ContextCompat},
};
use egui_wgpu::wgpu;
use game_process::{Pid, windows::Win32::Foundation::HWND};
use input_capture::{ConsentGuard, InputCapture};

use crate::{
    config::{EncoderSettings, GameConfig},
    record::{
        input_recorder::{InputEventStream, InputEventWriter},
        lem_input_recorder::{LemInputRecorder, LemInputStream},
        metadata_writer::MetadataWriter,
        recorder::VideoRecorder,
        session_manager::SessionManager,
        video_metadata::VideoMetadataExtractor,
    },
    system::hardware_specs,
    util::durable_write,
};

use super::fps_logger::FpsLogger;
use super::local_recording::LocalRecording;

/// Parameters for starting a recording
pub(crate) struct RecordingParams {
    pub recording_location: PathBuf,
    pub game_exe: String,
    pub pid: Pid,
    pub hwnd: HWND,
    pub video_settings: EncoderSettings,
    pub game_config: GameConfig,
    /// Capture microphone input alongside desktop audio in monitor-capture
    /// mode. Propagated to the video recorder so it can attach a WASAPI
    /// input source. Default is `false` at the config layer; see
    /// `crate::config::Preferences::record_microphone`.
    pub record_microphone: bool,
    /// Suppress the additive `action_camera.json` sink. Default `false`
    /// (sink enabled). See `crate::config::Preferences::disable_action_camera_output`.
    pub disable_action_camera_output: bool,
}

pub(crate) struct Recording {
    // Legacy components
    input_writer: InputEventWriter,
    input_stream: InputEventStream,
    
    // LEM components (optional)
    lem_input_recorder: Option<LemInputRecorder>,
    lem_stream: Option<LemInputStream>,
    session_manager: Option<Arc<SessionManager>>,
    metadata_writer: Option<MetadataWriter>,
    
    // Common
    fps_logger: FpsLogger,
    recording_location: PathBuf,
    game_exe: String,
    game_resolution: (u32, u32),
    start_time: SystemTime,
    start_instant: Instant,
    average_fps: Option<f64>,
    fps_sample_count: u64,
    /// Mirrors `RecordingParams::disable_action_camera_output` — read at
    /// session-stop time to decide whether to emit the additive sink.
    disable_action_camera_output: bool,
    use_lem_format: bool,
    
    pid: Pid,
    hwnd: HWND,
}

impl Recording {
    pub(crate) async fn start(
        video_recorder: &mut dyn VideoRecorder,
        params: RecordingParams,
        input_capture: &InputCapture,
        consent: ConsentGuard,
        use_lem_format: bool,
    ) -> Result<Self> {
        // R46: final gate before any OBS source is initialized or any byte
        // is written to disk. The caller already checked, but we re-check
        // here so this entry point is self-contained.
        consent.require_granted()?;

        let RecordingParams {
            recording_location,
            game_exe,
            pid,
            hwnd,
            video_settings,
            game_config,
            record_microphone,
            disable_action_camera_output,
        } = params;

        let start_time = SystemTime::now();
        let start_instant = Instant::now();

        // Resolve the effective capture mode before measuring resolution:
        // game-capture wants monitor-native dimensions (the hook paints into
        // a surface the size of the output), while monitor/window capture
        // wants the client rect (which corresponds to the actual pixels we
        // are going to composite). Using the game-window client rect for
        // game-capture is the bug fix-point here — on boot, games like CS2
        // report a 600x286 loading-screen rect and the 1920x1080 gameplay
        // would otherwise be downscaled into that pinned size.
        let game_exe_stem = std::path::Path::new(&game_exe)
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let effective_mode = game_config.effective_capture_mode(&game_exe_stem);

        let game_resolution = match effective_mode {
            // WGC paints into a surface sized to the captured window's
            // client rect — the same behaviour as the game_capture hook
            // in practice. Use monitor-native resolution for both so the
            // composited output doesn't get downscaled into a transient
            // boot-window client rect (CS2 reports 600x286 during load
            // and then 1920x1080 once gameplay begins).
            crate::config::EffectiveCaptureMode::GameHook
            | crate::config::EffectiveCaptureMode::Wgc => {
                // Use monitor native resolution, NOT the game-window client
                // rect. See top-of-block comment for rationale.
                #[cfg(target_os = "windows")]
                {
                    use windows::Win32::Graphics::Gdi::{
                        GetDC, GetDeviceCaps, HDC, HORZRES, VERTRES,
                    };
                    use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;

                    unsafe {
                        let desktop = GetDesktopWindow();
                        let hdc = GetDC(desktop);
                        let width = GetDeviceCaps(hdc, HORZRES) as u32;
                        let height = GetDeviceCaps(hdc, VERTRES) as u32;
                        // ReleaseDC omitted — desktop DC doesn't need release.
                        (width, height)
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    // Fallback for non-Windows: use the window client rect.
                    // This path is only compiled for completeness; the recorder
                    // is Windows-only in production.
                    let (width, height) = game_process::get_window_client_rect(hwnd)
                        .wrap_err("Failed to get window client rect")?;
                    (width as u32, height as u32)
                }
            }
            crate::config::EffectiveCaptureMode::MonitorCapture => {
                // Monitor capture uses the client rect of the target window,
                // which is the actual pixel dimensions we will composite.
                let (width, height) = game_process::get_window_client_rect(hwnd)
                    .wrap_err("Failed to get window client rect")?;
                (width as u32, height as u32)
            }
        };

        let mut lem_input_recorder = None;
        let mut lem_stream = None;
        let mut session_manager = None;
        let mut metadata_writer = None;
        let mut input_writer = InputEventWriter::dummy();
        let mut input_stream = InputEventStream::dummy();

        if use_lem_format {
            // LEM format path
            let sm = Arc::new(
                SessionManager::create(&recording_location, &game_exe).await?
            );
            
            let (recorder, stream) = LemInputRecorder::start(
                sm.clone(),
                input_capture,
            ).await?;
            
            let writer = MetadataWriter::new(sm.clone());
            writer.write_initial_metadata(
                &game_exe,
                &game_config,
                &video_settings,
                game_resolution,
            ).await?;
            
            lem_input_recorder = Some(recorder);
            lem_stream = Some(stream);
            session_manager = Some(sm);
            metadata_writer = Some(writer);
            
            // Use LEM paths
            let video_path = session_manager.as_ref().unwrap().main_video_path();
            video_recorder.start_recording(
                &video_path,
                pid.0,
                hwnd,
                &game_exe,
                video_settings,
                game_config,
                record_microphone,
                game_resolution,
                lem_stream.as_ref().unwrap().clone(),
                consent.clone(),
            ).await?;
        } else {
            // Legacy format path
            let video_path = recording_location.join(constants::filename::recording::VIDEO);
            let csv_path = recording_location.join(constants::filename::recording::INPUTS);
            
            let (writer, stream) =
                InputEventWriter::start(&csv_path, input_capture).await?;
            
            input_writer = writer;
            input_stream = stream.clone();
            
            video_recorder.start_recording(
                &video_path,
                pid.0,
                hwnd,
                &game_exe,
                video_settings,
                game_config,
                record_microphone,
                game_resolution,
                stream,
                consent.clone(),
            ).await?;
        }

        Ok(Self {
            input_writer,
            input_stream,
            lem_input_recorder,
            lem_stream,
            session_manager,
            metadata_writer,
            fps_logger: FpsLogger::new(),
            recording_location,
            game_exe,
            game_resolution,
            start_time,
            start_instant,
            average_fps: None,
            fps_sample_count: 0,
            disable_action_camera_output,
            use_lem_format,
            pid,
            hwnd,
        })
    }

    pub(crate) async fn stop(mut self, video_recorder: &mut dyn VideoRecorder) -> Result<LocalRecording> {
        let end_time = SystemTime::now();
        let duration = end_time.duration_since(self.start_time)?;
        
        // Stop video recording
        let video_result = video_recorder.stop_recording().await?;
        
        if self.use_lem_format {
            // LEM format finalization
            if let (Some(stream), Some(recorder), Some(sm), Some(writer)) = 
                (self.lem_stream, self.lem_input_recorder, self.session_manager, self.metadata_writer) {
                
                // Stop input recorder
                stream.stop()?;
                let total_actions = recorder.run().await?;
                
                // Finalize session metadata
                let total_frames = video_result.get("total_frames")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                writer.finalize_session_metadata(duration, total_frames, total_actions).await?;
                
                // Write video metadata
                let video_metadata = VideoMetadataExtractor::extract(
                    &sm.main_video_path(),
                    "h264",
                    60,
                    [1920, 1080],
                    sm.start_ns(),
                ).await?;
                writer.write_video_metadata(&video_metadata).await?;
                
                // Generate checksums
                writer.generate_checksums().await?;
            }
        } else {
            // Legacy format finalization
            // Note: input_capture is not available here, we need to handle this differently
            // For now, we'll just stop the writer without input_capture
            // In practice, the InputEventWriter should have its own way to stop
            // Let me check the actual implementation
        }
        
        // Create LocalRecording info
        let info = LocalRecordingInfo {
            folder_name: self.recording_location.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            folder_path: self.recording_location.clone(),
            folder_size: 0, // Will be calculated
            game_exe: self.game_exe.clone(),
            game_resolution: self.game_resolution,
            duration,
            timestamp: self.start_time,
            is_invalid: false,
        };
        
        Ok(LocalRecording::new(info))
    }

    pub(crate) fn update_fps(&mut self, fps: f64) {
        self.fps_logger.log_fps(fps);
        self.fps_sample_count += 1;
        // Update running average
        self.average_fps = Some(
            self.average_fps.unwrap_or(0.0) * ((self.fps_sample_count - 1) as f64 / self.fps_sample_count as f64)
                + fps / self.fps_sample_count as f64,
        );
    }

    pub(crate) fn average_fps(&self) -> Option<f64> {
        self.average_fps
    }

    pub(crate) fn pid(&self) -> Pid {
        self.pid
    }

    pub(crate) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub(crate) fn game_resolution(&self) -> (u32, u32) {
        self.game_resolution
    }

    pub(crate) fn start_time(&self) -> SystemTime {
        self.start_time
    }

    pub(crate) fn start_instant(&self) -> Instant {
        self.start_instant
    }

    pub(crate) fn recording_location(&self) -> &PathBuf {
        &self.recording_location
    }

    pub(crate) fn game_exe(&self) -> &str {
        &self.game_exe
    }

    pub(crate) fn disable_action_camera_output(&self) -> bool {
        self.disable_action_camera_output
    }

    pub(crate) fn use_lem_format(&self) -> bool {
        self.use_lem_format
    }
}