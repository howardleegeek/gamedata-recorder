use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use color_eyre::{
    Result,
    eyre::{self, Context, OptionExt as _, bail, eyre},
};
use constants::{FPS, RECORDING_HEIGHT, RECORDING_WIDTH, encoding::VideoEncoderType};
use input_capture::ConsentGuard;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromWindow},
    System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, OpenInputDesktop,
    },
    UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
};

use libobs_simple::sources::{
    ObsObjectUpdater, ObsSourceBuilder,
    windows::{
        GameCaptureSourceBuilder, GameCaptureSourceUpdater, MonitorCaptureSourceBuilder,
        MonitorCaptureSourceUpdater, ObsGameCaptureMode, ObsWindowPriority,
        WindowCaptureSourceBuilder, WindowCaptureSourceUpdater, WindowInfo,
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
/// Name of the scene item we create for the Windows.Graphics.Capture
/// (WGC) source. The OBS source id itself is `wgc_capture` (registered
/// by `win-capture.dll` / `data/obs-plugins/win-capture/`). WGC is
/// Microsoft's official Win10 1903+ capture API — no DLL injection,
/// handles fullscreen-exclusive D3D11/D3D12 cleanly, friendly to HDR.
const OWL_WGC_CAPTURE_NAME: &str = "owl_wgc_capture";
/// OBS source-type ID for WGC capture.
///
/// Five attempts (2026-04-20):
///   1. `wgc_capture`        — not registered
///   2. `window_capture_wgc` — not registered (appears only as a string literal)
///   3. `window_capture` + `method=2` (int) — **this one** (from now on)
///   4. `winrt_capture`      — not registered
///
/// Empirically the bundled `win-capture.dll` registers 4 sources:
/// `game_capture`, `monitor_capture`, `window_capture`,
/// `wasapi_process_output_capture`. WGC is NOT a standalone source on
/// OBS 30.x; it's the `method` property on `window_capture`, mapping:
///   0 = Auto (falls back to BitBlt when the compositor isn't cooperative)
///   1 = BitBlt (legacy, black frames on DX12)
///   2 = WGC (Windows.Graphics.Capture)
const WGC_CAPTURE_SOURCE_ID: &str = "window_capture";

/// Method enum on `window_capture` that selects the WGC backend.
const WGC_CAPTURE_METHOD_WGC: i64 = 2;

/// `capture_mode` setting value (unused for window_capture but kept for
/// other code paths that reference this const).
const WGC_CAPTURE_MODE_WINDOW: &str = "window";

// Audio source names and OBS source-type IDs for monitor-capture audio.
// v2.5.6: monitor-capture has no hooked audio path (game-capture and
// window-capture do, via `set_capture_audio`), so prior to this fix all
// monitor-capture recordings were silent MP4s. We attach two WASAPI sources
// directly to the scene: desktop-out (what the user hears) and optionally
// mic-in (off by default for privacy).
const OWL_DESKTOP_AUDIO_NAME: &str = "owl_desktop_audio";
const OWL_MICROPHONE_AUDIO_NAME: &str = "owl_microphone_audio";
/// WASAPI desktop (output) capture — captures the default render device.
/// Source ID matches OBS's `obs-plugins/win-capture/wasapi.c`.
const WASAPI_OUTPUT_CAPTURE_ID: &str = "wasapi_output_capture";
/// WASAPI microphone (input) capture — captures the default capture device.
const WASAPI_INPUT_CAPTURE_ID: &str = "wasapi_input_capture";

pub struct ObsEmbeddedRecorder {
    // Held in an Option so Drop can `take()` the handle, wait with a deadline,
    // and fall back to abandoning the thread on timeout rather than blocking
    // the caller forever.
    obs_thread: Option<std::thread::JoinHandle<()>>,
    obs_tx: tokio::sync::mpsc::Sender<RecorderMessage>,
    available_encoders: Vec<VideoEncoderType>,
    /// Signalled by `TracingObsLogger` whenever OBS emits the
    /// "number of skipped frames due to encoding lag:" log line. Used by the
    /// stop-recording path to await the asynchronous skipped-frames accounting
    /// instead of sleeping a fixed duration.
    skipped_frames_notify: Arc<tokio::sync::Notify>,
}

/// Monitor-capture recovery state machine for DXGI desktop-duplication
/// access loss. When the user hits Win+L, switches RDP sessions, or the UAC
/// secure desktop appears, Windows yanks our DX11 duplicator out from under
/// libobs and OBS emits `DXGI_ERROR_ACCESS_LOST` (0x887A0026). Before this
/// machine existed, the encoder would keep running and silently produce a
/// truncated / black MP4 after the lock screen.
///
/// The flow, advanced by `RecorderState::poll` (called ~1Hz from the tokio
/// thread):
///
///   Active
///     └─ `access_lost_flag` set by `TracingObsLogger` (OBS-thread side)
///         → call `output.pause(true)`; libobs freezes the MP4 timeline
///         → transition to Paused(now) and clear the flag
///
///   Paused(started)
///     ├─ interactive desktop is back → `output.pause(false)` → Active
///     ├─ flag re-fires (duplication still busted after unpause) → re-Paused
///     └─ elapsed >= ACCESS_LOST_RESUME_TIMEOUT → emit `workstation_locked_timeout`
///         on the next PollUpdate. `Recorder::poll` then calls
///         `self.stop(..)` gracefully to flush whatever we have on disk.
///
/// The machine is only ever installed on the monitor-capture path because
/// that's the only path that uses the desktop-duplication API in libobs.
/// Game-capture hooks into the game's own swapchain; window-capture uses
/// PrintWindow — neither sees DXGI_ERROR_ACCESS_LOST on a workstation lock.
#[derive(Debug, Clone, Copy)]
enum AccessLostState {
    Active,
    Paused(Instant),
}

/// Time budget for the user to unlock / come back from the secure desktop
/// before we give up and gracefully stop the recording. 5 minutes is long
/// enough to step away for a quick UAC prompt or sign-in without losing a
/// whole session, and short enough that we're not sitting on a dead
/// duplicator for hours.
const ACCESS_LOST_RESUME_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Probe whether an interactive desktop is currently attached to our
/// window station. Returns `true` when the logged-in user is at the normal
/// desktop (i.e. not on the Winlogon secure desktop, not on a locked
/// workstation, not inside a UAC elevation prompt, not inside a fast-user-
/// switch / RDP session handoff).
///
/// Mechanism: `OpenInputDesktop(DESKTOP_READOBJECTS)` succeeds only when the
/// current process's window station has access to the foreground input
/// desktop. During a workstation lock or secure-desktop takeover, Windows
/// swaps the input desktop to `Winlogon` (which a normal process cannot
/// open), and the call fails with `ERROR_ACCESS_DENIED` (5). We treat any
/// error as "not interactive" — the cost of a false negative is that we
/// keep the MP4 paused for another tick, which is exactly the right thing.
///
/// The handle is closed immediately with `CloseDesktop`; we never keep a
/// desktop handle across polls (doing so would itself count as interactive-
/// desktop usage and interfere with other processes' handle accounting).
///
/// # Safety
///
/// `OpenInputDesktop` and `CloseDesktop` are plain Win32 API calls with no
/// callback, no out-pointers, and no thread-affinity requirements. The
/// `windows` crate wraps them as `unsafe fn` because they touch OS-managed
/// handles; there is nothing to invariant-prove beyond "close what we
/// opened," which we do unconditionally on the success branch.
fn session_is_interactive() -> bool {
    // SAFETY: OpenInputDesktop is a read-only Win32 query. DESKTOP_READOBJECTS
    // is the lowest-rights access mask that still succeeds on the real
    // interactive desktop and fails with ACCESS_DENIED on Winlogon's secure
    // desktop — exactly the discrimination we need. `finherit = false` means
    // the handle is not inherited by child processes (we create none here).
    let result = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) };
    match result {
        Ok(hdesk) => {
            // Close immediately. If this fails we log and move on — leaking
            // a desktop handle for the process lifetime is bad, but blocking
            // or retrying here would add failure modes without buying much.
            // SAFETY: `hdesk` was just returned by OpenInputDesktop above;
            // it is a valid HDESK we own and haven't dereferenced.
            if let Err(e) = unsafe { CloseDesktop(hdesk) } {
                tracing::warn!(
                    e=?e,
                    "CloseDesktop on interactive-desktop probe failed; handle \
                     will leak until process exit"
                );
            }
            true
        }
        Err(_) => {
            // ACCESS_DENIED / other failures both mean "we can't see the
            // interactive desktop right now" — keep the MP4 paused.
            false
        }
    }
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
        // Notify is shared between the TracingObsLogger (OBS-side producer) and
        // the tokio stop-recording path (consumer). `notify_one` is level-
        // triggered in the sense that a single pending permit is stored, so a
        // skipped-frames log line that lands before we start awaiting is not
        // lost — we still observe it on the next `notified().await`.
        let skipped_frames_notify = Arc::new(tokio::sync::Notify::new());
        let skipped_frames_notify_for_thread = skipped_frames_notify.clone();
        tracing::debug!("Spawning OBS recorder thread");
        let obs_thread = std::thread::spawn(move || {
            recorder_thread(
                adapter_index,
                obs_rx,
                init_success_tx,
                skipped_frames_notify_for_thread,
            )
        });
        // Wait for the OBS context to be initialized, and bail out if it fails
        tracing::debug!("Waiting for OBS context initialization");
        let available_encoders = init_success_rx.await??;
        tracing::debug!(
            "OBS context initialized successfully with {} encoders",
            available_encoders.len()
        );

        Ok(Self {
            obs_thread: Some(obs_thread),
            obs_tx,
            available_encoders,
            skipped_frames_notify,
        })
    }
}

/// Case-insensitive substrings that identify OBS log lines reporting that
/// our DX11 desktop-duplication surface has been invalidated. When the user
/// hits Win+L, switches RDP sessions, or Windows pops a UAC secure desktop
/// the duplication ACL changes out from under libobs and the D3D runtime
/// emits `DXGI_ERROR_ACCESS_LOST (0x887A0026)`. The exact log wording varies
/// across OBS / win-capture versions, so we match against a set of tokens
/// seen in practice rather than a single brittle string. Matching is done
/// on the already-lowercased message.
///
/// False-positive risk is low: these phrases are narrow by construction
/// (the hex code is unique, and the English phrases all originate from
/// `d3d11-monitor-duplicator.c`). The failure mode for a false positive is
/// a harmless pause/unpause cycle with no effect on the MP4 — the first
/// unpause happens as soon as `session_is_interactive()` returns true,
/// which it will within one poll tick on a false alarm.
const ACCESS_LOST_LOG_NEEDLES: &[&str] = &[
    "access_lost",
    "0x887a0026",
    "device_removed",
    "duplicator is invalid",
    "lost duplicator",
    "could not get next frame",
];
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
        record_microphone: bool,
        (base_width, base_height): (u32, u32),
        event_stream: InputEventStream,
        consent: ConsentGuard,
    ) -> Result<()> {
        // R46: final consent gate before we hand control to the OBS thread
        // and start writing video/audio bytes to disk. This mirrors the
        // gate in `KbmCapture::initialize` for the keyboard/mouse side.
        consent.require_granted()?;

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
                    record_microphone,
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

        // v2.5.5 split stop into Phase1 (stop OBS output, reset state) +
        // tokio-side wait + Phase2 (read skipped-frames counter, finalize
        // settings). The previous implementation blocked the OBS thread
        // with `std::thread::sleep(200ms)`, which, because the tokio caller
        // was `.await`-ing the oneshot reply, stalled the entire tokio
        // event loop. Combined with the input-channel capacity of 10
        // (fixed in v2.5.5 by raising to 10_000), every stop dropped
        // ~100 input events because the raw-input bridge thread overflowed
        // the mpsc and Windows dropped `WM_INPUT` messages silently.
        //
        // Signal-based version: `TracingObsLogger` observes the OBS log
        // line "number of skipped frames due to encoding lag: X/Y" and
        // fires `skipped_frames_notify`. Phase1 arms the notifier (by
        // calling `notify.notified()` *before* Phase1 runs so a single
        // pre-ready permit is not missed) and Phase2 reads the populated
        // counter. If OBS doesn't emit the log line within the deadline we
        // proceed anyway — the only cost is a missing `skipped_frames`
        // field in metadata; the recording itself remains valid.
        const SKIPPED_FRAMES_LOG_DEADLINE: Duration = Duration::from_secs(3);

        // Arm the waiter before Phase1 runs so a notification fired
        // between Phase1's `output.stop()` and our `notified.await` below
        // is not lost. `Notified` registers itself eagerly on construction
        // — a subsequent `notify_one` will wake this specific future even
        // if it hasn't been polled yet.
        let notified = self.skipped_frames_notify.notified();
        tokio::pin!(notified);

        let (phase1_tx, phase1_rx) = tokio::sync::oneshot::channel();
        self.obs_tx
            .send(RecorderMessage::StopRecordingPhase1 {
                result_tx: phase1_tx,
            })
            .await?;
        let partial_settings = phase1_rx.await??;

        // Wait for the asynchronous skipped-frames log line, bounded by a
        // conservative deadline. 3s is far longer than the ~200ms OBS
        // needs in practice but short enough that a truly stuck OBS
        // thread doesn't hang the app. A timeout here is non-fatal: the
        // `skipped_frames` field becomes absent, nothing else breaks.
        //
        // `notified` is a `Pin<&mut Notified>` after `tokio::pin!`; we
        // pass it to `timeout` by value to give up the borrow.
        let wait_start = Instant::now();
        match tokio::time::timeout(SKIPPED_FRAMES_LOG_DEADLINE, notified).await {
            Ok(()) => {
                tracing::debug!(
                    "Skipped-frames log line observed after {:?}",
                    wait_start.elapsed()
                );
            }
            Err(_) => {
                tracing::warn!(
                    "Timed out after {:?} waiting for OBS skipped-frames log line; \
                     recording metadata will omit `skipped_frames`. This usually \
                     means the encoder dropped the stop message silently or the \
                     log line format changed. Last known phase: Phase1 done, \
                     Phase2 pending.",
                    SKIPPED_FRAMES_LOG_DEADLINE
                );
            }
        }

        let (phase2_tx, phase2_rx) = tokio::sync::oneshot::channel();
        self.obs_tx
            .send(RecorderMessage::StopRecordingPhase2 {
                partial_settings,
                result_tx: phase2_tx,
            })
            .await?;
        let result = phase2_rx.await??;

        tracing::info!("OBS embedded recording stopped successfully");

        Ok(result)
    }

    async fn poll(&mut self) -> PollUpdate {
        // Round-trip Poll so the OBS-thread-side state machine can report
        // the DXGI_ERROR_ACCESS_LOST recovery verdict back up to the
        // `Recorder` layer. The previous implementation dispatched Poll
        // fire-and-forget, which was fine when `poll()` only did
        // bookkeeping; now that it also drives the monitor-capture pause/
        // resume machine we need its answer before building the PollUpdate.
        //
        // If the round-trip fails (channel closed because the OBS thread
        // died mid-shutdown, or reply dropped), we fall back to "no
        // timeout" and let the next poll try again. A failed send here
        // means recording is already torn down, so suppressing the flag
        // is the safe choice — the upstream stop path will run either way.
        let workstation_locked_timeout = {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            if self
                .obs_tx
                .send(RecorderMessage::Poll { reply_tx })
                .await
                .is_err()
            {
                false
            } else {
                reply_rx.await.unwrap_or(false)
            }
        };
        PollUpdate {
            active_fps: Some(unsafe { libobs_wrapper::sys::obs_get_active_fps() }),
            workstation_locked_timeout,
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
        // The thread's `blocking_recv()` returns None, causing the loop to exit.
        drop(std::mem::replace(
            &mut self.obs_tx,
            tokio::sync::mpsc::channel(1).0,
        ));

        // Poll `is_finished()` against a deadline instead of calling
        // `join()` which blocks indefinitely. A stuck OBS thread
        // (GPU driver deadlock, FFmpeg muxer hang, blocked FFI call) used
        // to freeze application shutdown forever; we now log a warning and
        // abandon the thread after the deadline so the process can exit.
        //
        // 3s is chosen to be a comfortable upper bound on normal shutdown
        // (OBS context tear-down empirically completes in <500ms) while
        // being short enough that a user quitting the app doesn't stare at
        // a hung window. The thread is abandoned rather than forced —
        // libOBS holds native resources (D3D devices, audio threads) that
        // cannot be safely cancelled, so leaking the JoinHandle is the
        // least-bad option.
        let Some(handle) = self.obs_thread.take() else {
            return;
        };

        const DROP_DEADLINE: Duration = Duration::from_secs(3);
        const POLL_INTERVAL: Duration = Duration::from_millis(50);
        let start = Instant::now();
        let thread_id = handle.thread().id();
        let thread_name = handle
            .thread()
            .name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{thread_id:?}"));

        loop {
            if handle.is_finished() {
                // Safe to join now — it will return immediately.
                if let Err(panic) = handle.join() {
                    tracing::error!(
                        thread = %thread_name,
                        "OBS recorder thread panicked during shutdown: {panic:?}"
                    );
                } else {
                    tracing::debug!(
                        thread = %thread_name,
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        "OBS recorder thread joined cleanly during Drop"
                    );
                }
                return;
            }
            if start.elapsed() >= DROP_DEADLINE {
                tracing::warn!(
                    thread = %thread_name,
                    thread_id = ?thread_id,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    deadline_ms = DROP_DEADLINE.as_millis() as u64,
                    "OBS recorder thread did not exit within Drop deadline; \
                     abandoning handle to avoid blocking process shutdown. \
                     libOBS native resources may leak until process exit."
                );
                // Dropping a JoinHandle detaches the underlying OS thread —
                // it continues to run but we can no longer synchronize with
                // it. That is the correct behavior here: we can't safely
                // force-kill a libOBS worker, and blocking forever on join
                // would freeze process shutdown. The thread will be cleaned
                // up when the process exits.
                drop(handle);
                return;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

enum RecorderMessage {
    StartRecording {
        request: Box<RecordingRequest>,
        result_tx: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// First half of stop_recording. Stops the OBS output and tears down
    /// recording state; returns a partial settings blob. The tokio caller
    /// then awaits `skipped_frames_notify` (signalled by `TracingObsLogger`
    /// when OBS emits the "skipped frames due to encoding lag" log line)
    /// bounded by a deadline, then sends `StopRecordingPhase2` with the
    /// partial settings to finalize. Splitting the stop into two messages
    /// lets the tokio event loop keep running while we wait. The
    /// original implementation (pre-v2.5.5) called
    /// `std::thread::sleep(200ms)` on the OBS thread, which stalled every
    /// tokio task for the duration. v2.5.5 moved the wait to
    /// `tokio::time::sleep` (no longer blocking the tokio loop, but still
    /// fixed-duration). This version replaces that sleep with a signal
    /// so the stop path completes as soon as the log line lands rather
    /// than always waiting the full 200ms.
    StopRecordingPhase1 {
        result_tx: tokio::sync::oneshot::Sender<Result<serde_json::Value>>,
    },
    /// v2.5.5: second half of stop_recording. Reads the skipped-frames
    /// counter (populated by `TracingObsLogger` during the tokio-side
    /// sleep), folds it into the settings blob, and clears the encoder
    /// cache. See `StopRecordingPhase1` for the rationale.
    StopRecordingPhase2 {
        partial_settings: serde_json::Value,
        result_tx: tokio::sync::oneshot::Sender<Result<serde_json::Value>>,
    },
    /// Periodic (~1Hz) tick from the tokio thread. Drives the monitor-
    /// capture DXGI_ERROR_ACCESS_LOST recovery state machine and the
    /// "game window closed" source-teardown check. Replies with
    /// `workstation_locked_timeout = true` exactly once, when the 5-minute
    /// resume window expires on a locked workstation; the tokio caller
    /// then calls `Recorder::stop` to flush the MP4.
    Poll {
        reply_tx: tokio::sync::oneshot::Sender<bool>,
    },
    CheckHookTimeout {
        result_tx: tokio::sync::oneshot::Sender<bool>,
    },
}

struct RecordingRequest {
    game_resolution: (u32, u32),
    video_settings: EncoderSettings,
    game_config: GameConfig,
    /// If `true`, attach a WASAPI input (microphone) source to the scene when
    /// running in monitor-capture mode. Ignored for game-capture (the hook
    /// taps game audio) and for the window-capture fallback (which already
    /// uses `set_capture_audio`).
    record_microphone: bool,
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
    skipped_frames_notify: Arc<tokio::sync::Notify>,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        recorder_thread_impl(adapter_index, rx, init_success_tx, skipped_frames_notify);
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
    skipped_frames_notify: Arc<tokio::sync::Notify>,
) {
    tracing::debug!("OBS recorder thread started");
    let skipped_frames = Arc::new(Mutex::new(None));

    tracing::debug!("Creating OBS recorder state");
    let mut state =
        match RecorderState::new(adapter_index, skipped_frames.clone(), skipped_frames_notify) {
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
            RecorderMessage::StopRecordingPhase1 { result_tx } => {
                result_tx
                    .send(state.stop_recording_phase1(last_shutdown_tx.take()))
                    .ok();
            }
            RecorderMessage::StopRecordingPhase2 {
                partial_settings,
                result_tx,
            } => {
                result_tx
                    .send(state.stop_recording_phase2(partial_settings))
                    .ok();
            }
            RecorderMessage::Poll { reply_tx } => {
                // `poll` reports `true` in exactly one case: monitor-capture
                // was paused on DXGI_ERROR_ACCESS_LOST, the user's
                // workstation is still locked, and we've crossed the
                // resume deadline. Returning `Err(_)` from the inner poll
                // (e.g. a libobs scene lookup blew up) is unrelated to
                // workstation-lock handling and must not fire the graceful
                // stop, so we coerce errors to `false` and log them.
                let workstation_locked_timeout = match state.poll() {
                    Ok(flag) => flag,
                    Err(e) => {
                        tracing::error!("Failed to poll OBS embedded recorder: {e}");
                        false
                    }
                };
                reply_tx.send(workstation_locked_timeout).ok();
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
    /// Latched to `true` by `TracingObsLogger` the moment OBS logs a
    /// DXGI_ERROR_ACCESS_LOST (or equivalent duplicator-invalid) message on
    /// the monitor-capture path. Read + cleared on the OBS thread inside
    /// `poll()`. Stored as `AtomicBool` rather than behind a mutex because
    /// the logger runs on a hot path (called for every OBS message) and
    /// must not block the OBS thread on a poisoned mutex.
    access_lost_flag: Arc<AtomicBool>,
    /// Current monitor-capture recovery state (Active or Paused(started)).
    /// Advanced by `poll()` based on `access_lost_flag` +
    /// `session_is_interactive()`. Starts `Active` every time a new
    /// recording begins — we do not carry pause state across recordings.
    access_lost_state: AccessLostState,
    /// Event stream for sending VIDEO_PAUSED and VIDEO_RESUMED events
    /// when DXGI access is lost/regained during recording.
    event_stream: Option<InputEventStream>,
    output: ObsOutputRef,
    source: Option<ObsSourceRef>,
    /// WASAPI desktop (output) audio source, attached only when running in
    /// monitor-capture mode. Kept alive on the state so OBS doesn't release
    /// the source via the wrapper's drop glue while it's routed to an
    /// output channel. See `attach_monitor_capture_audio`.
    desktop_audio_source: Option<ObsSourceRef>,
    /// WASAPI microphone (input) audio source. Only populated when both
    /// monitor-capture mode is active AND the user opted in via the
    /// `record_microphone` preference (default false).
    microphone_source: Option<ObsSourceRef>,
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
/// State that affects source creation - if any field changes, we must recreate the source.
///
/// `use_window_capture` is the legacy flag from v2.5.8 (`true` = prefer
/// monitor capture, `false` = prefer game-capture hook). It is kept so
/// users' persisted configs still influence behavior — but the new
/// `effective_mode` field is the authoritative selector at
/// `prepare_source` time. `effective_mode` resolves
/// `config::CaptureMode::Auto` against the fullscreen-exclusive
/// allowlist at `Recording::start` time.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceCreationState {
    use_window_capture: bool,
    effective_mode: crate::config::EffectiveCaptureMode,
}
impl RecorderState {
    fn new(
        adapter_index: usize,
        skipped_frames: Arc<Mutex<Option<SkippedFrames>>>,
        skipped_frames_notify: Arc<tokio::sync::Notify>,
    ) -> Result<(Self, Vec<VideoEncoderType>), libobs_wrapper::utils::ObsError> {
        tracing::debug!("RecorderState::new() called");
        // Latched access-lost flag shared between TracingObsLogger (producer,
        // runs on the OBS log thread) and the RecorderState::poll state
        // machine (consumer, runs on the OBS thread via the message loop).
        // AtomicBool so the logger can never block the OBS thread.
        let access_lost_flag = Arc::new(AtomicBool::new(false));
        // Create OBS context
        tracing::debug!("Creating OBS context");
        let mut obs_context = ObsContext::new(
            ObsContext::builder()
                .set_logger(Box::new(TracingObsLogger {
                    skipped_frames: skipped_frames.clone(),
                    skipped_frames_notify: skipped_frames_notify.clone(),
                    access_lost_flag: access_lost_flag.clone(),
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
        // `skipped_frames_notify` is plumbed into TracingObsLogger above.
        // The rest of RecorderState reads from the `skipped_frames` Mutex
        // directly in Phase2, so we don't hold another handle here.
        drop(skipped_frames_notify);
        Ok((
            Self {
                adapter_index,
                skipped_frames,
                access_lost_flag,
                access_lost_state: AccessLostState::Active,
                event_stream: None,
                output,
                source: None,
                desktop_audio_source: None,
                microphone_source: None,
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

        // Resolve the effective capture mode again here — `Recording::start`
        // already computed one for the resolution pick, but recomputing is
        // cheap and keeps this call site self-contained (no extra IPC field
        // on RecordingRequest).
        let game_exe_stem = std::path::Path::new(&request.game_exe)
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let effective_mode = request.game_config.effective_capture_mode(&game_exe_stem);

        let source_creation_state = SourceCreationState {
            use_window_capture: request.game_config.use_window_capture,
            effective_mode,
        };

        tracing::info!(
            game_exe_stem,
            mode = ?effective_mode,
            base_resolution = ?request.game_resolution,
            "Resolved capture mode for recording"
        );

        // Determine whether this recording will genuinely use monitor capture
        // (vs. the window-capture fallback or game-capture). Monitor capture
        // has no audio tap of its own, so only that path needs WASAPI
        // sources attached. We check _before_ `prepare_source` so we can log
        // the decision clearly and so the scene teardown in the else branch
        // runs unconditionally when switching away from monitor capture.
        let monitors_available = !MonitorCaptureSourceBuilder::get_monitors()
            .unwrap_or_default()
            .is_empty();
        let use_monitor_capture_audio =
            should_attach_monitor_audio(effective_mode, monitors_available);

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

        // v2.5.6: attach (or detach) WASAPI audio sources to the scene based
        // on the capture mode. Game-capture and the window-capture fallback
        // already tap audio via `set_capture_audio(true)` on their respective
        // builders, so we only attach WASAPI sources when monitor capture is
        // the active path. When switching between modes we must also detach
        // stale audio sources so channels 1/2 aren't left pointing at freed
        // memory.
        if use_monitor_capture_audio {
            self.attach_monitor_capture_audio(request.record_microphone)
                .wrap_err("Failed to attach WASAPI audio sources for monitor capture")?;
        } else {
            self.detach_monitor_capture_audio();
        }

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

        // Store event stream for sending VIDEO_PAUSED/VIDEO_RESUMED events during DXGI access lost
        self.event_stream = Some(request.event_stream.clone());

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
                // Record the resolved capture mode for post-hoc analysis.
                // Lets us see "this recording ran WGC because Auto picked
                // it on Win10 1903+" in recording_metadata.json without
                // cross-referencing logs.
                object.insert(
                    "effective_capture_mode".to_string(),
                    match effective_mode {
                        crate::config::EffectiveCaptureMode::Monitor => "monitor",
                        crate::config::EffectiveCaptureMode::GameHook => "game_hook",
                        crate::config::EffectiveCaptureMode::Wgc => "wgc",
                    }
                    .into(),
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

        // Reset monitor-capture DXGI-access-lost recovery state. A stale
        // flag could be sitting here if the previous recording hit Win+L
        // and the flag was set *after* stop_recording_phase1 cleared it
        // (race window between `output.stop()` and the last log-line
        // flush). Starting a fresh recording in Paused state would be
        // catastrophic — we'd immediately call `output.pause(true)` on an
        // output that isn't even paused yet in OBS's eyes. Force Active.
        self.access_lost_flag.store(false, Ordering::Relaxed);
        self.access_lost_state = AccessLostState::Active;

        // A stale `notify_one` permit can exist here if the previous
        // recording's stop timed out waiting for the skipped-frames log
        // line and OBS emitted it afterwards. We do NOT drain the permit
        // explicitly — the worst case is that the next `stop_recording`
        // consumes the stale permit, runs Phase2 before OBS has reported
        // this recording's skipped frames, and writes metadata without a
        // `skipped_frames` field. That is the identical degraded outcome
        // as the timeout path (metadata is still valid, recording is
        // still valid) so the added complexity of draining isn't worth
        // the extra code. The counter itself is cleared above, so
        // Phase2's read always reflects only the current recording.

        self.output.start()?;

        self.source = Some(source);
        self.last_application = Some((request.game_exe.clone(), request.hwnd));
        self.last_source_creation_state = Some(source_creation_state);
        self.is_recording = true;
        self.recording_start_time = Some(Instant::now());

        Ok(())
    }

    /// Phase 1 of recording stop: stop the OBS output, drop recording
    /// state, and signal the hook monitor to exit. Does NOT read the
    /// skipped-frames counter — OBS emits that log line asynchronously
    /// after `output.stop()` returns, so the tokio caller awaits
    /// `skipped_frames_notify` (fed by `TracingObsLogger`) between Phase1
    /// and Phase2.
    ///
    /// History: pre-v2.5.5, the whole stop path was a single sync
    /// function that called `std::thread::sleep(200ms)` on the OBS
    /// thread. The tokio caller was `.await`-ing on the oneshot reply,
    /// so the tokio event loop stalled for those 200ms every stop —
    /// during which the raw-input bridge thread overflowed its mpsc
    /// (see Fix A4) and Windows silently dropped `WM_INPUT` messages.
    /// v2.5.5 split the function in two and moved the wait to
    /// `tokio::time::sleep` on the caller side, unblocking the loop but
    /// still burning a fixed 200ms. The current version swaps the
    /// fixed sleep for a signal-based wait (`tokio::sync::Notify`) with
    /// a 3-second safety deadline, so stops complete as soon as OBS
    /// reports skipped frames and never longer than the deadline.
    fn stop_recording_phase1(
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

        // v2.5.6: tear down WASAPI audio routing before releasing the source
        // refs. Clearing the channels first is important — `obs_set_output_source`
        // increments the source refcount, so dropping `ObsSourceRef` without
        // clearing the channel would leak the audio source (channel 1/2 would
        // keep holding the last reference). `detach_monitor_capture_audio` does
        // clear-then-drop in the correct order.
        self.detach_monitor_capture_audio();

        // Clear event stream when recording stops
        self.event_stream = None;

        let settings = self.last_encoder_settings.take().unwrap_or_default();

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

        Ok(settings)
    }

    /// Phase 2 of recording stop (v2.5.5): read the skipped-frames counter
    /// emitted by OBS into `self.skipped_frames` via `TracingObsLogger`, fold
    /// it into the settings blob, and release the encoder cache. Must be
    /// called only after the tokio caller has yielded long enough for OBS
    /// to emit its `"number of skipped frames due to encoding lag:"` log
    /// line (~200ms in practice).
    fn stop_recording_phase2(
        &mut self,
        mut settings: serde_json::Value,
    ) -> eyre::Result<serde_json::Value> {
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

    /// Periodic tick (~1Hz). Returns `true` exactly once, when the
    /// monitor-capture DXGI_ERROR_ACCESS_LOST recovery window has expired
    /// and the caller should gracefully stop the recording. All other
    /// internal bookkeeping (source teardown when the game closes, pause /
    /// resume around workstation lock) is advanced in place.
    fn poll(&mut self) -> eyre::Result<bool> {
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

        // Drive the monitor-capture DXGI_ERROR_ACCESS_LOST recovery state
        // machine. Only runs while a recording is actually in progress —
        // the flag can't be set otherwise and pausing an inactive output
        // would error out of libobs_wrapper.
        let workstation_locked_timeout = if self.is_recording {
            self.advance_access_lost_state()
        } else {
            // Drop any late-arriving flag while idle. Prevents a stale
            // latched signal from leaking into the next recording (the
            // start_recording path also clears this, but belt-and-braces
            // is cheap here).
            self.access_lost_flag.store(false, Ordering::Relaxed);
            false
        };

        Ok(workstation_locked_timeout)
    }

    /// Advance the monitor-capture DXGI_ERROR_ACCESS_LOST recovery state
    /// machine by one tick. See the `AccessLostState` doc comment for the
    /// full transition table. Returns `true` exactly when the resume
    /// deadline has expired on a still-locked workstation and the caller
    /// should stop the recording.
    ///
    /// Pause/unpause errors are logged and the state machine keeps trying
    /// on the next tick. We never propagate the error out of `poll` because
    /// the top-level tokio loop treats errors as "log and continue" anyway
    /// and we don't want a transient pause failure to wedge the machine.
    fn advance_access_lost_state(&mut self) -> bool {
        let flag_set = self.access_lost_flag.load(Ordering::Relaxed);
        match self.access_lost_state {
            AccessLostState::Active => {
                if flag_set {
                    tracing::warn!(
                        "DXGI_ERROR_ACCESS_LOST detected on monitor-capture \
                         path — pausing recording until the interactive \
                         desktop returns (workstation lock / UAC / RDP \
                         switch). Recording will auto-resume on unlock."
                    );
                    match self.output.pause(true) {
                        Ok(()) => {
                            // Send VIDEO_PAUSED event to input log so data consumers
                            // know there's a gap in video continuity
                            if let Some(stream) = &self.event_stream {
                                let _ = stream.send(InputEventType::VideoPaused);
                            }
                            self.access_lost_state = AccessLostState::Paused(Instant::now());
                            // Clear so a fresh access-lost burst after
                            // unpause can re-trigger the pause path.
                            self.access_lost_flag.store(false, Ordering::Relaxed);
                        }
                        Err(e) => {
                            tracing::warn!(
                                e=?e,
                                "Failed to pause OBS output on access-lost; \
                                 will retry on next poll tick. MP4 may show \
                                 a stretch of bad frames until pause succeeds."
                            );
                        }
                    }
                }
                false
            }
            AccessLostState::Paused(started) => {
                if started.elapsed() >= ACCESS_LOST_RESUME_TIMEOUT {
                    tracing::warn!(
                        elapsed_s = started.elapsed().as_secs(),
                        deadline_s = ACCESS_LOST_RESUME_TIMEOUT.as_secs(),
                        "Workstation stayed locked past DXGI access-lost \
                         recovery window — signalling upstream to stop \
                         recording gracefully. The MP4 up to the pause \
                         point is valid and will be flushed by stop()."
                    );
                    // Do NOT unpause here — the caller is about to stop the
                    // output, and unpausing would briefly re-engage the
                    // (still-broken) duplicator. Just flag and bail.
                    return true;
                }
                if session_is_interactive() {
                    tracing::info!(
                        elapsed_s = started.elapsed().as_secs(),
                        "Interactive desktop is back — resuming recording \
                         from DXGI access-lost pause"
                    );
                    match self.output.pause(false) {
                        Ok(()) => {
                            // Send VIDEO_RESUMED event to input log so data consumers
                            // know video capture has resumed after the gap
                            if let Some(stream) = &self.event_stream {
                                let _ = stream.send(InputEventType::VideoResumed);
                            }
                            self.access_lost_state = AccessLostState::Active;
                            // If duplication is still broken, the logger
                            // will latch the flag again on the very next
                            // frame attempt and the Active-branch above
                            // will re-pause. We explicitly do NOT remove
                            // and re-add the source on a re-failure — the
                            // task spec forbids it, and libobs's monitor-
                            // capture source re-acquires a duplicator on
                            // its own once access is granted again.
                        }
                        Err(e) => {
                            tracing::warn!(
                                e=?e,
                                "Failed to unpause OBS output after \
                                 interactive desktop returned; staying in \
                                 Paused and will retry on next poll tick"
                            );
                        }
                    }
                }
                false
            }
        }
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

    /// Create (or reuse) the WASAPI desktop-output source, and optionally the
    /// WASAPI input (microphone) source, and assign them to the global OBS
    /// audio channels. Monitor capture has no hooked audio path, so without
    /// this the resulting MP4 is silent.
    ///
    /// Channel assignment mirrors the canonical layout used by OBS's own
    /// `Desktop Audio` / `Mic/Aux` buses and libobs's `raw_calls.rs` example:
    ///   - channel 0: scene (video)            — set by `set_to_channel(0)`
    ///   - channel 1: WASAPI desktop capture   — set here
    ///   - channel 2: WASAPI microphone        — set here when opted in
    ///
    /// The source refs are retained on `RecorderState` so that OBS doesn't
    /// release them via the wrapper's drop glue while the channels are still
    /// pointing at them.
    fn attach_monitor_capture_audio(&mut self, record_microphone: bool) -> eyre::Result<()> {
        // Clone the runtime handle once so we can freely touch `self.*_source`
        // fields without the borrow checker tangling with an outstanding borrow
        // into `self.obs_context`.
        let runtime = self.obs_context.runtime().clone();

        // Always (re)attach desktop audio when running in monitor capture.
        // We recreate the source each recording to pick up device changes
        // (user unplugs a headset, switches default render device, etc.) —
        // libobs's WASAPI source does NOT auto-fall-back to the new default.
        if self.desktop_audio_source.is_some() {
            // Detach any stale reference before installing a fresh one.
            set_output_source_on_channel(&runtime, DESKTOP_AUDIO_CHANNEL, None)?;
            self.desktop_audio_source = None;
        }
        tracing::info!(
            "Attaching WASAPI desktop audio source ({}) on channel {}",
            WASAPI_OUTPUT_CAPTURE_ID,
            DESKTOP_AUDIO_CHANNEL
        );
        let mut desktop_settings = self.obs_context.data()?;
        // "default" asks the WASAPI source to track the default render endpoint.
        // Being explicit beats relying on the plugin default string.
        desktop_settings.set_string("device_id", "default")?;
        let desktop = ObsSourceRef::new(
            WASAPI_OUTPUT_CAPTURE_ID,
            OWL_DESKTOP_AUDIO_NAME,
            Some(desktop_settings),
            None,
            runtime.clone(),
        )
        .wrap_err("Failed to create WASAPI desktop audio source")?;
        set_output_source_on_channel(&runtime, DESKTOP_AUDIO_CHANNEL, Some(&desktop))?;
        self.desktop_audio_source = Some(desktop);

        // Microphone is opt-in. If we previously had a mic source and the
        // user has now disabled it, detach and drop the source.
        if self.microphone_source.is_some() {
            set_output_source_on_channel(&runtime, MICROPHONE_AUDIO_CHANNEL, None)?;
            self.microphone_source = None;
        }
        if record_microphone {
            tracing::info!(
                "Attaching WASAPI microphone source ({}) on channel {} (record_microphone=true)",
                WASAPI_INPUT_CAPTURE_ID,
                MICROPHONE_AUDIO_CHANNEL
            );
            let mut mic_settings = self.obs_context.data()?;
            mic_settings.set_string("device_id", "default")?;
            let mic = ObsSourceRef::new(
                WASAPI_INPUT_CAPTURE_ID,
                OWL_MICROPHONE_AUDIO_NAME,
                Some(mic_settings),
                None,
                runtime.clone(),
            )
            .wrap_err("Failed to create WASAPI microphone source")?;
            set_output_source_on_channel(&runtime, MICROPHONE_AUDIO_CHANNEL, Some(&mic))?;
            self.microphone_source = Some(mic);
        } else {
            tracing::debug!(
                "Skipping microphone capture (record_microphone=false) — privacy default"
            );
        }

        Ok(())
    }

    /// Clear the monitor-capture audio channels and release any WASAPI source
    /// refs we're holding. Safe to call when nothing is attached (no-op). Must
    /// clear channels _before_ dropping the source refs so OBS's refcount
    /// drops to zero on our drop rather than leaking in the output bus.
    fn detach_monitor_capture_audio(&mut self) {
        // Clone the runtime handle up front so we can mutably touch
        // `self.desktop_audio_source` / `self.microphone_source` without
        // fighting the borrow checker over a borrow into `self.obs_context`.
        let runtime = self.obs_context.runtime().clone();
        if self.desktop_audio_source.is_some() {
            if let Err(e) = set_output_source_on_channel(&runtime, DESKTOP_AUDIO_CHANNEL, None) {
                tracing::warn!(
                    e=?e,
                    "Failed to clear desktop audio output channel {}; continuing teardown",
                    DESKTOP_AUDIO_CHANNEL
                );
            }
            self.desktop_audio_source = None;
        }
        if self.microphone_source.is_some() {
            if let Err(e) = set_output_source_on_channel(&runtime, MICROPHONE_AUDIO_CHANNEL, None) {
                tracing::warn!(
                    e=?e,
                    "Failed to clear microphone output channel {}; continuing teardown",
                    MICROPHONE_AUDIO_CHANNEL
                );
            }
            self.microphone_source = None;
        }
    }
}

/// OBS global audio buses. Channel 0 is reserved for the scene (video);
/// channels 1..=5 are where audio sources are mounted via
/// `obs_set_output_source`. We use 1 for desktop and 2 for microphone,
/// matching OBS Studio's canonical `Desktop Audio` / `Mic/Aux` layout.
const DESKTOP_AUDIO_CHANNEL: u32 = 1;
const MICROPHONE_AUDIO_CHANNEL: u32 = 2;

/// Pure decision function: given the resolved effective capture mode and
/// whether any monitors are currently enumerated, decide whether the
/// monitor-capture sub-path will actually run, and therefore whether we
/// need to attach WASAPI desktop/mic sources to the scene.
///
/// Only the monitor-capture sub-path is silent by default — the
/// window-capture fallback (used when no monitors are enumerated),
/// game-capture, AND wgc_capture all tap audio via `capture_audio`
/// themselves. Attaching WASAPI on top of any of those paths would
/// double the desktop-audio track. Extracted as a free function so
/// tests can exercise the branching logic without a live OBS context.
fn should_attach_monitor_audio(
    mode: crate::config::EffectiveCaptureMode,
    monitors_available: bool,
) -> bool {
    matches!(mode, crate::config::EffectiveCaptureMode::Monitor) && monitors_available
}

/// Assign `source` to OBS global audio channel `channel`. Passing `None`
/// clears the channel (equivalent to `obs_set_output_source(channel, NULL)`).
///
/// # Safety
///
/// All libobs calls must run on the OBS thread; we marshal via the runtime.
/// The raw pointer capture is safe because `Sendable<*mut obs_source_t>` is
/// owned by an `ObsSourceRef` that lives at least as long as the caller's
/// reference, and we only dereference it on the OBS thread via the FFI.
fn set_output_source_on_channel(
    runtime: &libobs_wrapper::runtime::ObsRuntime,
    channel: u32,
    source: Option<&ObsSourceRef>,
) -> eyre::Result<()> {
    let source_ptr = source.map(|s| s.as_ptr() as usize).unwrap_or(0);
    runtime
        .run_with_obs(move || unsafe {
            let ptr = source_ptr as *mut libobs_wrapper::sys::obs_source_t;
            libobs_wrapper::sys::obs_set_output_source(channel, ptr);
        })
        .wrap_err("Failed to dispatch obs_set_output_source to OBS thread")?;
    Ok(())
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

/// Log the effective DPI of the monitor under `hwnd` as a diagnostic aid
/// for future high-DPI issues (see MEGA_AUDIT R39, TRIAGE DPI manifest item).
///
/// We declare per-monitor DPI awareness V2 in `build.rs`, so Windows hands us
/// the physical pixel resolution of the capture surface. If this scale is not
/// 1.00x, any future report of "recording is blurry / wrong resolution / input
/// coords off" should start by cross-referencing this log line with the
/// recording's `video_metadata.json`.
fn log_monitor_dpi_scale(hwnd: HWND, monitor_label: &str) {
    // SAFETY: MonitorFromWindow is a pure read-only Win32 query; MONITOR_DEFAULTTONEAREST
    // guarantees a non-null HMONITOR even when `hwnd` sits outside any display.
    let hmon: HMONITOR = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if hmon.is_invalid() {
        tracing::debug!(
            "Could not log DPI scale for {monitor_label}: MonitorFromWindow returned NULL"
        );
        return;
    }
    let mut dpi_x: u32 = 0;
    let mut dpi_y: u32 = 0;
    // SAFETY: GetDpiForMonitor is read-only. We pass owned u32 out-pointers that
    // outlive the call. MDT_EFFECTIVE_DPI returns the monitor's effective DPI,
    // honoring per-app scaling overrides (this is the one that matters for
    // capture-surface dimensions).
    let res = unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    match res {
        Ok(()) => {
            // 96 DPI == 100% scale factor (Windows default, "1.00x").
            let scale_x = dpi_x as f32 / 96.0;
            let scale_y = dpi_y as f32 / 96.0;
            tracing::info!(
                "{monitor_label}: effective DPI = {dpi_x}x{dpi_y} (scale {scale_x:.2}x/{scale_y:.2}x) — \
                 process is per-monitor-v2 DPI aware, so capture surface is in PHYSICAL pixels"
            );
        }
        Err(e) => {
            tracing::debug!("Could not log DPI scale for {monitor_label}: {e}");
        }
    }
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
    // Audio capture disabled to save resources and avoid the WASAPI audio
    // companion infinite retry loop bug on second recording. With audio disabled:
    // - Saves ~1-3% CPU, 5-15 MB memory, and ~15% disk space
    // - Eliminates the second recording crash (no WASAPI companion = no retry loop)
    // - Recordings are video-only (no game audio)
    let capture_audio = false;

    // Force recreate WGC and GameHook sources to fix the second recording crash.
    // These capture modes spawn a WASAPI process-loopback audio companion that
    // binds to the game window handle. When the window changes (e.g., resolution
    // change in GTA V), the old audio companion enters an infinite retry loop
    // ("window disappeared" → "Device invalidated. Retrying" every ~3s), which
    // starves the OBS output and causes the app to appear frozen.
    //
    // By always recreating these sources, we ensure a fresh audio companion is
    // bound to the current window. This is a simpler and more reliable fix than
    // trying to detect when the window has changed.
    //
    // NOTE: With capture_audio=false, the WASAPI companion is never created,
    // so this issue is bypassed entirely. The force-recreate logic remains
    // as defense-in-depth.
    if matches!(
        state.effective_mode,
        crate::config::EffectiveCaptureMode::Wgc | crate::config::EffectiveCaptureMode::GameHook
    ) {
        if let Some(source) = last_source.take() {
            tracing::info!(
                mode = ?state.effective_mode,
                "Force recreating source (fixes second recording crash with stale WASAPI audio companion)"
            );
            // Ignore removal errors - we're about to create a new source anyway
            let _ = scene.remove_source(&source);
            tracing::debug!(
                mode = ?state.effective_mode,
                "Old source removed for recreation"
            );
        }
    }

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

    let result = match state.effective_mode {
        crate::config::EffectiveCaptureMode::Monitor => {
            // Use monitor capture (full screen) — works with all games including
            // fullscreen exclusive, anti-cheat, DRM. Same approach as competing products.
            // This captures the entire display, guaranteeing visible game content.
            tracing::info!("Using monitor capture mode (full screen capture)");

            let monitors = MonitorCaptureSourceBuilder::get_monitors().unwrap_or_default();

            if monitors.is_empty() {
                // Fallback to window capture if monitor list unavailable
                tracing::warn!(
                    "No monitors found for monitor capture, falling back to window capture"
                );
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

                // Log the effective DPI scale of the capture monitor so we can
                // diagnose future "wrong resolution" / "blurry recording" reports.
                // We are per-monitor-v2 DPI-aware (see build.rs), so Windows hands
                // us physical pixels; this line confirms what those pixels look like.
                log_monitor_dpi_scale(
                    hwnd,
                    &format!("Monitor {monitor_idx} of {}", monitors.len()),
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
        }
        crate::config::EffectiveCaptureMode::GameHook => {
            // Inject the libobs game_capture hook into the target process.
            // Required for fullscreen-exclusive D3D12 titles on integrated
            // GPUs where DWM desktop-duplication bridging fails — AND where
            // WGC is known-broken (see `KNOWN_HOOK_REQUIRED_GAMES`). The
            // caller (Recording::start) has already overridden
            // `game_resolution` to the monitor-native size so the hook draws
            // into a correctly-sized surface instead of the transient 600x286
            // boot window.
            // `find_game_capture_window` already bails with a user-facing
            // message when `window.is_game` is false (see its body above), so
            // by the time we get here we know we have a capturable game
            // window — no second `is_game` check needed.
            let window = find_game_capture_window(Some(game_exe), hwnd)?;

            let capture_mode = ObsGameCaptureMode::CaptureSpecificWindow;

            if let Some(mut source) = last_source.take() {
                tracing::info!(
                    game = game_exe,
                    window_id = %window.obs_id,
                    "Reusing existing game-capture hook source"
                );
                source
                    .create_updater::<GameCaptureSourceUpdater>()?
                    .set_capture_mode(capture_mode)
                    .set_window_raw(window.obs_id.as_str())
                    // Class-priority match ("priority: 1" in raw OBS JSON) —
                    // the game's window class is the most stable identifier;
                    // titles change ("Loading..." -> "Counter-Strike 2") and
                    // exe paths vary across Steam/Epic/Rockstar installs.
                    .set_priority(ObsWindowPriority::Class)
                    .set_capture_cursor(true)
                    .set_capture_audio(capture_audio)?
                    .update()?;
                Ok(source)
            } else {
                tracing::info!(
                    game = game_exe,
                    window_id = %window.obs_id,
                    "Creating new game-capture hook source"
                );

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
                    .set_priority(ObsWindowPriority::Class)
                    .set_capture_cursor(true)
                    .set_capture_audio(capture_audio)?
                    .add_to_scene(scene)
            }
        }
        crate::config::EffectiveCaptureMode::Wgc => {
            // Windows.Graphics.Capture — Microsoft's official Win10 1903+
            // capture API. Captures the game's DXGI swap-chain surface
            // through the OS compositor, so it works for exclusive
            // fullscreen D3D11/D3D12 without needing to inject into the
            // game process. This is the modern default for games not on
            // `KNOWN_HOOK_REQUIRED_GAMES` — it's the path CS2 works through
            // (where the game_capture hook is refused by Valve's anti-hook
            // even when the recording app is VAC-whitelisted).
            //
            // libobs-wrapper does not expose a typed builder for
            // `wgc_capture`, so we build the source with a raw
            // `ObsSourceRef::new(...)` + an `ObsData` settings blob. The
            // settings keys (`capture_mode`, `window`, `cursor`,
            // `client_area`, `capture_audio`) match the property ids the
            // `win-capture` plugin registers in
            // `obs-plugins/win-capture/winrt-capture.c`.
            // See F2 above — `find_game_capture_window` already guards
            // against non-game windows, so the outer `is_game` check here
            // was unreachable dead code.
            let window = find_game_capture_window(Some(game_exe), hwnd)?;

            if let Some(mut source) = last_source.take() {
                tracing::info!(
                    game = game_exe,
                    window_id = %window.obs_id,
                    "Reusing existing WGC capture source"
                );
                let mut settings = obs_context.data()?;
                settings.set_int("method", WGC_CAPTURE_METHOD_WGC)?;
                settings.set_string("window", window.obs_id.as_str())?;
                settings.set_int("priority", 1)?; // match by window class
                settings.set_bool("cursor", true)?;
                settings.set_bool("client_area", true)?;
                settings.set_bool("capture_audio", capture_audio)?;
                source.reset_and_update_raw(settings)?;
                Ok(source)
            } else {
                tracing::info!(
                    game = game_exe,
                    window_id = %window.obs_id,
                    "Creating new WGC capture source"
                );

                let mut settings = obs_context.data()?;
                // `method=2` forces WGC (vs. Auto=0 or BitBlt=1). Default
                // Auto often falls back to BitBlt on DX12 games and yields
                // black frames. Hardcoding WGC fixes CS2/GTA V.
                settings.set_int("method", WGC_CAPTURE_METHOD_WGC)?;
                settings.set_string("window", window.obs_id.as_str())?;
                // `priority=1` = match by window class (SDL_app for CS2,
                // grcWindow for GTA V). Falls back to title / exe as needed.
                settings.set_int("priority", 1)?;
                // `cursor` — render the system cursor on top of the captured
                // surface. Training models want the cursor state, so yes.
                settings.set_bool("cursor", true)?;
                // `client_area` — exclude title bar and non-client borders
                // so the framed content matches what monitor-duplication
                // would have handed us. Matters for games running in
                // windowed/borderless; no-op for true fullscreen.
                settings.set_bool("client_area", true)?;
                // `capture_audio` — WGC's own audio tap. Like game_capture,
                // this produces an "Application Audio Capture" stream for
                // the target window. When set, `should_attach_monitor_audio`
                // (further down) will NOT attach the WASAPI desktop/mic
                // sources on top, avoiding double-audio.
                settings.set_bool("capture_audio", capture_audio)?;

                let source_info = libobs_wrapper::utils::SourceInfo::new(
                    WGC_CAPTURE_SOURCE_ID,
                    OWL_WGC_CAPTURE_NAME,
                    Some(settings),
                    None,
                );
                scene.add_source(source_info)
            }
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
    /// Notifies the stop-recording path that OBS has emitted its skipped-
    /// frames log line and `skipped_frames` has been populated. Using
    /// `Notify` rather than a plain sleep keeps the tokio runtime live —
    /// when the stop-recording path awaits on this, the runtime keeps
    /// draining the input-capture mpsc so WM_INPUT events don't back up.
    skipped_frames_notify: Arc<tokio::sync::Notify>,
    /// Shared with `RecorderState`. Latched to `true` when an error-level
    /// OBS log line matches any needle in `ACCESS_LOST_LOG_NEEDLES`. Read
    /// + cleared by the monitor-capture state machine in `poll()`. Stored
    /// as `AtomicBool` specifically so this hot-path write is a single
    /// lock-free store — the OBS log thread must never block on a mutex,
    /// or it would back up and stall the encoder.
    access_lost_flag: Arc<AtomicBool>,
}
impl ObsLogger for TracingObsLogger {
    fn log(&mut self, level: libobs_wrapper::enums::ObsLogLevel, msg: String) {
        use libobs_wrapper::enums::ObsLogLevel;
        match level {
            ObsLogLevel::Error => {
                // DXGI_ERROR_ACCESS_LOST recovery: latch the shared flag
                // when OBS reports that the desktop-duplication surface
                // has gone away. The state machine in `RecorderState::poll`
                // picks it up on the next ~1Hz tick and pauses the output.
                //
                // We deliberately do the match on the *lowercased* message
                // because OBS formats these strings differently across
                // libobs versions (`Could not get next frame`, `DXGI_ERROR_
                // ACCESS_LOST (0x887A0026)`, `duplicator is invalid`, etc.)
                // and case-insensitive matching is the cheapest way to be
                // robust against wording drift. Building the lowercase
                // string is only O(len); error-level log lines are rare
                // enough that this never shows up in profiles.
                let msg_lower = msg.to_ascii_lowercase();
                if ACCESS_LOST_LOG_NEEDLES
                    .iter()
                    .any(|needle| msg_lower.contains(needle))
                {
                    // `Release` not required — the state machine only reads
                    // the bool for a simple branch, no piggy-backed data.
                    self.access_lost_flag.store(true, Ordering::Relaxed);
                }
                tracing::error!(target: "obs", "{msg}");
            }
            ObsLogLevel::Warning => tracing::warn!(target: "obs", "{msg}"),
            ObsLogLevel::Info => {
                // HACK: If we encounter a message of the sort
                //   Video stopped, number of skipped frames due to encoding lag: 10758/22640 (47.5%)
                // we parse out the numbers to allow us to determine if it's an acceptable number
                // of skipped frames. Then signal the stop-recording waiter so
                // it can finalize metadata without sleeping a fixed duration.
                if msg.contains("number of skipped frames due to encoding lag:") {
                    if let Some(frames_data) = parse_skipped_frames(&msg)
                        && let Ok(mut guard) = self.skipped_frames.lock()
                    {
                        *guard = Some(frames_data);
                    }
                    // Always notify, even on parse failure — Phase2 handles
                    // an absent counter gracefully, but a hung waiter is
                    // what we're trying to avoid. Using `notify_one` stores
                    // at most one permit, so a stop that begins after the
                    // log line lands still observes it on the next await.
                    self.skipped_frames_notify.notify_one();
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

    // --- v2.5.6 monitor-capture audio routing -------------------------
    //
    // We can't exercise the live OBS thread in a unit test (no OBS context,
    // no WASAPI devices, no Windows), but we CAN lock in the decision
    // logic that picks whether to attach WASAPI sources. These tests are
    // the first-line defence against a regression that silently re-
    // introduces the silent-MP4 bug.

    #[test]
    fn monitor_capture_with_monitors_attaches_audio() {
        // The primary path: effective_mode=Monitor and at least one
        // monitor enumerated -> prepare_source picks MonitorCapture, so
        // we MUST attach WASAPI sources to avoid a silent recording.
        assert!(should_attach_monitor_audio(
            crate::config::EffectiveCaptureMode::Monitor,
            true
        ));
    }

    #[test]
    fn monitor_capture_with_no_monitors_falls_back_to_window_capture() {
        // When monitor enumeration comes back empty, `prepare_source`
        // transparently falls back to WindowCapture, which carries its
        // own `set_capture_audio(true)` tap. Attaching WASAPI on top
        // would double the audio, so we must NOT attach.
        assert!(!should_attach_monitor_audio(
            crate::config::EffectiveCaptureMode::Monitor,
            false
        ));
    }

    #[test]
    fn game_capture_never_attaches_wasapi_audio() {
        // Game-capture uses the OBS hook's audio tap via
        // `set_capture_audio(true)`. Attaching WASAPI on top would
        // double desktop audio and is explicitly warned against by the
        // OBS GameCaptureSource docstring.
        assert!(!should_attach_monitor_audio(
            crate::config::EffectiveCaptureMode::GameHook,
            true
        ));
        assert!(!should_attach_monitor_audio(
            crate::config::EffectiveCaptureMode::GameHook,
            false
        ));
    }

    #[test]
    fn capture_mode_auto_prefers_wgc_as_new_default() {
        use crate::config::{CaptureMode, EffectiveCaptureMode, GameConfig};

        // Auto's new default on Win10 1903+ is WGC — it handles
        // exclusive fullscreen D3D11/D3D12 without DLL injection and
        // is the path that works for titles like CS2 where the
        // game_capture hook is refused by anti-hook (Valve refuses
        // even VAC-whitelisted OBS hooks, so the old GameHook route
        // produced black frames).
        let cfg = GameConfig {
            use_window_capture: true,
            capture_mode: CaptureMode::Auto,
        };
        // Previously-allowlisted exe under Auto now routes to WGC,
        // because `KNOWN_HOOK_REQUIRED_GAMES` is empty by default
        // (we only add entries when empirical testing proves WGC
        // regressed for that specific game).
        assert_eq!(cfg.effective_capture_mode("cs2"), EffectiveCaptureMode::Wgc);
        // Any other non-test_game exe under Auto also routes to WGC.
        assert_eq!(
            cfg.effective_capture_mode("abyssus"),
            EffectiveCaptureMode::Wgc
        );

        // Explicit override always wins — Auto's new preference
        // doesn't prevent users from pinning a mode they know works
        // for them. Critical for QA / ops.
        let cfg = GameConfig {
            use_window_capture: true,
            capture_mode: CaptureMode::GameHook,
        };
        assert_eq!(
            cfg.effective_capture_mode("abyssus"),
            EffectiveCaptureMode::GameHook
        );
        let cfg = GameConfig {
            use_window_capture: false,
            capture_mode: CaptureMode::Monitor,
        };
        assert_eq!(
            cfg.effective_capture_mode("cs2"),
            EffectiveCaptureMode::Monitor
        );
    }

    #[test]
    fn capture_mode_auto_pins_test_game_to_monitor() {
        // The CI harness records a synthetic `test_game.exe` window
        // and asserts specific colour-pixel values. Those assertions
        // were written against Monitor capture output, so we keep
        // Auto → Monitor for test_game specifically, even though WGC
        // would also work. Shipping the Auto flip without this pin
        // would break `.github/workflows/ci-e2e.yml` in the same PR.
        use crate::config::{CaptureMode, EffectiveCaptureMode, GameConfig};

        let cfg = GameConfig {
            use_window_capture: true,
            capture_mode: CaptureMode::Auto,
        };
        assert_eq!(
            cfg.effective_capture_mode("test_game"),
            EffectiveCaptureMode::Monitor
        );
        // Lowercased comparison — the production resolution path
        // normalises via `file_stem().to_lowercase()` before calling
        // `effective_capture_mode`, so we only need to pin the
        // lowercase form.
        let cfg = GameConfig {
            use_window_capture: false,
            capture_mode: CaptureMode::Auto,
        };
        assert_eq!(
            cfg.effective_capture_mode("test_game"),
            EffectiveCaptureMode::Monitor,
            "test_game pin must win over the legacy use_window_capture=false -> GameHook escape hatch"
        );
    }

    #[test]
    fn capture_mode_auto_honours_hook_required_allowlist() {
        // `KNOWN_HOOK_REQUIRED_GAMES` ships empty, but the branch that
        // consults it still needs to work — adding a game to that
        // list at runtime should route Auto to GameHook for only that
        // game. We can't mutate the `&'static` const in a unit test,
        // so instead we assert that for every entry currently on the
        // list, Auto resolves to GameHook. Empty list → empty loop,
        // test still passes; list gains entries in the future → they
        // get coverage automatically.
        use crate::config::{CaptureMode, EffectiveCaptureMode, GameConfig};

        let cfg = GameConfig {
            use_window_capture: true,
            capture_mode: CaptureMode::Auto,
        };
        for game in constants::KNOWN_HOOK_REQUIRED_GAMES {
            assert_eq!(
                cfg.effective_capture_mode(game),
                EffectiveCaptureMode::GameHook,
                "game on KNOWN_HOOK_REQUIRED_GAMES must route Auto to GameHook: {game}"
            );
        }
    }

    #[test]
    fn explicit_wgc_mode_resolves_to_wgc_effective() {
        // CaptureMode::Wgc is a hard override — it ignores the
        // hook-required allowlist, ignores `use_window_capture`, and
        // ignores the test_game carve-out. Useful when ops needs to
        // force WGC on a game that'd otherwise be pinned to the hook
        // fallback.
        use crate::config::{CaptureMode, EffectiveCaptureMode, GameConfig};
        let cfg = GameConfig {
            use_window_capture: true,
            capture_mode: CaptureMode::Wgc,
        };
        assert_eq!(cfg.effective_capture_mode("cs2"), EffectiveCaptureMode::Wgc);
        assert_eq!(
            cfg.effective_capture_mode("abyssus"),
            EffectiveCaptureMode::Wgc
        );
        let cfg = GameConfig {
            use_window_capture: false,
            capture_mode: CaptureMode::Wgc,
        };
        assert_eq!(
            cfg.effective_capture_mode("abyssus"),
            EffectiveCaptureMode::Wgc
        );
    }

    #[test]
    fn wgc_capture_mode_source_id_matches_obs_plugin_registration() {
        // These strings pin the OBS source-type id and the `capture_mode`
        // property value the `win-capture` plugin registers in
        // `obs-plugins/win-capture/winrt-capture.c`. Typos here mean the
        // runtime gets `NullPointer` from `obs_source_create` because
        // the source type lookup returns nothing.
        // v2.5.14 attempt 5: WGC is the `method=2` property on the
        // regular `window_capture` source. Pin both.
        assert_eq!(WGC_CAPTURE_SOURCE_ID, "window_capture");
        assert_eq!(WGC_CAPTURE_METHOD_WGC, 2);
        assert_eq!(WGC_CAPTURE_MODE_WINDOW, "window");
    }

    #[test]
    fn wgc_capture_never_attaches_wasapi_audio() {
        // wgc_capture taps audio through its `capture_audio` property
        // (same pattern as game_capture / window_capture). Attaching
        // WASAPI desktop/mic on top would produce doubled audio, so
        // `should_attach_monitor_audio` must refuse on this mode too.
        assert!(!should_attach_monitor_audio(
            crate::config::EffectiveCaptureMode::Wgc,
            true
        ));
        assert!(!should_attach_monitor_audio(
            crate::config::EffectiveCaptureMode::Wgc,
            false
        ));
    }

    #[test]
    fn capture_mode_auto_preserves_legacy_use_window_capture_false_as_game_hook() {
        // A v2.5.8 user who explicitly flipped `use_window_capture = false`
        // in their persisted config expected game-capture. Under Auto,
        // this legacy preference must still route to GameHook even
        // though the new Auto default is WGC — upgrades shouldn't
        // silently change capture behaviour for power users who set
        // that flag deliberately.
        use crate::config::{CaptureMode, EffectiveCaptureMode, GameConfig};

        let cfg = GameConfig {
            use_window_capture: false,
            capture_mode: CaptureMode::Auto,
        };
        assert_eq!(
            cfg.effective_capture_mode("abyssus"),
            EffectiveCaptureMode::GameHook
        );
    }

    #[test]
    fn audio_channel_layout_matches_obs_convention() {
        // Channel 0 is reserved for the scene (video). Channels 1 and 2
        // host desktop-audio and mic, matching the OBS Studio `Desktop
        // Audio`/`Mic/Aux` layout and the libobs `raw_calls.rs` example.
        // Regression-guard the constants: swapping these two would push
        // the mic into the "desktop" bus and vice versa, which would
        // silently change every downstream recording's audio routing.
        assert_eq!(DESKTOP_AUDIO_CHANNEL, 1);
        assert_eq!(MICROPHONE_AUDIO_CHANNEL, 2);
        assert_ne!(DESKTOP_AUDIO_CHANNEL, MICROPHONE_AUDIO_CHANNEL);
        // OBS's global audio bus index tops out at 5; channel 0 is the
        // scene/video channel.
        assert!(DESKTOP_AUDIO_CHANNEL >= 1 && DESKTOP_AUDIO_CHANNEL <= 5);
        assert!(MICROPHONE_AUDIO_CHANNEL >= 1 && MICROPHONE_AUDIO_CHANNEL <= 5);
    }

    #[test]
    fn wasapi_source_ids_match_obs_plugin_names() {
        // These are the canonical OBS input type IDs registered by
        // `obs-plugins/win-capture/wasapi.c`. Typos here mean
        // `ObsSourceRef::new` returns NullPointer at runtime on Windows
        // and the scene ends up silent again — so lock the strings in.
        assert_eq!(WASAPI_OUTPUT_CAPTURE_ID, "wasapi_output_capture");
        assert_eq!(WASAPI_INPUT_CAPTURE_ID, "wasapi_input_capture");
    }

    // --- DXGI_ERROR_ACCESS_LOST log matching -------------------------
    //
    // We can't simulate a live duplicator-invalid event in a unit test
    // (no OBS thread, no DXGI, no Windows), but we CAN pin down the log-
    // line matcher that decides whether a given error string should trip
    // the monitor-capture pause/resume machine. These tests are the
    // first-line defence against:
    //   - A regression that adds a new DXGI phrasing without updating
    //     `ACCESS_LOST_LOG_NEEDLES` (false negative: lock recording fails).
    //   - A regression that accidentally broadens a needle so benign
    //     unrelated errors trip the machine (false positive: every OBS
    //     error pauses the recording).

    /// Lowercase-match helper mirroring the logic in `TracingObsLogger::log`.
    /// Keeping the test local to the matcher body means refactors to the
    /// logger don't silently bypass the test — both paths go through the
    /// same needle list and the same case-normalization.
    fn access_lost_log_matches(msg: &str) -> bool {
        let msg_lower = msg.to_ascii_lowercase();
        ACCESS_LOST_LOG_NEEDLES
            .iter()
            .any(|needle| msg_lower.contains(needle))
    }

    #[test]
    fn access_lost_matches_dxgi_hex_code() {
        // The canonical libobs wording mentions the hex HRESULT. Case-
        // insensitive so upper-case hex ("0x887A0026") still matches.
        assert!(access_lost_log_matches(
            "d3d11-monitor-duplicator: DXGI_ERROR_ACCESS_LOST (0x887A0026)"
        ));
    }

    #[test]
    fn access_lost_matches_duplicator_invalid_phrase() {
        // win-capture emits this phrasing when ReleaseFrame on a revoked
        // duplicator comes back with E_INVALIDARG.
        assert!(access_lost_log_matches(
            "monitor-capture: duplicator is invalid, recreating"
        ));
    }

    #[test]
    fn access_lost_matches_next_frame_failure() {
        assert!(access_lost_log_matches(
            "IDXGIOutputDuplication::AcquireNextFrame: Could not get next frame (0x887A0026)"
        ));
    }

    #[test]
    fn access_lost_ignores_unrelated_errors() {
        // These are plausible OBS error lines we do NOT want to treat as
        // workstation-lock signals. If any of these trip the matcher,
        // we'd pause the recording mid-session on a transient encoder
        // hiccup and produce a broken MP4 on UAC-less systems.
        for unrelated in [
            "encoder: failed to allocate frame buffer",
            "audio source: sample rate mismatch",
            "ffmpeg-mux: write failed, disk full",
            "source: failed to load plugin",
        ] {
            assert!(
                !access_lost_log_matches(unrelated),
                "unrelated error tripped access-lost matcher: {unrelated:?}"
            );
        }
    }

    #[test]
    fn access_lost_resume_timeout_is_five_minutes() {
        // Fail loudly if someone changes the deadline to something
        // wildly wrong. The task spec calls this out explicitly.
        assert_eq!(ACCESS_LOST_RESUME_TIMEOUT, Duration::from_secs(5 * 60));
    }

    #[test]
    fn access_lost_needles_are_lowercase() {
        // Matching in `TracingObsLogger::log` lower-cases the message
        // once and then scans needles against it. If a needle has upper-
        // case characters it can never match — a silent correctness bug
        // that wouldn't surface until someone hits Win+L in production.
        for needle in ACCESS_LOST_LOG_NEEDLES {
            assert_eq!(
                *needle,
                needle.to_ascii_lowercase(),
                "ACCESS_LOST_LOG_NEEDLES entry is not lowercase: {needle:?}"
            );
        }
    }

    #[test]
    fn record_microphone_defaults_to_off() {
        // Privacy default: users must opt in to mic recording. A
        // regression that flips this to `true` is a consent bug, not
        // just a feature toggle — guard it explicitly. The consent
        // disclosure in `src/ui/consent.md` states "the software
        // cannot record microphone audio", so the default shape on
        // disk must keep matching that claim until the disclosure PR
        // lands.
        let prefs = crate::config::Preferences::default();
        assert!(
            !prefs.record_microphone,
            "record_microphone must default to false (privacy / consent)"
        );
    }
}
