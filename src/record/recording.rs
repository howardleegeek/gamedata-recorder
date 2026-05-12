use std::{
    path::PathBuf,
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
        recorder::VideoRecorder,
        telemetry,
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
    /// Mirrors `RecordingParams::disable_action_camera_output` — read at
    /// session-stop time to decide whether to emit the additive sink.
    disable_action_camera_output: bool,

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
                    match get_monitor_resolution_for_hwnd(hwnd) {
                        Ok(wh) => {
                            tracing::info!(
                                ?wh,
                                mode = ?effective_mode,
                                game_exe_stem,
                                "Recording::start: using monitor-native resolution for game-capture"
                            );
                            wh
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "Failed to get monitor resolution for game-capture, falling back to window client rect"
                            );
                            get_recording_base_resolution(hwnd)?
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    get_recording_base_resolution(hwnd)?
                }
            }
            crate::config::EffectiveCaptureMode::MonitorCapture
            | crate::config::EffectiveCaptureMode::WindowCapture => {
                // Monitor/window capture reads the client rect directly
                // from the target window (or the whole monitor). Use that
                // rect as the output size.
                get_recording_base_resolution(hwnd)?
            }
        };

        tracing::info!(
            ?game_resolution,
            mode = ?effective_mode,
            game_exe_stem,
            "Recording::start: resolved game resolution"
        );

        // Start the video recorder with the resolved resolution
        video_recorder
            .start_recording(
                &recording_location,
                game_resolution,
                video_settings,
                record_microphone,
            )
            .await
            .context("Failed to start video recorder")?;

        // Start input recording
        let (input_writer, input_stream) = crate::record::input_recorder::start(
            &recording_location,
            input_capture,
            consent,
        )
        .await?;

        // Start FPS logger
        let fps_logger = FpsLogger::new(&recording_location).await?;

        Ok(Self {
            input_writer,
            input_stream,
            fps_logger,

            recording_location,
            game_exe,
            game_resolution,
            start_time,
            start_instant,
            average_fps: None,
            fps_sample_count: 0,
            disable_action_camera_output,

            pid,
            hwnd,
        })
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
        let dropped_input_events = match self.input_writer.stop(input_capture).await {
            Ok(count) => count,
            Err(e) => {
                tracing::error!("Failed to stop input writer: {e}");
                if result.is_ok() {
                    result = Err(e);
                }
                0 // Default to 0 if error occurred
            }
        };

        // Log if any input events were dropped
        if dropped_input_events > 0 {
            let percentage = if self.fps_sample_count > 0 {
                (dropped_input_events as f64 / self.fps_sample_count as f64) * 100.0
            } else {
                0.0
            };
            tracing::warn!(
                "Recording had {} dropped input events ({:.2}%)",
                dropped_input_events,
                percentage
            );
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

        // action_camera.json — additive sink for the buyer plugin's wire
        // contract. Reads inputs.jsonl + frames.jsonl back from disk (both
        // already durably written above) and produces the per-frame array
        // mirroring oyster-enrichment/bin/convert_to_action_camera.py.
        //
        // Failures here are logged and swallowed: the file is purely
        // additive, and we never want to invalidate an otherwise-good
        // recording over a sink that downstream tooling can rebuild.
        if !self.disable_action_camera_output {
            let (w, h) = self.game_resolution;
            match super::action_camera_writer::write_action_camera_json(
                &self.recording_location,
                w,
                h,
            )
            .await
            {
                Ok(n) => {
                    tracing::info!("action_camera.json: wrote {n} frame records");
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to write action_camera.json (recording NOT invalidated, \
                         downstream can rebuild from inputs.jsonl + frames.jsonl): {e}"
                    );
                }
            }
        } else {
            tracing::debug!(
                "action_camera.json sink disabled via Preferences::disable_action_camera_output"
            );
        }

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

            // Validate dropped input events rate
            // Threshold: 1% of frames or at least 100 events (whichever is higher)
            // This prevents data integrity issues while avoiding false positives on short recordings
            if dropped_input_events > 0 {
                let dropped_threshold = frame_count
                    .map(|fc| fc as f64 * 0.01) // 1% of frames
                    .unwrap_or(100.0)
                    .max(100.0) // At least 100 events
                    as u64;

                if dropped_input_events > dropped_threshold {
                    result = Err(color_eyre::eyre::eyre!(
                        "Dropped {} input events exceeds threshold of {} ({}%), recording data may be incomplete",
                        dropped_input_events,
                        dropped_threshold,
                        if let Some(fc) = frame_count {
                            (dropped_input_events as f64 / fc as f64) * 100.0
                        } else {
                            0.0
                        }
                    ));
                }
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
                    // warn level so we can see it in production logs.
                    tracing::warn!("MP4 fsync failed (disk full?): {e}");
                }
                Err(join_err) => {
                    tracing::error!("Failed to join MP4 fsync task: {join_err}");
                }
            }
        } else {
            // This should never happen — `stop_recording` succeeded but the
            // MP4 file is missing. Log at error and continue; the validator
            // will catch the missing MP4 downstream and mark the recording
            // INVALID.
            tracing::error!(
                "MP4 file missing after successful stop_recording: {}",
                mp4_path.display()
            );
        }

        // rc16.4 — capture-thread diagnostics, surfaced so we can tell
        // "hook never installed" from "hook installed but no events".
        let input_capture_diagnostics = input_capture.diagnostics();

        // rc16.4 — gamepad enumeration at recording stop (not start) so we
        // capture the final state, not the initial state. This matches the
        // PRD requirement "list all gamepads present at the end of the
        // session".
        let gamepads = input_capture.gamepads();

        // rc17.2 / Stream BD — PRD page 3 `recordDpi`: OS scaling factor at
        // recording start. `GetDpiForSystem` returns raw DPI (e.g., 144 for
        // 150% scaling), but the wire field expects the scale factor
        // (1.0 / 1.5 / 2.0), so we convert the raw DPI to a scale relative
        // to the Windows 96-DPI baseline. `GetDpiForSystem` is the
        // process-wide value; per-monitor V2 awareness is declared in
        // build.rs (see log_monitor_dpi_scale in obs_embedded_recorder.rs).
        // Detection is a pure read-only Win32 call and cannot fail, but we
        // guard it behind a cfg so non-Windows builds (test harness on Mac)
        // emit None rather than fabricating a value.
        let record_dpi = detect_system_dpi_scale();

        // rc17.2 / Stream BN — keep a copy of the session directory so we
        // can re-lint after metadata flush. `write_metadata_and_validate`
        // consumes the original `self.recording_location`, so the clone is
        // the only handle we have left for the post-finalize lint v3 hook
        // wired below.
        let session_dir_for_lint = self.recording_location.clone();

        // rc17.2.2 / Stream BJ — extra clones for the gameinfo.xlsx
        // writer and the per-frame depth EXR writer. Both run AFTER
        // metadata flush but BEFORE the lint v3 hook, so lint can
        // verify both outputs exist (criteria #23-26). gameinfo is
        // awaited (cheap, <1s); depth is fire-and-forget tokio::spawn
        // because DepthAnything V2 at 1Hz for ~300 frames takes
        // minutes on CPU/DML and must not block the user from quitting
        // the recorder.
        let session_dir_for_xlsx = self.recording_location.clone();
        let session_dir_for_depth = self.recording_location.clone();

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
            dropped_input_events,
            input_capture_diagnostics,
            record_dpi,
        )
        .await?;

        // Stream BJ (rc17.2.2): generate gameinfo.xlsx + spawn depth-EXR
        // background job. Both run AFTER metadata flush so they can read
        // metadata.json / frames.jsonl. gameinfo is small + cheap, await
        // it; depth EXR at 1080p × 1Hz × DepthAnything V2 takes minutes
        // and must NOT block the user from closing the recorder — fire
        // and forget. Both writers log their own errors via tracing and
        // never propagate failures here (advisory output: missing xlsx
        // / depth files only affect lint v3 criteria #23-26 and are
        // surfaced to the user via BN's toast, not a hard error).
        match super::gameinfo_writer::write_gameinfo_xlsx(&session_dir_for_xlsx).await {
            Ok(rows) => tracing::info!(rows = rows, "Recording::stop: gameinfo.xlsx written"),
            Err(e) => tracing::warn!(error = %e, "gameinfo_writer failed (advisory)"),
        }
        tokio::spawn(async move {
            match super::depth_exr_writer::write_depth_exr(
                &session_dir_for_depth,
                Some((1920, 1080)),
                Some("auto".to_string()),
            )
            .await
            {
                Ok(n) => tracing::info!(frames = n, "depth EXR background job complete"),
                Err(e) => tracing::warn!(error = %e, "depth_exr_writer failed (advisory)"),
            }
        });

        // Stream BN (rc17.2): automatic post-session lint v3.
        //
        // EVERY recorded session self-verifies against the 32 PRD
        // criteria BEFORE entering the upload pipeline. This catches
        // the regression Howard's tester reported on 2026-05-11
        // ("yesterday's data had many nulls"): null `camera_position`,
        // null rotations, null `player_*`, marker-only inputs.jsonl,
        // missing gameinfo.xlsx, missing depth EXR. Streams BG / BH /
        // BJ are fixing the root causes upstream, but until those
        // land, lint v3 is the safety net that ensures we never
        // silently ship a bad session.
        //
        // Failure isolation: lint runs are advisory only — a failed
        // or errored lint MUST NOT invalidate the session here. The
        // `lint_result.json` file we write next to metadata.json is
        // the contract the uploader / Stream T pre-upload gate reads
        // to make the gating decision. See
        // `src/record/validation.rs` for the full rationale.
        let lint_result = match super::validation::run_lint_v3(session_dir_for_lint.clone()).await {
            Ok(lint_result) => {
                tracing::info!(
                    status = %lint_result.overall_status,
                    passed = lint_result.passed,
                    failed = lint_result.failed,
                    "Recording::stop: post-session lint v3 finished"
                );
                Some(lint_result)
            }
            Err(e) => {
                // `run_lint_v3` itself returns `Ok(_)` even on lint
                // errors (it materializes an ERROR-status
                // LintResult); reaching this arm means a genuine I/O
                // error somewhere in the result-write path. Log and
                // continue — Recording::stop has already produced a
                // valid session on disk.
                tracing::warn!(
                    error = %e,
                    "Recording::stop: post-session lint v3 raised I/O error (session NOT invalidated)"
                );
                None
            }
        };

        // Stream OTLP — telemetry to Oyster servers.
        // Send session telemetry (best-effort, non-blocking) after lint v3.
        telemetry::spawn_telemetry_task(&session_dir_for_lint, lint_result);

        Ok(())
    }

    /// Returns the window title (if any) of the game window being recorded.
    /// Used for metadata.json's `window_name` field.
    fn get_window_name(&self) -> Option<String> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::UI::WindowsAndMessaging::{
                GetWindowTextW, GW_OWNER,
            };

            // SAFETY: `GetWindowTextW` reads the window's title bar text.
            // It's a pure query with no side effects; the HWND is valid
            // because we're actively capturing it.
            let mut buffer = [0u16; 256];
            let len = unsafe { GetWindowTextW(self.hwnd, &mut buffer) };
            if len > 0 {
                let title = String::from_utf16_lossy(&buffer[..len as usize]);
                Some(title.trim().to_string())
            } else {
                None
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }

    /// Update FPS statistics for the current recording.
    /// Called from the video recorder's frame callback.
    pub(crate) fn update_fps(&mut self, fps: f64) {
        self.fps_sample_count += 1;
        // Update running average
        if let Some(avg) = self.average_fps {
            // Weighted average: new average = (old average * (n-1) + new value) / n
            self.average_fps = Some((avg * (self.fps_sample_count - 1) as f64 + fps) / self.fps_sample_count as f64);
        } else {
            self.average_fps = Some(fps);
        }
    }

    /// Get the current average FPS.
    pub(crate) fn average_fps(&self) -> Option<f64> {
        self.average_fps
    }

    /// Get the number of FPS samples taken so far.
    pub(crate) fn fps_sample_count(&self) -> u64 {
        self.fps_sample_count
    }
}

/// System DPI scale factor relative to the 96-DPI Windows baseline.
///
/// `1.0` = 100% (default), `1.5` = 150%, `2.0` = 200%. Maps directly to the
/// PRD `recordDpi` field in `systeminfo.json` (BUYER_SPEC_V1.md §1). Returns
/// `None` on non-Windows builds (the recorder is Windows-only in
/// production, but lint runs cross-platform and the metadata schema is
/// shared).
fn detect_system_dpi_scale() -> Option<f64> {
    #[cfg(windows)]
    {
        // SAFETY: `GetDpiForSystem` is a parameter-free, side-effect-free
        // Win32 query that returns the process-wide system DPI. Available
        // since Windows 10 1607; build.rs declares per-monitor V2 DPI
        // awareness so this returns the unscaled physical DPI of the
        // primary monitor (not a virtualized 96).
        use windows::Win32::UI::HiDpi::GetDpiForSystem;
        let dpi = unsafe { GetDpiForSystem() };
        if dpi == 0 {
            // Defensive: docs don't list 0 as a return but check anyway —
            // we'd rather emit None than divide by 0 / propagate a bogus
            // scale.
            tracing::warn!("GetDpiForSystem returned 0; omitting recordDpi");
            None
        } else {
            Some(dpi as f64 / 96.0)
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub fn get_recording_base_resolution(hwnd: HWND) -> Result<(u32, u32)> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::{
            Foundation::{RECT, BOOL},
            Graphics::Gdi::{GetClientRect, ClientToScreen, GetWindowRect},
            UI::WindowsAndMessaging::GetWindowLongW,
        };

        // SAFETY: `GetClientRect` reads the window's client-area dimensions.
        // It's a pure query with no side effects; the HWND is valid because
        // we're actively capturing it.
        let mut client_rect = RECT::default();
        let success = unsafe { GetClientRect(hwnd, &mut client_rect) };
        if success.as_bool() {
            let width = (client_rect.right - client_rect.left) as u32;
            let height = (client_rect.bottom - client_rect.top) as u32;
            Ok((width, height))
        } else {
            // Fall back to window rect if client rect fails
            let mut window_rect = RECT::default();
            let success = unsafe { GetWindowRect(hwnd, &mut window_rect) };
            if success.as_bool() {
                let width = (window_rect.right - window_rect.left) as u32;
                let height = (window_rect.bottom - window_rect.top) as u32;
                Ok((width, height))
            } else {
                Err(color_eyre::eyre::eyre!(
                    "Failed to get window rect for HWND {:?}",
                    hwnd
                ))
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Non-Windows builds (test harness) — return a dummy resolution.
        Ok((1920, 1080))
    }
}

#[cfg(target_os = "windows")]
fn get_monitor_resolution_for_hwnd(hwnd: HWND) -> Result<(u32, u32)> {
    use windows::Win32::{
        Foundation::{HMONITOR, RECT},
        Graphics::Gdi::{
            MonitorFromWindow, MONITOR_DEFAULTTONEAREST,
            GetMonitorInfoW, MONITORINFO,
        },
    };

    // SAFETY: `MonitorFromWindow` returns the monitor that contains the
    // largest area of the window. It's a pure query.
    let hmonitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if hmonitor.is_invalid() {
        return Err(color_eyre::eyre::eyre!(
            "MonitorFromWindow returned invalid handle for HWND {:?}",
            hwnd
        ));
    }

    // SAFETY: `GetMonitorInfoW` reads monitor dimensions from the system.
    // It's a pure query; the HMONITOR is valid because we just got it from
    // `MonitorFromWindow`.
    let mut monitor_info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        rcMonitor: RECT::default(),
        rcWork: RECT::default(),
        dwFlags: 0,
    };
    let success = unsafe { GetMonitorInfoW(hmonitor, &mut monitor_info) };
    if success.as_bool() {
        let width = (monitor_info.rcMonitor.right - monitor_info.rcMonitor.left) as u32;
        let height = (monitor_info.rcMonitor.bottom - monitor_info.rcMonitor.top) as u32;
        Ok((width, height))
    } else {
        Err(color_eyre::eyre::eyre!(
            "GetMonitorInfoW failed for HMONITOR {:?}",
            hmonitor
        ))
    }
}