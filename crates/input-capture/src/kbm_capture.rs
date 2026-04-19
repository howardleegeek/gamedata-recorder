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
                RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK, RegisterRawInputDevices,
            },
            WindowsAndMessaging::{
                self, CreateWindowExA, DefWindowProcA, DestroyWindow, DispatchMessageA,
                GetMessageA, GetSystemMetrics, HWND_MESSAGE, MSG, PostQuitMessage, RI_KEY_BREAK,
                RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, RI_MOUSE_BUTTON_5_DOWN,
                RI_MOUSE_BUTTON_5_UP, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP,
                RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP, RI_MOUSE_RIGHT_BUTTON_DOWN,
                RI_MOUSE_RIGHT_BUTTON_UP, RI_MOUSE_WHEEL, RegisterClassA, SM_CXSCREEN,
                SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
                SM_YVIRTUALSCREEN, TranslateMessage, UnregisterClassA, WINDOW_EX_STYLE,
                WINDOW_STYLE, WNDCLASSA,
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
    pub fn initialize(
        active_keys: Arc<Mutex<ActiveKeys>>,
        consent: &ConsentGuard,
    ) -> Result<Self> {
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
                WINDOW_EX_STYLE(0),
                class_name,
                PCSTR::null(),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(h_instance),
                None,
            )
            .wrap_err("failed to create window")?;

            tracing::debug!("RawInput window created: {hwnd:?}");

            let raw_input_devices = [
                0x02, // Mouse
                0x06, // Keyboard
            ]
            .map(|usage| RAWINPUTDEVICE {
                usUsagePage: 0x01, // Generic Desktop Controls
                usUsage: usage,
                dwFlags: RIDEV_INPUTSINK, // Receive input even when not in foreground
                // For message-only windows (HWND_MESSAGE parent), hwndTarget should be NULL
                hwndTarget: HWND::default(),
            });

            let device_count = raw_input_devices.len() as u32;
            // v2.5.2: don't panic if RegisterRawInputDevices fails — this is
            // known to return 0x80070057 (ERROR_INVALID_PARAMETER) on certain
            // Windows sessions (session 0 / headless / SSH-launched processes).
            // Session logs from nucbox showed zero keyboard/mouse events for
            // 302 seconds because the whole pipeline died on this call.
            // Log a warning and proceed without Raw Input — the capture thread
            // will still pump window messages and gamepad input will continue
            // to work via XInput. A future fix can wire up SetWindowsHookEx
            // as a proper fallback.
            if let Err(e) = RegisterRawInputDevices(&raw_input_devices, device_count)
                .wrap_err("failed to register raw input devices")
            {
                tracing::warn!(
                    error = ?e,
                    "RegisterRawInputDevices failed — continuing without \
                     keyboard/mouse Raw Input. Video recording and gamepad \
                     input are unaffected."
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
