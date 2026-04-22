//! Keyboard/mouse Raw Input capture.
//!
//! # Consent contract (R46, GDPR/CCPA)
//!
//! This module registers **global, system-wide** Windows Raw Input devices for
//! keyboard and mouse. Once registered with `RIDEV_INPUTSINK`, Windows delivers
//! every keystroke and mouse event on the user's machine to this process — it
//! does **not** restrict capture to a specific window, game, or foreground
//! application. Treating this as "during gameplay" would be legally false.
//!
//! Because of that reach, no `RegisterRawInputDevices` call may happen until
//! the user has explicitly accepted the current consent version via the
//! `ConsentView` UI. The gate is enforced by [`ConsentGuard`]:
//!
//! * [`KbmCapture::initialize`] takes a `&ConsentGuard` and calls
//!   [`ConsentGuard::require_granted`] **before** any Win32 registration.
//! * If consent is not granted ([`ConsentStatus::NotGranted`] or
//!   [`ConsentStatus::VersionMismatch`]), `initialize` returns `Err` and the
//!   function short-circuits before window/class creation.
//! * A bumped `CARGO_PKG_VERSION` invalidates any previously-granted consent,
//!   re-prompting the user — see `Config::consent_given_at_version` in the
//!   host crate.
//!
//! Callers (e.g. [`super::InputCapture::new`]) MUST NOT construct a
//! `KbmCapture` without a granted guard. The test suite in `src/config.rs`
//! asserts the recording entry point errors until consent is set.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, MutexGuard},
};

use color_eyre::{
    Result,
    eyre::{Context, bail, eyre},
};

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleA,
        UI::{
            Input::{
                self, GetRawInputData, HRAWINPUT,
                KeyboardAndMouse::{VK_LBUTTON, VK_MBUTTON, VK_RBUTTON, VK_XBUTTON1, VK_XBUTTON2},
                MOUSE_MOVE_ABSOLUTE, MOUSE_VIRTUAL_DESKTOP, RAWINPUT, RAWINPUTDEVICE,
                RAWINPUTDEVICE_FLAGS, RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK,
                RegisterRawInputDevices,
            },
            WindowsAndMessaging::{
                self, CreateWindowExA, DefWindowProcA, DestroyWindow, DispatchMessageA,
                GetMessageA, GetSystemMetrics, MSG, PostQuitMessage, RI_KEY_BREAK,
                RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, RI_MOUSE_BUTTON_5_DOWN,
                RI_MOUSE_BUTTON_5_UP, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP,
                RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP, RI_MOUSE_RIGHT_BUTTON_DOWN,
                RI_MOUSE_RIGHT_BUTTON_UP, RI_MOUSE_WHEEL, RegisterClassA, SM_CXSCREEN,
                SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
                SM_YVIRTUALSCREEN, SW_HIDE, ShowWindow, TranslateMessage, UnregisterClassA,
                WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSA, WS_EX_TOOLWINDOW,
            },
        },
    },
    core::PCSTR,
};

use crate::{Event, PressState};

/// Result of checking whether the user has consented to the current version.
///
/// Returned by the host crate when it computes whether stored consent still
/// matches the running binary's `CARGO_PKG_VERSION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentStatus {
    /// User has accepted the disclosure for the currently-running version.
    Granted,
    /// User has never accepted any version of the disclosure.
    NotGranted,
    /// User accepted a prior version; the disclosure has since changed and
    /// re-consent is required.
    VersionMismatch,
}

/// Thread-safe consent gate passed into any recording entry point.
///
/// Clone-able and cheap to copy. Holds a single `ConsentStatus`; the host
/// crate constructs one after reading `Config::consent_given_at_version` and
/// passes it down into [`super::InputCapture::new`] and the OBS recorder's
/// `start_recording` path.
#[derive(Debug, Clone)]
pub struct ConsentGuard {
    status: ConsentStatus,
}

impl ConsentGuard {
    /// Construct a guard from a computed status. The host crate is responsible
    /// for computing the status from its config (see `Config::consent_status`
    /// in the host crate).
    pub fn new(status: ConsentStatus) -> Self {
        Self { status }
    }

    /// Convenience constructor for callers that only need to say "consent is
    /// granted" (e.g. tests after setting the consent field).
    pub fn granted() -> Self {
        Self::new(ConsentStatus::Granted)
    }

    /// Convenience constructor for the default "no consent" case.
    pub fn not_granted() -> Self {
        Self::new(ConsentStatus::NotGranted)
    }

    /// The underlying status.
    pub fn status(&self) -> ConsentStatus {
        self.status
    }

    /// Returns `true` if the user has consented to the current version.
    pub fn is_granted(&self) -> bool {
        matches!(self.status, ConsentStatus::Granted)
    }

    /// Enforce the gate: returns `Err` if consent is not granted.
    ///
    /// Callers entering any code path that registers a global input hook,
    /// opens a video/audio capture pipeline, or reads the primary monitor
    /// MUST call this first and propagate the error.
    pub fn require_granted(&self) -> Result<()> {
        match self.status {
            ConsentStatus::Granted => Ok(()),
            ConsentStatus::NotGranted => Err(eyre!(
                "input capture blocked: user has not accepted the consent \
                 disclosure. The recording entry point must not be reached \
                 before ConsentView records acceptance."
            )),
            ConsentStatus::VersionMismatch => Err(eyre!(
                "input capture blocked: consent was granted for a prior \
                 version. The user must re-accept the updated disclosure \
                 before recording can resume."
            )),
        }
    }
}

#[derive(Default)]
pub struct ActiveKeys {
    pub keyboard: HashSet<u16>,
    pub mouse: HashSet<u16>,
}

pub struct KbmCapture {
    hwnd: HWND,
    class_name: PCSTR,
    h_instance: HINSTANCE,
    active_keys: Arc<Mutex<ActiveKeys>>,
}
impl Drop for KbmCapture {
    fn drop(&mut self) {
        unsafe {
            // Destroy window first; only unregister class if window was successfully destroyed.
            // UnregisterClassA fails with ERROR_CLASS_HAS_WINDOWS if any windows still exist.
            match DestroyWindow(self.hwnd) {
                Ok(_) => {
                    if let Err(e) = UnregisterClassA(self.class_name, Some(self.h_instance)) {
                        tracing::error!(
                            "Failed to unregister window class during cleanup: {:?}",
                            e
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to destroy window during cleanup: {:?}", e);
                }
            }
        }
    }
}
impl KbmCapture {
    /// Initialize global keyboard/mouse capture.
    ///
    /// R46 consent gate: the `consent` argument is checked **before** any
    /// Win32 window class is registered, before any window is created, and
    /// before `RegisterRawInputDevices` is called. If consent is not granted
    /// this function returns `Err` immediately without installing any hook.
    /// See the module-level doc comment for the full contract.
    pub fn initialize(active_keys: Arc<Mutex<ActiveKeys>>, consent: &ConsentGuard) -> Result<Self> {
        // R46: no hook installation without consent. This MUST run before any
        // Win32 call that registers a system-wide input sink.
        consent.require_granted()?;

        unsafe {
            let class_name = PCSTR(c"RawInputWindowClass".to_bytes_with_nul().as_ptr());
            let h_instance: HINSTANCE = GetModuleHandleA(None)?.into();

            let wc = WNDCLASSA {
                lpfnWndProc: Some(Self::window_proc),
                hInstance: h_instance,
                lpszClassName: class_name,
                ..Default::default()
            };

            if RegisterClassA(&wc) == 0 {
                use windows::Win32::Foundation::GetLastError;
                let error = GetLastError();
                bail!("failed to register window class: {error:?}");
            }

            let hwnd = CreateWindowExA(
                WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0),
                class_name,
                PCSTR::null(),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(h_instance),
                None,
            )
            .wrap_err("failed to create window")?;

            // Immediately hide the window - we only need it for Raw Input, not display
            let _ = ShowWindow(hwnd, SW_HIDE);

            tracing::debug!("RawInput window created: {hwnd:?}");

            // v2.5.13 — Win11 26100 ERROR_INVALID_PARAMETER (0x80070057) fix.
            //
            // Background: on Windows 11 build 26100 (and some earlier 24H2
            // cumulative updates), `RegisterRawInputDevices` strictly enforces
            // the documented contract that when `RIDEV_INPUTSINK` is set,
            // `hwndTarget` MUST be a valid HWND (not NULL). Older builds
            // silently accepted NULL here; 26100 rejects it with
            // ERROR_INVALID_PARAMETER, killing keyboard+mouse capture for the
            // whole session. A user's debug log on RTX 4060 / Win11 26100
            // showed 5 consecutive failures and 0 successes, producing MP4s
            // with empty input streams — poison for training data.
            //
            // The previous code passed `HWND::default()` here and commented
            // that message-only windows want NULL; that comment was wrong per
            // the MSDN `RAWINPUTDEVICE` docs. We always have a valid message-
            // only `hwnd` from `CreateWindowExA` above, so we pass it.
            //
            // Fallback cascade — we try progressively less-demanding
            // registrations so that at least *some* kbm input flows even if
            // the preferred path is rejected by the OS or a third-party
            // filter driver:
            //
            //   1. RIDEV_INPUTSINK + valid hwnd, both devices in one call
            //      (preferred: global capture regardless of foreground state)
            //   2. RIDEV_INPUTSINK + valid hwnd, registered ONE DEVICE AT A
            //      TIME (works around batch-rejection quirks on some driver
            //      stacks)
            //   3. dwFlags = 0 + valid hwnd, one device at a time
            //      (foreground-only fallback: only delivers input when our
            //      window owns focus, but never NULL-target and never INPUTSINK
            //      — this is the maximally-compatible shape accepted by every
            //      Windows build we care about)
            let preferred_flags = RIDEV_INPUTSINK;
            let foreground_flags = RAWINPUTDEVICE_FLAGS(0);
            let make_devices = |flags: RAWINPUTDEVICE_FLAGS| {
                [
                    0x02, // Mouse
                    0x06, // Keyboard
                ]
                .map(|usage| RAWINPUTDEVICE {
                    usUsagePage: 0x01, // Generic Desktop Controls
                    usUsage: usage,
                    dwFlags: flags,
                    // Per MSDN: when RIDEV_INPUTSINK is set, hwndTarget MUST be
                    // a valid HWND. Win11 26100 enforces this strictly. We use
                    // our message-only window so we receive WM_INPUT even when
                    // not in foreground. For the dwFlags=0 fallback, a valid
                    // HWND is also accepted (and is what Windows wants for
                    // foreground capture).
                    hwndTarget: hwnd,
                })
            };

            // Tier 1: preferred path. Batch register both devices with
            // RIDEV_INPUTSINK and the valid message-only hwnd.
            let tier1 = make_devices(preferred_flags);
            let tier1_result = RegisterRawInputDevices(&tier1, tier1.len() as u32)
                .wrap_err("tier 1: RIDEV_INPUTSINK batch with valid hwnd");
            let registered = if let Err(ref e) = tier1_result {
                tracing::info!(
                    error = ?e,
                    "RegisterRawInputDevices tier 1 (INPUTSINK batch) failed, trying tier 2"
                );
                // Tier 2: same flags, but register devices one at a time.
                // Some filter drivers reject the whole batch if they dislike a
                // single entry; sequential registration lets us succeed for
                // at least one device (the mouse often goes through even when
                // the keyboard does not, or vice versa).
                let mut any_ok = false;
                for dev in &tier1 {
                    let single = [*dev];
                    match RegisterRawInputDevices(&single, 1)
                        .wrap_err("tier 2: RIDEV_INPUTSINK per-device with valid hwnd")
                    {
                        Ok(()) => {
                            tracing::debug!(
                                usage = format!("0x{:02X}", dev.usUsage),
                                "tier 2 succeeded for device"
                            );
                            any_ok = true;
                        }
                        Err(e) => {
                            tracing::info!(
                                error = ?e,
                                usage = format!("0x{:02X}", dev.usUsage),
                                "tier 2 failed for device, will try tier 3"
                            );
                        }
                    }
                }

                if !any_ok {
                    // Tier 3: dwFlags = 0 (foreground-only), one device at a
                    // time. No INPUTSINK means we only receive input when our
                    // hidden message window owns the foreground — which it
                    // normally does not — so coverage will be partial. This
                    // is still strictly better than zero input: the game
                    // overlay and any injected focus windows will pass through
                    // real WM_INPUT events instead of the silent empty stream
                    // we ship today.
                    let tier3 = make_devices(foreground_flags);
                    let mut any_tier3_ok = false;
                    for dev in &tier3 {
                        let single = [*dev];
                        match RegisterRawInputDevices(&single, 1)
                            .wrap_err("tier 3: dwFlags=0 per-device foreground-only")
                        {
                            Ok(()) => {
                                tracing::debug!(
                                    usage = format!("0x{:02X}", dev.usUsage),
                                    "tier 3 succeeded for device (foreground-only)"
                                );
                                any_tier3_ok = true;
                            }
                            Err(e) => {
                                tracing::debug!(
                                    error = ?e,
                                    usage = format!("0x{:02X}", dev.usUsage),
                                    "tier 3 failed for device"
                                );
                            }
                        }
                    }
                    any_tier3_ok
                } else {
                    true
                }
            } else {
                tracing::debug!("RegisterRawInputDevices tier 1 (INPUTSINK batch) succeeded");
                true
            };

            if !registered {
                // All three tiers failed. This is very rare (e.g. session 0 /
                // headless / certain kernel-mode anti-cheat filters). Log and
                // proceed — the capture thread still pumps window messages so
                // the rest of the pipeline (gamepad via XInput, video via OBS)
                // is unaffected. Downstream validators will flag the empty
                // input stream.
                tracing::warn!(
                    "RegisterRawInputDevices failed at all fallback tiers \
                     (preferred INPUTSINK batch, per-device INPUTSINK, and \
                     foreground-only) — continuing without keyboard/mouse \
                     Raw Input. Video recording and gamepad input are \
                     unaffected."
                );
            }

            Ok(Self {
                hwnd,
                class_name,
                h_instance,
                active_keys,
            })
        }
    }

    pub fn run_queue(&mut self, mut event_callback: impl FnMut(Event) -> bool) -> Result<()> {
        unsafe {
            let mut msg = MSG::default();
            let mut last_absolute: Option<(i32, i32)> = None;

            // GetMessageA returns:
            // - 0 if WM_QUIT is received (exit loop)
            // - -1 if an error occurs (handle error)
            // - positive non-zero if a message is retrieved
            // We must check for -1 explicitly; .as_bool() would treat it as true.
            loop {
                let result = GetMessageA(&mut msg, None, 0, 0);
                let result_i32 = result.0;
                if result_i32 == 0 {
                    break; // WM_QUIT received
                }
                if result.0 == -1 {
                    use windows::Win32::Foundation::GetLastError;
                    let error = GetLastError();
                    bail!("GetMessageA failed: {error:?}");
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageA(&msg);

                if msg.message == WindowsAndMessaging::WM_INPUT {
                    // Process each WM_INPUT message individually via GetRawInputData.
                    // NOTE: GetRawInputBuffer batch mode was removed because the
                    // previous implementation had bugs (no size query, wrong stride).
                    // Single-message processing is reliable and sufficient for 1000Hz mice.
                    for event in self.parse_wm_input(msg.lParam, &mut last_absolute) {
                        if !event_callback(event) {
                            return Ok(());
                        }
                    }
                }
            }
            Ok(())
        }
    }

    #[tracing::instrument(skip_all, fields(hwnd = ?hwnd))]
    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: Windows API callback - unsafe required for FFI boundary
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging;
            match msg {
                WindowsAndMessaging::WM_CREATE => {
                    tracing::debug!(msg = "WM_CREATE");
                    LRESULT(0)
                }
                WindowsAndMessaging::WM_DESTROY => {
                    tracing::debug!(msg = "WM_DESTROY");
                    PostQuitMessage(0);
                    LRESULT(0)
                }

                _ => DefWindowProcA(hwnd, msg, wparam, lparam),
            }
        }
    }

    fn active_keys(&self) -> MutexGuard<'_, ActiveKeys> {
        self.active_keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Parse raw input from GetRawInputBuffer batch reading.
    /// Includes message time for latency tracking.
    #[allow(dead_code)]
    fn parse_raw_input(
        &mut self,
        rawinput: &RAWINPUT,
        _msg_time: i32,
        last_absolute: &mut Option<(i32, i32)>,
    ) -> Vec<Event> {
        // Note: _msg_time can be used for latency analysis by comparing
        // with current QPC time. For now, we pass it through for future use.
        // SAFETY: We trust the RAWINPUT data from Windows. Union field access
        // is required because RAWINPUT.data is a union of mouse/keyboard/hid.
        // The dwType field tells us which union variant is valid.
        unsafe {
            match Input::RID_DEVICE_INFO_TYPE(rawinput.header.dwType) {
                Input::RIM_TYPEMOUSE => {
                    let mut events = Vec::new();
                    let mouse = rawinput.data.mouse;
                    let us_flags = mouse.usFlags.0;

                    // Handle mouse movement
                    if mouse.lLastX != 0 || mouse.lLastY != 0 {
                        let (delta_x, delta_y) = if (us_flags & MOUSE_MOVE_ABSOLUTE.0) != 0 {
                            let is_virtual_desktop = (us_flags & MOUSE_VIRTUAL_DESKTOP.0) != 0;
                            let (screen_x, screen_y) = convert_absolute_to_screen_coords(
                                mouse.lLastX,
                                mouse.lLastY,
                                is_virtual_desktop,
                            );
                            let delta = last_absolute
                                .map(|(last_x, last_y)| {
                                    (
                                        screen_x.saturating_sub(last_x),
                                        screen_y.saturating_sub(last_y),
                                    )
                                })
                                .unwrap_or_default();
                            *last_absolute = Some((screen_x, screen_y));
                            delta
                        } else {
                            (mouse.lLastX, mouse.lLastY)
                        };

                        if delta_x != 0 || delta_y != 0 {
                            events.push(Event::MouseMove([delta_x, delta_y]));
                        }
                    }

                    let us_button_flags = u32::from(mouse.Anonymous.Anonymous.usButtonFlags);

                    if us_button_flags & RI_MOUSE_LEFT_BUTTON_DOWN != 0 {
                        events.push(Event::MousePress {
                            key: VK_LBUTTON.0,
                            press_state: PressState::Pressed,
                        });
                        self.active_keys().mouse.insert(VK_LBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_LEFT_BUTTON_UP != 0 {
                        events.push(Event::MousePress {
                            key: VK_LBUTTON.0,
                            press_state: PressState::Released,
                        });
                        self.active_keys().mouse.remove(&VK_LBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_RIGHT_BUTTON_DOWN != 0 {
                        events.push(Event::MousePress {
                            key: VK_RBUTTON.0,
                            press_state: PressState::Pressed,
                        });
                        self.active_keys().mouse.insert(VK_RBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_RIGHT_BUTTON_UP != 0 {
                        events.push(Event::MousePress {
                            key: VK_RBUTTON.0,
                            press_state: PressState::Released,
                        });
                        self.active_keys().mouse.remove(&VK_RBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_MIDDLE_BUTTON_DOWN != 0 {
                        events.push(Event::MousePress {
                            key: VK_MBUTTON.0,
                            press_state: PressState::Pressed,
                        });
                        self.active_keys().mouse.insert(VK_MBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_MIDDLE_BUTTON_UP != 0 {
                        events.push(Event::MousePress {
                            key: VK_MBUTTON.0,
                            press_state: PressState::Released,
                        });
                        self.active_keys().mouse.remove(&VK_MBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_BUTTON_4_DOWN != 0 {
                        events.push(Event::MousePress {
                            key: VK_XBUTTON1.0,
                            press_state: PressState::Pressed,
                        });
                        self.active_keys().mouse.insert(VK_XBUTTON1.0);
                    }
                    if us_button_flags & RI_MOUSE_BUTTON_4_UP != 0 {
                        events.push(Event::MousePress {
                            key: VK_XBUTTON1.0,
                            press_state: PressState::Released,
                        });
                        self.active_keys().mouse.remove(&VK_XBUTTON1.0);
                    }
                    if us_button_flags & RI_MOUSE_BUTTON_5_DOWN != 0 {
                        events.push(Event::MousePress {
                            key: VK_XBUTTON2.0,
                            press_state: PressState::Pressed,
                        });
                        self.active_keys().mouse.insert(VK_XBUTTON2.0);
                    }
                    if us_button_flags & RI_MOUSE_BUTTON_5_UP != 0 {
                        events.push(Event::MousePress {
                            key: VK_XBUTTON2.0,
                            press_state: PressState::Released,
                        });
                        self.active_keys().mouse.remove(&VK_XBUTTON2.0);
                    }

                    if us_button_flags & RI_MOUSE_WHEEL != 0 {
                        events.push(Event::MouseScroll {
                            scroll_amount: mouse.Anonymous.Anonymous.usButtonData as i16,
                        });
                    }

                    events
                }
                Input::RIM_TYPEKEYBOARD => {
                    let keyboard = rawinput.data.keyboard;
                    let key = keyboard.VKey;
                    let flags = u32::from(keyboard.Flags);
                    let press_state = if flags & RI_KEY_BREAK != 0 {
                        PressState::Released
                    } else {
                        PressState::Pressed
                    };
                    if press_state == PressState::Pressed {
                        self.active_keys().keyboard.insert(key);
                    } else {
                        self.active_keys().keyboard.remove(&key);
                    }
                    vec![Event::KeyPress { key, press_state }]
                }
                _ => vec![],
            }
        }
    }

    fn parse_wm_input(
        &mut self,
        lparam: LPARAM,
        last_absolute: &mut Option<(i32, i32)>,
    ) -> Vec<Event> {
        unsafe {
            let hrawinput = HRAWINPUT(lparam.0 as *mut _);
            let header_size = match size_of::<RAWINPUTHEADER>().try_into() {
                Ok(size) => size,
                Err(e) => {
                    tracing::error!("size of RAWINPUTHEADER should fit in u32: {e}");
                    return Vec::new();
                }
            };

            // Query required buffer size first - some devices send larger data
            let mut pcbsize: u32 = 0;
            let size_result =
                GetRawInputData(hrawinput, RID_INPUT, None, &mut pcbsize, header_size);
            if size_result == u32::MAX {
                return Vec::new();
            }

            // Allocate buffer with required size (handles oversized input data)
            let mut buffer: Vec<u8> = vec![0; pcbsize as usize];
            let result = GetRawInputData(
                hrawinput,
                RID_INPUT,
                Some(buffer.as_mut_ptr() as *mut _),
                &mut pcbsize,
                header_size,
            );
            if result == u32::MAX {
                use windows::Win32::Foundation::GetLastError;
                let error = GetLastError();
                tracing::warn!("GetRawInputData failed: {:?}, dropping input event", error);
                return Vec::new();
            }

            let rawinput = &*(buffer.as_ptr() as *const RAWINPUT);
            match Input::RID_DEVICE_INFO_TYPE(rawinput.header.dwType) {
                Input::RIM_TYPEMOUSE => {
                    let mut events = Vec::new();

                    let mouse = rawinput.data.mouse;
                    let us_flags = mouse.usFlags.0;

                    // Handle mouse movement
                    if mouse.lLastX != 0 || mouse.lLastY != 0 {
                        let (delta_x, delta_y) = if (us_flags & MOUSE_MOVE_ABSOLUTE.0) != 0 {
                            // Absolute movement - convert to screen coordinates and calculate delta
                            let is_virtual_desktop = (us_flags & MOUSE_VIRTUAL_DESKTOP.0) != 0;
                            let (screen_x, screen_y) = convert_absolute_to_screen_coords(
                                mouse.lLastX,
                                mouse.lLastY,
                                is_virtual_desktop,
                            );

                            let delta = last_absolute
                                .map(|(last_x, last_y)| {
                                    (
                                        screen_x.saturating_sub(last_x),
                                        screen_y.saturating_sub(last_y),
                                    )
                                })
                                .unwrap_or_default();

                            // Update stored absolute position
                            *last_absolute = Some((screen_x, screen_y));

                            delta
                        } else {
                            // Relative movement - use raw values directly
                            (mouse.lLastX, mouse.lLastY)
                        };

                        if delta_x != 0 || delta_y != 0 {
                            events.push(Event::MouseMove([delta_x, delta_y]));
                        }
                    }

                    let us_button_flags = u32::from(mouse.Anonymous.Anonymous.usButtonFlags);

                    if us_button_flags & RI_MOUSE_LEFT_BUTTON_DOWN != 0 {
                        events.push(Event::MousePress {
                            key: VK_LBUTTON.0,
                            press_state: PressState::Pressed,
                        });
                        self.active_keys().mouse.insert(VK_LBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_LEFT_BUTTON_UP != 0 {
                        events.push(Event::MousePress {
                            key: VK_LBUTTON.0,
                            press_state: PressState::Released,
                        });
                        self.active_keys().mouse.remove(&VK_LBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_RIGHT_BUTTON_DOWN != 0 {
                        events.push(Event::MousePress {
                            key: VK_RBUTTON.0,
                            press_state: PressState::Pressed,
                        });
                        self.active_keys().mouse.insert(VK_RBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_RIGHT_BUTTON_UP != 0 {
                        events.push(Event::MousePress {
                            key: VK_RBUTTON.0,
                            press_state: PressState::Released,
                        });
                        self.active_keys().mouse.remove(&VK_RBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_MIDDLE_BUTTON_DOWN != 0 {
                        events.push(Event::MousePress {
                            key: VK_MBUTTON.0,
                            press_state: PressState::Pressed,
                        });
                        self.active_keys().mouse.insert(VK_MBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_MIDDLE_BUTTON_UP != 0 {
                        events.push(Event::MousePress {
                            key: VK_MBUTTON.0,
                            press_state: PressState::Released,
                        });
                        self.active_keys().mouse.remove(&VK_MBUTTON.0);
                    }
                    if us_button_flags & RI_MOUSE_BUTTON_4_DOWN != 0 {
                        events.push(Event::MousePress {
                            key: VK_XBUTTON1.0,
                            press_state: PressState::Pressed,
                        });
                        self.active_keys().mouse.insert(VK_XBUTTON1.0);
                    }
                    if us_button_flags & RI_MOUSE_BUTTON_4_UP != 0 {
                        events.push(Event::MousePress {
                            key: VK_XBUTTON1.0,
                            press_state: PressState::Released,
                        });
                        self.active_keys().mouse.remove(&VK_XBUTTON1.0);
                    }
                    if us_button_flags & RI_MOUSE_BUTTON_5_DOWN != 0 {
                        events.push(Event::MousePress {
                            key: VK_XBUTTON2.0,
                            press_state: PressState::Pressed,
                        });
                        self.active_keys().mouse.insert(VK_XBUTTON2.0);
                    }
                    if us_button_flags & RI_MOUSE_BUTTON_5_UP != 0 {
                        events.push(Event::MousePress {
                            key: VK_XBUTTON2.0,
                            press_state: PressState::Released,
                        });
                        self.active_keys().mouse.remove(&VK_XBUTTON2.0);
                    }

                    if us_button_flags & RI_MOUSE_WHEEL != 0 {
                        let scroll = mouse.Anonymous.Anonymous.usButtonData as i16;
                        events.push(Event::MouseScroll {
                            scroll_amount: scroll,
                        });
                    }

                    events
                }
                Input::RIM_TYPEKEYBOARD => {
                    let keyboard = rawinput.data.keyboard;
                    let key = keyboard.VKey;
                    let flags = u32::from(keyboard.Flags);
                    let press_state = if flags & RI_KEY_BREAK != 0 {
                        PressState::Released
                    } else {
                        PressState::Pressed
                    };
                    if press_state == PressState::Pressed {
                        // Only emit event if key wasn't already pressed (filters autorepeat)
                        if self.active_keys().keyboard.insert(key) {
                            vec![Event::KeyPress { key, press_state }]
                        } else {
                            vec![] // Key was already pressed (autorepeat), don't record duplicate
                        }
                    } else {
                        self.active_keys().keyboard.remove(&key);
                        vec![Event::KeyPress { key, press_state }]
                    }
                }
                _ => vec![],
            }
        }
    }
}

/// Convert normalized absolute mouse coordinates to screen coordinates
/// Based on Microsoft documentation: coordinates are normalized between 0 and 65535
/// Accounts for virtual desktop if the MOUSE_VIRTUAL_DESKTOP flag is set
fn convert_absolute_to_screen_coords(x: i32, y: i32, is_virtual_desktop: bool) -> (i32, i32) {
    let (left, top, right, bottom) = unsafe {
        if is_virtual_desktop {
            let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            // SM_CXVIRTUALSCREEN/SM_CYVIRTUALSCREEN return width/height, not coordinates
            // Calculate right/bottom by adding width/height to left/top
            (
                left,
                top,
                left.saturating_add(width),
                top.saturating_add(height),
            )
        } else {
            (
                0,
                0,
                GetSystemMetrics(SM_CXSCREEN),
                GetSystemMetrics(SM_CYSCREEN),
            )
        }
    };

    // Convert from normalized coordinates (0-65535) to screen coordinates
    // Using MulDiv equivalent: (x * (right - left)) / 65535 + left
    // Use i64 for intermediate calculations to prevent integer overflow
    let width = (right - left) as i64;
    let height = (bottom - top) as i64;
    let screen_x = (((x as i64 * width) / 65535) + left as i64) as i32;
    let screen_y = (((y as i64 * height) / 65535) + top as i64) as i32;

    (screen_x, screen_y)
}
