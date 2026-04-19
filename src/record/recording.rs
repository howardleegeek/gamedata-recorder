use std::{
    path::PathBuf,
    time::{Instant, SystemTime},
};

use color_eyre::{Result, eyre::ContextCompat};
use egui_wgpu::wgpu;
use game_process::{Pid, windows::Win32::Foundation::HWND};
use input_capture::{ConsentGuard, InputCapture};

use crate::{
    config::{EncoderSettings, GameConfig},
    record::{
        input_recorder::{InputEventStream, InputEventWriter},
        recorder::VideoRecorder,
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
}

pub(crate) struct Recording {
    input_writer: InputEventWriter,
    input_stream: InputEventStream,
    fps_logger: FpsLogger,

    recording_location: PathBuf,
    game_exe: String,
    game_resolution: (u32, u32),
    start_time: SystemTime,
    start_instant: Instant,
    average_fps: Option<f64>,
    fps_sample_count: u64,

    pid: Pid,
    hwnd: HWND,
}

impl Recording {
    pub(crate) async fn start(
        video_recorder: &mut dyn VideoRecorder,
        params: RecordingParams,
        input_capture: &InputCapture,
        consent: ConsentGuard,
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
        } = params;

        let start_time = SystemTime::now();
        let start_instant = Instant::now();

        let game_resolution = get_recording_base_resolution(hwnd)?;
        tracing::info!("Game resolution: {game_resolution:?}");

        let video_path = recording_location.join(constants::filename::recording::VIDEO);
        let csv_path = recording_location.join(constants::filename::recording::INPUTS);

        let (input_writer, input_stream) =
            InputEventWriter::start(&csv_path, input_capture).await?;
        video_recorder
            .start_recording(
                &video_path,
                pid.0,
                hwnd,
                &game_exe,
                video_settings,
                game_config,
                game_resolution,
                input_stream.clone(),
                consent,
            )
            .await?;

        Ok(Self {
            input_writer,
            input_stream,
            fps_logger: FpsLogger::new(),
            recording_location,
            game_exe,
            game_resolution,
            start_time,
            start_instant,
            average_fps: None,
            fps_sample_count: 0,

            pid,
            hwnd,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn game_exe(&self) -> &str {
        &self.game_exe
    }

    #[allow(dead_code)]
    pub(crate) fn start_time(&self) -> SystemTime {
        self.start_time
    }

    #[allow(dead_code)]
    pub(crate) fn start_instant(&self) -> Instant {
        self.start_instant
    }

    #[allow(dead_code)]
    pub(crate) fn elapsed(&self) -> std::time::Duration {
        self.start_instant.elapsed()
    }

    #[allow(dead_code)]
    pub(crate) fn pid(&self) -> Pid {
        self.pid
    }

    #[allow(dead_code)]
    pub(crate) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub(crate) fn game_resolution(&self) -> (u32, u32) {
        self.game_resolution
    }

    pub(crate) fn get_window_name(&self) -> Option<String> {
        use game_process::windows::Win32::UI::WindowsAndMessaging::{
            GetWindowTextLengthW, GetWindowTextW,
        };

        let title_len = unsafe { GetWindowTextLengthW(self.hwnd) };
        if title_len <= 0 || title_len > 4096 {
            // 0 means error or empty title; cap at 4096 to prevent absurd allocations
            return None;
        }
        {
            let mut buf = vec![0u16; (title_len + 1) as usize];
            let copied = unsafe { GetWindowTextW(self.hwnd, &mut buf) };
            if copied > 0 {
                if let Some(end) = buf.iter().position(|&c| c == 0) {
                    return Some(String::from_utf16_lossy(&buf[..end]));
                } else {
                    return Some(String::from_utf16_lossy(&buf));
                }
            }
        }
        None
    }

    pub(crate) fn input_stream(&self) -> &InputEventStream {
        &self.input_stream
    }

    /// Flush all pending input events to disk
    pub(crate) async fn flush_input_events(&mut self) -> Result<()> {
        self.input_writer.flush().await
    }

    pub(crate) fn update_fps(&mut self, fps: f64) {
        // True cumulative average (not exponential decay which biases toward recent samples)
        self.fps_sample_count += 1;
        self.average_fps = Some(match self.average_fps {
            Some(avg) => avg + (fps - avg) / self.fps_sample_count as f64,
            None => fps,
        });
        // Feed frame timing data to the per-second FPS logger
        self.fps_logger.on_frame();
    }

    pub(crate) async fn stop(
        self,
        recorder: &mut dyn VideoRecorder,
        adapter_infos: &[wgpu::AdapterInfo],
        input_capture: &InputCapture,
    ) -> Result<()> {
        let window_name = self.get_window_name();
        let mut result = recorder.stop_recording().await;

        // Don't propagate input_writer errors — treat like recorder errors
        // (write INVALID marker instead of returning Err which skips metadata)
        if let Err(e) = self.input_writer.stop(input_capture).await {
            tracing::error!("Failed to stop input writer: {e}");
            if result.is_ok() {
                result = Err(e);
            }
        }

        // Save per-second FPS log + per-frame frames.jsonl (buyer spec requirement).
        // Frame count is captured here and forwarded to metadata below.
        let frame_count = match self.fps_logger.save(&self.recording_location).await {
            Ok(n) => Some(n),
            Err(e) => {
                tracing::warn!("Failed to save FPS log / frames.jsonl: {e}");
                None
            }
        };

        #[allow(clippy::collapsible_if)]
        if result.is_ok() {
            // Conditions that need to be met, even if the recording is otherwise valid
            if let Some(average_fps) = self.average_fps
                && average_fps < constants::MIN_AVERAGE_FPS
            {
                result = Err(color_eyre::eyre::eyre!(
                    "Average FPS {average_fps:.1} is below required minimum of {:.1}",
                    constants::MIN_AVERAGE_FPS
                ));
            }
        }

        if let Err(e) = result {
            tracing::error!("Error while stopping recording, invalidating recording: {e}");
            // Best-effort write — may fail on disk full, which is acceptable.
            // Use atomic write so a partial INVALID marker from a second-level
            // crash can't promote the recording back to Unuploaded. The helper
            // runs on spawn_blocking; errors are reported but not propagated.
            let invalid_path = self
                .recording_location
                .join(constants::filename::recording::INVALID);
            let reason = e.to_string().into_bytes();
            let write_result = tokio::task::spawn_blocking(move || {
                durable_write::write_atomic(&invalid_path, &reason)
            })
            .await;
            match write_result {
                Ok(Ok(())) => {}
                Ok(Err(write_err)) => {
                    tracing::error!("Failed to write INVALID marker (disk full?): {write_err}");
                }
                Err(join_err) => {
                    tracing::error!("Failed to join INVALID marker write task: {join_err}");
                }
            }
            return Ok(());
        }

        // CRITICAL: fsync the MP4 before writing metadata.json.
        //
        // OBS closes the MP4 file inside its own thread as part of
        // `stop_recording`, but "close" only schedules the final block
        // flushes; on a clean shutdown the kernel flushes them shortly
        // after. On an UNCLEAN shutdown (power loss, hard kill) the MP4's
        // moov atom (written last by libobs-ffmpeg-mux) can still be sitting
        // in the page cache when the process dies — at which point
        // metadata.json will claim a valid recording exists but the MP4 is
        // unplayable (no moov, no seek index, truncated at some arbitrary
        // stream offset). The fsync here forces the MP4 to disk BEFORE we
        // commit metadata, so the invariant "metadata.json exists ⇒ MP4 is
        // playable" is preserved across power loss.
        //
        // Runs on spawn_blocking because fsync on a 10-min H.265 file can
        // easily take >100ms on a spinning disk, and we don't want to stall
        // the tokio reactor for that duration.
        let mp4_path = self
            .recording_location
            .join(constants::filename::recording::VIDEO);
        if mp4_path.exists() {
            let mp4_for_fsync = mp4_path.clone();
            let fsync_result =
                tokio::task::spawn_blocking(move || durable_write::fsync_file(&mp4_for_fsync))
                    .await;
            match fsync_result {
                Ok(Ok(())) => {
                    tracing::debug!("MP4 fsync'd before metadata write: {}", mp4_path.display());
                }
                Ok(Err(e)) => {
                    // Swallow the error — we still want to write metadata and
                    // validate. The validator will catch an unplayable MP4
                    // downstream and mark the recording INVALID. Logging at
                    // warn so we see this in the field.
                    tracing::warn!(
                        "Failed to fsync MP4 before metadata write, continuing: {} (err={:?})",
                        mp4_path.display(),
                        e
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to join MP4 fsync task: {e}");
                }
            }
        } else {
            // The recorder reported success but produced no MP4 — rare, but
            // possible on some encoder-failure paths. We let the validator
            // flag it.
            tracing::warn!(
                "MP4 file missing after successful stop_recording: {}",
                mp4_path.display()
            );
        }

        let gamepads = input_capture.gamepads();
        LocalRecording::write_metadata_and_validate(
            self.recording_location,
            self.game_exe,
            self.game_resolution,
            self.start_instant,
            self.start_time,
            self.average_fps,
            window_name,
            adapter_infos,
            gamepads,
            recorder.id(),
            result.as_ref().ok().cloned(),
            frame_count,
        )
        .await?;

        Ok(())
    }
}

pub fn get_recording_base_resolution(hwnd: HWND) -> Result<(u32, u32)> {
    use windows::Win32::{Foundation::RECT, UI::WindowsAndMessaging::GetClientRect};

    /// Returns the size (width, height) of the inner area of a window given its HWND.
    /// Returns None if the window does not exist or the call fails.
    fn get_window_inner_size(hwnd: HWND) -> Option<(u32, u32)> {
        unsafe {
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect).ok()?;
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            Some((width as u32, height as u32))
        }
    }

    match get_window_inner_size(hwnd) {
        Some(size) => Ok(size),
        None => {
            tracing::info!("Failed to get window inner size, using primary monitor resolution");
            hardware_specs::get_primary_monitor_resolution()
                .context("Failed to get primary monitor resolution")
        }
    }
}
