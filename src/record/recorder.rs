use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use color_eyre::{
    Result,
    eyre::{Context as _, OptionExt as _, bail},
};
use egui_wgpu::wgpu::DeviceType;
use input_capture::{ConsentGuard, InputCapture};
use windows::Win32::Foundation::HWND;

use crate::{
    app_state::{AppState, RecordingStatus},
    config::{EncoderSettings, GameConfig, RecordingBackend, consent_guard_from_config},
    output_types::InputEventType,
    record::{
        LocalRecording,
        input_recorder::InputEventStream,
        obs_embedded_recorder::ObsEmbeddedRecorder,
        obs_socket_recorder::ObsSocketRecorder,
        recording::{Recording, RecordingParams},
    },
};
use constants::{
    MIN_FREE_SPACE_MB, encoding::VideoEncoderType, unsupported_games::UnsupportedGames,
};

#[async_trait::async_trait(?Send)]
pub trait VideoRecorder {
    fn id(&self) -> &'static str;
    fn available_encoders(&self) -> &[VideoEncoderType];

    /// Start a recording.
    ///
    /// R46: `consent` MUST be `ConsentStatus::Granted`. Implementations MUST
    /// call `consent.require_granted()?` before initializing any OBS source,
    /// opening any capture pipeline, or writing any bytes to disk. This is
    /// the final gate before video/audio capture begins.
    ///
    /// `record_microphone` on `RecordingParams`: capture microphone input
    /// alongside desktop audio in monitor-capture mode. Recorders that use
    /// OBS's game-capture hook (or the socket backend's window-capture)
    /// ignore this — the hook already taps game/desktop audio directly.
    #[allow(clippy::too_many_arguments)]
    async fn start_recording(
        &mut self,
        dummy_video_path: &Path,
        pid: u32,
        hwnd: HWND,
        game_exe: &str,
        video_settings: EncoderSettings,
        game_config: GameConfig,
        record_microphone: bool,
        game_resolution: (u32, u32),
        event_stream: InputEventStream,
        consent: ConsentGuard,
    ) -> Result<()>;
    /// Result contains any additional metadata the recorder wants to return about the recording
    /// If this returns an error, the recording will be invalidated with the error message
    async fn stop_recording(&mut self) -> Result<serde_json::Value>;
    /// Called periodically for any work the recorder might need to do
    async fn poll(&mut self) -> PollUpdate;
    /// Returns true if the window is capturable by the recorder
    fn is_window_capturable(&self, hwnd: HWND) -> bool;
    /// Returns true if the recording has failed to hook after the timeout period
    async fn check_hook_timeout(&mut self) -> bool;
}
#[derive(Default)]
pub struct PollUpdate {
    pub active_fps: Option<f64>,
}
pub struct Recorder {
    recording_dir: Box<dyn FnMut() -> PathBuf>,
    recording: Option<Recording>,
    app_state: Arc<AppState>,
    video_recorder: Box<dyn VideoRecorder>,
}

impl Recorder {
    pub async fn new(
        recording_dir: Box<dyn FnMut() -> PathBuf>,
        app_state: Arc<AppState>,
    ) -> Result<Self> {
        tracing::debug!("Recorder::new() called");
        let backend = app_state
            .config
            .read()
            .unwrap()
            .preferences
            .recording_backend;
        tracing::debug!("Recording backend: {:?}", backend);

        // Incredibly ugly hack: assume that the first dGPU is the one we want,
        // and that this list agrees with OBS's. There's no real guarantee that
        // this is the case, and that the target game is even running on the dGPU,
        // but it's a first-pass solution for now.
        //
        // TODO: Investigate what OBS actually does here. I spent over an hour
        // pouring through the OBS source code and couldn't find anything of
        // note with regards to how it chooses the adapter; I might have to
        // reach out to an OBS developer if this becomes an issue again.
        let adapter_index = app_state
            .adapter_infos
            .iter()
            .position(|a| a.device_type == DeviceType::DiscreteGpu)
            .unwrap_or_default();

        tracing::info!(
            "Initializing recorder with adapter index {adapter_index} ({:?})",
            app_state.adapter_infos.get(adapter_index)
        );

        tracing::debug!("Creating video recorder backend");
        let video_recorder: Box<dyn VideoRecorder> = match backend {
            RecordingBackend::Embedded => Box::new(ObsEmbeddedRecorder::new(adapter_index).await?),
            RecordingBackend::Socket => Box::new(ObsSocketRecorder::new().await?),
        };

        tracing::info!("Using {} as video recorder", video_recorder.id());
        tracing::debug!("Recorder::new() complete");
        Ok(Self {
            recording_dir,
            recording: None,
            app_state,
            video_recorder,
        })
    }

    pub fn recording(&self) -> Option<&Recording> {
        self.recording.as_ref()
    }

    pub fn available_video_encoders(&self) -> &[VideoEncoderType] {
        self.video_recorder.available_encoders()
    }

    pub async fn start(
        &mut self,
        input_capture: &InputCapture,
        unsupported_games: &UnsupportedGames,
    ) -> Result<()> {
        if self.recording.is_some() {
            return Ok(());
        }

        // R46 (GDPR/CCPA): refuse to start a recording session if the user
        // has not accepted the current consent disclosure. Check this BEFORE
        // we create any directory, query free space, probe the foreground
        // window, or instantiate any capture object — consent is the gate.
        {
            let config = self.app_state.config.read().unwrap();
            consent_guard_from_config(&config).require_granted()?;
        }

        let recording_location = (self.recording_dir)();

        let local_recording = LocalRecording::create_at(&recording_location)
            .wrap_err("Failed to create directory for recording. Did you install GameData Recorder to a location where your account is allowed to write files?")?;

        struct DeleteRecordingOnExit(Option<LocalRecording>);
        impl Drop for DeleteRecordingOnExit {
            fn drop(&mut self) {
                if let Some(recording) = self.0.take()
                    && let Err(e) = recording.delete_without_abort_sync()
                {
                    tracing::error!(e=?e, "Failed to delete recording folder on failure to start recording: {}: {:?}", recording.info().folder_path.display(), e);
                }
            }
        }
        impl DeleteRecordingOnExit {
            pub fn disarm(&mut self) {
                self.0 = None;
            }
        }
        let mut delete_recording_on_exit = DeleteRecordingOnExit(Some(local_recording));

        let free_space_mb = get_free_space_in_mb(&recording_location);
        if let Some(free_space_mb) = free_space_mb
            && free_space_mb < MIN_FREE_SPACE_MB
        {
            bail!(
                "There is not enough free space on the disk to record. Please free up some space. Required: at least {MIN_FREE_SPACE_MB} MB, available: {free_space_mb} MB"
            );
        }

        let Some((game_exe, pid, hwnd)) =
            get_foregrounded_game().wrap_err("failed to get foregrounded game")?
        else {
            bail!(
                "You do not have a game window in focus. Please focus on a game window and try again."
            );
        };

        let game_exe_without_extension = std::path::Path::new(&game_exe)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| game_exe.clone())
            .to_lowercase();
        if let Some(unsupported) = unsupported_games.get(&game_exe_without_extension) {
            bail!("{game_exe} is not supported: {}", unsupported.reason);
        }

        if let Err(error) = is_process_game_shaped(pid) {
            bail!(
                "This application ({game_exe}) doesn't look like a game. Please contact us if you think this is a mistake. Error: {error}"
            );
        }

        tracing::info!(
            game_exe,
            ?pid,
            ?hwnd,
            recording_location=%recording_location.display(),
            "Starting recording"
        );

        let (params, consent) = {
            let config = self.app_state.config.read().unwrap();
            let params = RecordingParams {
                recording_location: recording_location.clone(),
                game_exe: game_exe.clone(),
                pid,
                hwnd,
                video_settings: config.preferences.encoder.clone(),
                game_config: config
                    .preferences
                    .games
                    .get(&game_exe_without_extension)
                    .cloned()
                    .unwrap_or_default(),
                record_microphone: config.preferences.record_microphone,
            };
            // Compute the guard again under the same lock snapshot so we
            // don't race with the user revoking consent between the top-of-
            // function gate and the recorder start.
            (params, consent_guard_from_config(&config))
        };

        let recording = Recording::start(
            self.video_recorder.as_mut(),
            params,
            input_capture,
            consent,
        )
        .await;

        let recording = match recording {
            Ok(recording) => recording,
            Err(e) => {
                tracing::error!(game_exe=?game_exe, e=?e, "Failed to start recording");
                return Err(e);
            }
        };

        delete_recording_on_exit.disarm();

        self.recording = Some(recording);
        *self.app_state.state.write().unwrap() = RecordingStatus::Recording {
            start_time: Instant::now(),
            game_exe,
            current_fps: None,
        };
        Ok(())
    }

    pub async fn seen_input(&mut self, e: input_capture::Event) -> Result<()> {
        let Some(recording) = self.recording.as_ref() else {
            return Ok(());
        };
        recording
            .input_stream()
            .send(InputEventType::from_input_event(e)?)?;
        Ok(())
    }

    /// Flush all pending input events to disk
    pub async fn flush_input_events(&mut self) -> Result<()> {
        let Some(recording) = self.recording.as_mut() else {
            return Ok(());
        };
        recording.flush_input_events().await
    }

    /// Stops the current recording. Returns the recording folder path of the
    /// session that was just saved (if any), so callers can use it as a
    /// dedup key when enqueueing the session for auto-upload.
    pub async fn stop(&mut self, input_capture: &InputCapture) -> Result<Option<PathBuf>> {
        let Some(recording) = self.recording.take() else {
            return Ok(None);
        };

        let session_path = recording.recording_location().to_path_buf();

        recording
            .stop(
                self.video_recorder.as_mut(),
                &self.app_state.adapter_infos,
                input_capture,
            )
            .await?;
        *self.app_state.state.write().unwrap() = RecordingStatus::Stopped;

        tracing::info!("Recording stopped");
        Ok(Some(session_path))
    }

    pub async fn poll(&mut self) {
        let update = self.video_recorder.poll().await;
        if let Some(fps) = update.active_fps {
            let mut state = self.app_state.state.write().unwrap();
            if let RecordingStatus::Recording { current_fps, .. } = &mut *state {
                *current_fps = Some(fps);
            }
            if let Some(recording) = self.recording.as_mut() {
                recording.update_fps(fps);
            }
        }
    }

    pub fn is_window_capturable(&self, hwnd: HWND) -> bool {
        self.video_recorder.is_window_capturable(hwnd)
    }

    pub async fn check_hook_timeout(&mut self) -> bool {
        self.video_recorder.check_hook_timeout().await
    }

    /// Returns the current game exe name if recording, None otherwise
    pub fn current_game_exe(&self) -> Option<String> {
        self.recording.as_ref().map(|r| r.game_exe().to_string())
    }
}

fn get_free_space_in_mb(path: &std::path::Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let path = dunce::canonicalize(path).ok()?;

    // Find the disk with the longest matching mount point
    disks
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space() / 1024 / 1024)
}

/// Processes that should never be recorded (ourselves + common non-game apps)
const SELF_AND_SYSTEM_BLACKLIST: &[&str] = &[
    "gamedata-recorder.exe",
    "owl-control.exe",
    "explorer.exe",
    "steamwebhelper.exe",
    "steam.exe",
    "epicgameslauncher.exe",
    "unrealcefsubprocess.exe",
    "gog.exe",
    "galaxyclient.exe",
    "origin.exe",
    "uplay.exe",
    "battlenet.exe",
    "chrome.exe",
    "firefox.exe",
    "msedge.exe",
    "discord.exe",
    "slack.exe",
    "spotify.exe",
    "code.exe",
    "windowsterminal.exe",
    "cmd.exe",
    "powershell.exe",
    "taskmgr.exe",
    "searchhost.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "applicationframehost.exe",
    "textinputhost.exe",
    "systemsettings.exe",
    "nvidia share.exe",
    "nvcontainer.exe",
    // Video players
    "vlc.exe",
    "mpv.exe",
    "potplayer.exe",
    "potplayermini64.exe",
    // Streaming / recording tools (would cause recursive capture)
    "obs64.exe",
    "obs32.exe",
    "streamlabs obs.exe",
    "twitchstudio.exe",
    // Remote desktop / streaming
    "parsec.exe",
    "sunshine.exe",
    "moonlight.exe",
    // Creative apps (load D3D but not games)
    "blender.exe",
    "resolve.exe",
    "photoshop.exe",
    // Hardware monitoring
    "afterburner.exe",
    "rtss.exe",
    "rivatuner.exe",
    // Communication
    "teams.exe",
    "msteams.exe",
    // Steam/Epic launcher helpers (load D3D, briefly foreground between launcher and game)
    "steamapprun.exe",
    "gameoverlayui.exe",
    "steamoverlayrenderhelper64.exe",
    "galaxyclient helper.exe",
    "epicwebhelper.exe",
    "socialclubhelper.exe",
    // Rockstar launcher
    "rockstarservice.exe",
    "launcherdll.exe",
    "rockstarlauncher.exe",
    "gtavlauncher.exe",
    "playgtav.exe",
    "rockstarerrorhandler.exe",
    "launcher.exe",
];

pub fn get_foregrounded_game() -> Result<Option<(String, game_process::Pid, HWND)>> {
    let (hwnd, pid) = game_process::foreground_window()?;

    let exe_path = game_process::exe_name_for_pid(pid)?;
    let exe_name = exe_path
        .file_name()
        .ok_or_eyre("Failed to get file name from exe path")?
        .to_string_lossy()
        .into_owned();

    // Never record ourselves or known non-game processes
    let exe_lower = exe_name.to_lowercase();
    if SELF_AND_SYSTEM_BLACKLIST.iter().any(|b| exe_lower == *b) {
        // Foreground is not a game — try scanning all running processes for a known game
        return find_running_game();
    }

    // Validate executable has .exe extension and strip it for whitelist comparison
    let Some(exe_stem) = exe_lower.strip_suffix(".exe") else {
        // Not a .exe in foreground — try scanning all processes
        return find_running_game();
    };

    // Only record games in the whitelist
    if !constants::GAME_WHITELIST.iter().any(|g| exe_stem == *g) {
        // Foreground is not a whitelisted game — try scanning all processes
        return find_running_game();
    }

    Ok(Some((exe_name, pid, hwnd)))
}

/// Scan all running processes for a known game (fallback when foreground isn't a game).
/// This enables recording even when the game window isn't in focus — common when
/// the recorder UI, Steam overlay, or a Rockstar launcher is in front.
fn find_running_game() -> Result<Option<(String, game_process::Pid, HWND)>> {
    let mut found: Option<(String, game_process::Pid)> = None;

    game_process::for_each_process(|entry| {
        // v2.5.5: UTF-16 decode via `exe_file_name`. The v2.5.4 implementation
        // downcast each UTF-16 code unit to a byte and called `from_utf8`,
        // which dropped every non-ASCII exe name on the floor — so a game
        // under `C:\游戏\Steam\...` or with Chinese characters in the exe
        // name was invisible to the whitelist on Chinese-locale Windows.
        let name = game_process::exe_file_name(&entry);
        let name_lower = name.to_lowercase();

        // Skip blacklisted processes
        if SELF_AND_SYSTEM_BLACKLIST.iter().any(|b| name_lower == *b) {
            return true;
        }

        // Check whitelist
        if let Some(stem) = name_lower.strip_suffix(".exe") {
            if constants::GAME_WHITELIST.iter().any(|g| stem == *g) {
                found = Some((name.clone(), game_process::Pid(entry.th32ProcessID)));
                return false; // stop scanning
            }
        }
        true // continue
    })?;

    let Some((exe_name, pid)) = found else {
        return Ok(None);
    };

    // Try to find the game's main window handle
    let hwnd = find_window_for_pid(pid);
    tracing::info!(
        "Found running game via process scan (not foreground): {} (pid={}, hwnd={:?})",
        exe_name,
        pid.0,
        hwnd
    );

    Ok(Some((exe_name, pid, hwnd)))
}

/// Window titles that indicate a launcher / non-game surface.
/// If the foreground window title contains one of these substrings we
/// refuse to use it as the capture HWND — OBS would otherwise record
/// the launcher UI instead of the actual game, which we saw in v2.5.1
/// session data (`window_name: "Rockstar Games Launcher"` at 1266x598
/// producing 1-FPS launcher frames while GTA V ran above it).
const LAUNCHER_TITLE_SUBSTRINGS: &[&str] = &[
    "launcher",
    "rockstar games",
    "epic games",
    "steam",
    "galaxy",
    "origin",
    "uplay",
    "battle.net",
    "social club",
];

fn window_title(hwnd: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    // Safety: GetWindowTextW is a plain Win32 call, we give it a valid
    // buffer and copy out at most 512 UTF-16 code units.
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// Shared state for the EnumWindows callback. We can't capture Rust closures
/// from `unsafe extern "system" fn`, so we thread everything through a
/// stack-allocated context pointer.
struct EnumContext {
    target_pid: u32,
    found: HWND,
    candidate_area: i64,
}

unsafe extern "system" fn enum_windows_proc(
    hwnd: HWND,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::core::BOOL {
    // v2.5.6: `BOOL` relocated to `windows::core::BOOL` in the `windows` crate
    // 0.60+ series; pre-v2.5.6 we imported it from `Win32::Foundation` and
    // CI went red on every push. The callback ABI is unchanged — it's still
    // a repr-transparent `i32` — so the fix is purely the import path.
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, GetWindowThreadProcessId, IsWindowVisible,
    };
    use windows::core::BOOL;
    let ctx = unsafe { &mut *(lparam.0 as *mut EnumContext) };
    let mut pid: u32 = 0;
    let _ = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid != ctx.target_pid {
        return BOOL(1); // continue enumeration
    }
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    let mut rect = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        return BOOL(1);
    }
    let area = (rect.right - rect.left) as i64 * (rect.bottom - rect.top) as i64;
    // Keep the largest visible window belonging to this PID — that's almost
    // always the game's render surface (menus/HUD windows are smaller).
    if area > ctx.candidate_area {
        ctx.candidate_area = area;
        ctx.found = hwnd;
    }
    BOOL(1)
}

/// Our own process id — cached on first use. We must never capture windows
/// belonging to the recorder itself or we end up recording our UI instead
/// of the game (this happened on a client's machine running v2.5.3: the
/// metadata reported `window_name: "GameData Recorder v2.5.3 | Recording"`
/// and 4 minutes of our own UI frames instead of GTA V).
fn self_pid() -> u32 {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    unsafe { GetCurrentProcessId() }
}

/// Find the main window handle for a given process ID.
///
/// v2.5.4 rewrite: actually enumerate windows belonging to the target PID
/// instead of returning `foreground_window()` and ignoring the argument.
/// Previously the function took a `pid` parameter but discarded it — if the
/// recorder's own egui window happened to be foreground when recording
/// started, we'd hook ourselves and record 4 minutes of UI pixels. Also
/// hard-block our own process ID as a defense in depth.
fn find_window_for_pid(pid: game_process::Pid) -> HWND {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

    let target_pid = pid.0 as u32;
    let own_pid = self_pid();

    // 1. Enumerate all top-level visible windows belonging to the game's PID.
    //    Pick the one with the largest client area — almost always the game's
    //    main render surface. Skip the search entirely if the target somehow
    //    equals our own PID (would cause self-capture).
    if target_pid != own_pid {
        let mut ctx = EnumContext {
            target_pid,
            found: HWND::default(),
            candidate_area: 0,
        };
        let lparam = LPARAM(&mut ctx as *mut EnumContext as isize);
        let _ = unsafe { EnumWindows(Some(enum_windows_proc), lparam) };
        if ctx.found.0 != std::ptr::null_mut() && ctx.candidate_area > 0 {
            tracing::info!(
                pid = target_pid,
                hwnd = ?ctx.found,
                title = %window_title(ctx.found),
                area = ctx.candidate_area,
                "Found real game window by PID enumeration"
            );
            return ctx.found;
        }
        tracing::warn!(
            pid = target_pid,
            "PID enumeration found no visible windows — game may still be \
             starting up or running in a way that hides its HWND from \
             EnumWindows. Falling back to foreground-window check."
        );
    } else {
        tracing::error!(
            pid = target_pid,
            own_pid,
            "Target PID equals our own PID — refusing to capture self. \
             This is a bug in the caller; returning NULL HWND."
        );
        return HWND::default();
    }

    // 2. Fallback: foreground window, but rejected if it's the recorder or a
    //    launcher surface. Never return a window owned by our own process.
    let Ok((hwnd, fg_pid)) = game_process::foreground_window() else {
        return HWND::default();
    };
    if fg_pid.0 as u32 == own_pid {
        tracing::warn!(
            "Foreground window belongs to the recorder itself — returning \
             NULL HWND so monitor capture is used instead of hooking our UI"
        );
        return HWND::default();
    }
    let title_lower = window_title(hwnd).to_lowercase();
    if LAUNCHER_TITLE_SUBSTRINGS
        .iter()
        .any(|needle| title_lower.contains(needle))
    {
        tracing::warn!(
            title = %title_lower,
            "Foreground window looks like a launcher — returning NULL HWND \
             so monitor capture is used instead of hooking the wrong window"
        );
        return HWND::default();
    }
    // Also reject any window whose title contains "gamedata recorder" — even
    // if the PID check above missed something, this catches the class of
    // self-capture bug we hit on the client.
    if title_lower.contains("gamedata recorder") || title_lower.contains("owl control") {
        tracing::warn!(
            title = %title_lower,
            "Foreground window title looks like our own UI — returning NULL"
        );
        return HWND::default();
    }
    hwnd
}

fn is_process_game_shaped(pid: game_process::Pid) -> Result<()> {
    // We've seen reports of this failing with certain games (e.g. League of Legends),
    // so this "fails safe" for now. It's possible that we don't actually want to
    // capture any games that this would be tripped up by, but it's hard to say that
    // without more evidence. I would assume the primary factor involved here is
    // the presence of an anticheat or an antitamper that obscures the retrieval of modules.
    match game_process::get_modules(pid) {
        Ok(modules) => {
            let mut has_graphics_api = false;
            for module in modules {
                let module = module.to_lowercase();

                // Check for Direct3D DLLs
                if module.contains("d3d")
                    || module.contains("dxgi")
                    || module.contains("d3d11")
                    || module.contains("d3d12")
                    || module.contains("d3d9")
                {
                    has_graphics_api = true;
                }

                // Check for OpenGL DLLs
                if module.contains("opengl32")
                    || module.contains("gdi32")
                    || module.contains("glu32")
                    || module.contains("opengl")
                {
                    has_graphics_api = true;
                }

                // Check for Vulkan DLLs
                if module.contains("vulkan")
                    || module.contains("vulkan-1")
                    || module.contains("vulkan32")
                {
                    has_graphics_api = true;
                }
            }

            if !has_graphics_api {
                bail!(
                    "this application doesn't use any graphics APIs (DirectX, OpenGL, or Vulkan)"
                );
            }
        }
        Err(e) => {
            tracing::warn!(?e, pid=?pid, "Failed to get modules for process");
        }
    }

    Ok(())
}
