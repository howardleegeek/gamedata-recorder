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
    },
    system::hardware_specs,
    util::durable_write,
};

use super::fps_logger::FpsLogger;
use super::local_recording::LocalRecording;

/// Upper bound on how many bytes of the MC mod's `game_state.jsonl` we will
/// pull into memory during `stop()`.
///
/// The Oyster Fabric mod APPENDS to that file for the WHOLE Minecraft session
/// (from world-join, not record-start), so a multi-hour session can grow it to
/// hundreds of MB. Reading the whole thing with `tokio::fs::read` during the
/// critical stop window — while OBS is still flushing/encoding — risks OOM or a
/// multi-second stall on a low-RAM machine. Above this cap we do a bounded TAIL
/// read instead: the server pipeline only needs the rows whose `timestamp_ms`
/// fall inside this recording's window, and those are at the END of the file,
/// so dropping the head of a giant file is acceptable (we LOG a warn so it's
/// honest). 50 MiB of JSONL pose rows is far more than any single recording's
/// window needs while staying trivially safe to hold in RAM.
const MAX_GAME_STATE_BYTES: u64 = 50 * 1024 * 1024;

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
            record_microphone,
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
                                "Game resolution (monitor-native for game-capture/wgc)"
                            );
                            wh
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "Failed to get monitor resolution for HWND, falling back to client rect");
                            let fallback = get_recording_base_resolution(hwnd)?;
                            tracing::info!("Game resolution (fallback client rect): {fallback:?}");
                            fallback
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    get_recording_base_resolution(hwnd)?
                }
            }
            crate::config::EffectiveCaptureMode::Monitor => {
                let wh = get_recording_base_resolution(hwnd)?;
                tracing::info!(
                    ?wh,
                    mode = ?effective_mode,
                    game_exe_stem,
                    "Game resolution (client rect for monitor/window capture)"
                );
                wh
            }
        };

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
                record_microphone,
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

    pub(crate) fn recording_location(&self) -> &std::path::Path {
        &self.recording_location
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

    /// Bounded TAIL read of a too-large `game_state.jsonl`, returning only
    /// COMPLETE JSONL lines that fall within the last `MAX_GAME_STATE_BYTES`.
    ///
    /// Invoked only when the file is already known to be larger than the cap
    /// (caller stat's it first and passes `file_len`). The tail window starts at
    /// `file_len - MAX_GAME_STATE_BYTES`, which almost certainly lands in the
    /// MIDDLE of a line, so after reading we:
    ///   1. Drop everything up to AND INCLUDING the first `\n` (the partial head
    ///      line we sliced into), and
    ///   2. Drop everything after the last `\n` (the partial trailing line the
    ///      mod may be mid-append on).
    /// What's left is a run of whole lines. If the window contains zero or one
    /// `\n` (degenerate — e.g. an absurdly long single line), there are no two
    /// newlines to bracket a complete line, so we return an empty buffer rather
    /// than a partial record.
    ///
    /// We LOG a warn that the head was dropped so the lossy collection is
    /// honest in the field. Every failure path returns `None` (warn + continue);
    /// this can never fail the recording, and contains no unwrap/panic.
    async fn read_game_state_tail(path: &std::path::Path, file_len: u64) -> Option<Vec<u8>> {
        use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

        // Safe because the caller only calls us when file_len > the cap, so the
        // subtraction can't underflow; saturating_sub keeps it panic-free even
        // if that contract ever changes (a 0 offset just reads the whole file,
        // which the trimming below still handles correctly).
        let tail_len = MAX_GAME_STATE_BYTES;
        let start = file_len.saturating_sub(tail_len);

        let mut file = match tokio::fs::File::open(path).await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    "Failed to open large MC mod game_state.jsonl for tail read \
                     (non-fatal): {e}"
                );
                return None;
            }
        };
        if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
            tracing::warn!(
                "Failed to seek into large MC mod game_state.jsonl for tail read \
                 (non-fatal): {e}"
            );
            return None;
        }

        // Read the tail window. The file may have grown between stat and read
        // (the mod is still appending), so cap the buffer at the window size we
        // intended and read until EOF or full — extra appended bytes past the
        // window are simply not read this pass, which is fine.
        let mut buf = Vec::with_capacity(tail_len as usize);
        let mut limited = (&mut file).take(tail_len);
        if let Err(e) = limited.read_to_end(&mut buf).await {
            tracing::warn!(
                "Failed to read tail of large MC mod game_state.jsonl \
                 (non-fatal): {e}"
            );
            return None;
        }

        // Drop the partial HEAD line: everything up to and including the first
        // newline (our seek almost certainly split a line). If there is no
        // newline at all, the whole window is one partial line -> nothing
        // complete to keep.
        let after_head = match buf.iter().position(|&b| b == b'\n') {
            Some(idx) => idx + 1,
            None => {
                tracing::warn!(
                    "Tail window of MC mod game_state.jsonl had no newline; \
                     collecting empty game_state (file dominated by one oversized line)"
                );
                return Some(Vec::new());
            }
        };

        // Drop the partial TRAILING line: everything after the last newline.
        // Search only within the post-head region so we never keep the head.
        let tail_region = &buf[after_head..];
        let complete_end = match tail_region.iter().rposition(|&b| b == b'\n') {
            Some(idx) => idx + 1, // relative to tail_region start
            None => {
                // Only the head line's newline existed; no further complete
                // line in the window.
                tracing::warn!(
                    "Tail window of MC mod game_state.jsonl held no complete line after \
                     dropping the partial head; collecting empty game_state"
                );
                return Some(Vec::new());
            }
        };

        let complete = tail_region[..complete_end].to_vec();
        tracing::warn!(
            "MC mod game_state.jsonl is {} bytes (> {} cap); collected only the trailing \
             {} bytes of complete lines and DROPPED the head. Server trims by timestamp_ms \
             so this is expected for very long sessions, but the early part of game_state \
             is not in this session.",
            file_len,
            MAX_GAME_STATE_BYTES,
            complete.len()
        );
        Some(complete)
    }

    pub(crate) async fn stop(
        self,
        recorder: &mut dyn VideoRecorder,
        adapter_infos: &[wgpu::AdapterInfo],
        input_capture: &InputCapture,
    ) -> Result<()> {
        let window_name = self.get_window_name();
        let mut result = recorder.stop_recording().await;

        // Snapshot the dropped-event count BEFORE consuming the writer. The
        // writer's `stop()` takes `self` by value, so once it errors we can no
        // longer read its counter — but `input_stream` (not consumed by stop)
        // shares the SAME `Arc<AtomicU64>` (see InputEventStream::clone_shared),
        // so this snapshot reflects the writer's real accumulated drop count.
        // Used as the fallback below so a stop error doesn't zero out the count
        // that feeds the INVALID-reason diagnostics in metadata.
        let dropped_snapshot = self
            .input_stream
            .dropped_counter()
            .map(|c| c.get())
            .unwrap_or(0);

        // Don't propagate input_writer errors — treat like recorder errors
        // (write INVALID marker instead of returning Err which skips metadata)
        let dropped_input_events = match self.input_writer.stop(input_capture).await {
            Ok(count) => count,
            Err(e) => {
                tracing::error!("Failed to stop input writer: {e}");
                if result.is_ok() {
                    result = Err(e);
                }
                // The consuming `stop()` failed, so its return count is gone;
                // fall back to the pre-stop snapshot (same shared counter) so we
                // preserve the real drop count for diagnostics instead of 0.
                dropped_snapshot
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

        // Collect the MC mod's game_state.jsonl into this session (ISC-DATA-GS).
        //
        // The Oyster Fabric mod (world.oyster.recorder.OysterRecorderMod) streams
        // per-tick player pose/orientation/velocity/state to a FIXED legacy path it
        // hard-codes in SessionDir.java:
        //     ~/Documents/OysterClips/active_session/game_state.jsonl
        // That convention predates the Rust recorder (it was the old Python
        // OysterRecorder.exe's session dir), so the data is produced correctly but
        // never lands in THIS recorder's session dir. Copy it in now so game_state
        // ships with the rest of the session. Best-effort: the mod writes the whole
        // MC session (from world-join, not record-start), so the server pipeline
        // trims to this recording's [wall_clock_start, wall_clock_end] window by
        // each row's `timestamp_ms`. Absence is non-fatal (mod off / not installed).
        if let Some(home) = dirs::home_dir() {
            let mod_game_state = home
                .join("Documents")
                .join("OysterClips")
                .join("active_session")
                .join("game_state.jsonl");
            if mod_game_state.exists() {
                let dest = self
                    .recording_location
                    .join(constants::filename::recording::GAME_STATE);
                // The Fabric mod APPENDS to this file concurrently, so a plain
                // `tokio::fs::copy` can capture a partial final JSONL line (a
                // torn read between the mod's `write` and its newline) or hit
                // ERROR_SHARING_VIOLATION mid-flight. Instead we read the bytes
                // and trim to whole lines before the atomic write.
                //
                // But the file can be hundreds of MB for a multi-hour MC session
                // (the mod appends from world-join, not record-start), and a full
                // `tokio::fs::read` would pull all of that into RAM during the
                // critical stop window — alongside OBS encoding — risking OOM /
                // a multi-second stall on a low-RAM machine. So: stat the file
                // first, and only above MAX_GAME_STATE_BYTES fall back to a
                // BOUNDED TAIL read (the server trims by `timestamp_ms` and only
                // needs the tail, so dropping the head of a giant file is
                // acceptable — we warn so it's honest). Under the cap we keep the
                // existing full-read + trailing-trim path. Every error path here
                // stays non-fatal (warn + continue); this step must NEVER fail
                // the recording, and there is no unwrap/panic.
                let collected = match tokio::fs::metadata(&mod_game_state).await {
                    Ok(meta) if meta.len() > MAX_GAME_STATE_BYTES => {
                        Self::read_game_state_tail(&mod_game_state, meta.len()).await
                    }
                    Ok(_) => {
                        // Under the cap: read the whole file and drop only the
                        // (possibly torn) trailing partial line.
                        match tokio::fs::read(&mod_game_state).await {
                            Ok(bytes) => {
                                // Keep only up to and including the last newline
                                // so a half-written final line is discarded. With
                                // no newline at all the file holds no complete
                                // line yet, so we collect an empty file rather
                                // than a partial record.
                                let complete_len = match bytes.iter().rposition(|&b| b == b'\n') {
                                    Some(idx) => idx + 1,
                                    None => 0,
                                };
                                let dropped = bytes.len() - complete_len;
                                if dropped > 0 {
                                    tracing::debug!(
                                        "Dropping {dropped} trailing byte(s) of partial final \
                                         line from game_state.jsonl before collecting \
                                         (concurrent mod append)"
                                    );
                                }
                                Some(bytes[..complete_len].to_vec())
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to read MC mod game_state.jsonl for collection \
                                     (non-fatal): {e}"
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to stat MC mod game_state.jsonl for collection \
                             (non-fatal): {e}"
                        );
                        None
                    }
                };

                if let Some(complete) = collected {
                    let n = complete.len();
                    // Atomic write (write-tmp → fsync → rename), same crash-safe
                    // pattern as the INVALID marker above. Errors are logged and
                    // swallowed — collecting game_state must never fail the recording.
                    match durable_write::write_atomic_async(dest.clone(), complete).await {
                        Ok(()) => tracing::info!(
                            "Collected MC mod game_state.jsonl ({n} complete bytes) into \
                             session {}",
                            self.recording_location.display()
                        ),
                        Err(e) => tracing::warn!(
                            "Failed to write collected game_state.jsonl into session \
                             (non-fatal): {e}"
                        ),
                    }
                }
            } else {
                tracing::warn!(
                    "MC mod game_state.jsonl not found at {} — session will lack game_state \
                     (mod not running, or wrote elsewhere)",
                    mod_game_state.display()
                );
            }
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
            dropped_input_events,
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

/// Physical-pixel resolution of the monitor under `hwnd`, falling back to
/// the primary monitor when `MonitorFromWindow` fails. Used by the
/// game-capture path (CaptureMode::GameHook) where the tiny boot-window
/// client rect would be the wrong thing to pin OBS base resolution to —
/// we want the native monitor resolution so the hook draws into a
/// correctly-sized surface.
#[cfg(target_os = "windows")]
pub fn get_monitor_resolution_for_hwnd(hwnd: HWND) -> Result<(u32, u32)> {
    use windows::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        },
    };

    // SAFETY: MonitorFromWindow + GetMonitorInfoW are pure read-only Win32
    // queries. MONITOR_DEFAULTTONEAREST guarantees a non-null HMONITOR even
    // when `hwnd` sits outside any display. We pass an owned MONITORINFO
    // struct with `cbSize` set, as required by the documented contract.
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if hmon.is_invalid() {
            tracing::warn!(
                "MonitorFromWindow returned NULL, falling back to primary monitor resolution"
            );
            return hardware_specs::get_primary_monitor_resolution()
                .context("Failed to get primary monitor resolution");
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT::default(),
            rcWork: RECT::default(),
            dwFlags: 0,
        };
        GetMonitorInfoW(hmon, &mut info)
            .ok()
            .context("GetMonitorInfoW failed for window's monitor")?;
        let w = (info.rcMonitor.right - info.rcMonitor.left) as u32;
        let h = (info.rcMonitor.bottom - info.rcMonitor.top) as u32;
        if w == 0 || h == 0 {
            tracing::warn!(
                w,
                h,
                "GetMonitorInfoW returned zero-sized rect, falling back to primary"
            );
            return hardware_specs::get_primary_monitor_resolution()
                .context("Failed to get primary monitor resolution");
        }
        Ok((w, h))
    }
}
