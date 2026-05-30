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
    cell::RefCell,
    collections::{HashSet, VecDeque},
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
                self, CallNextHookEx, CreateWindowExA, DefWindowProcA, DestroyWindow,
                DispatchMessageA, GetMessageA, GetSystemMetrics, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
                MSG, MSLLHOOKSTRUCT, PostQuitMessage, RI_KEY_BREAK, RI_MOUSE_BUTTON_4_DOWN,
                RI_MOUSE_BUTTON_4_UP, RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP,
                RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP, RI_MOUSE_MIDDLE_BUTTON_DOWN,
                RI_MOUSE_MIDDLE_BUTTON_UP, RI_MOUSE_RIGHT_BUTTON_DOWN, RI_MOUSE_RIGHT_BUTTON_UP,
                RI_MOUSE_WHEEL, RegisterClassA, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN,
                SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE,
                SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx,
                UnregisterClassA, WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE, WINDOW_STYLE,
                WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
                WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
                WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSA, WS_EX_TOOLWINDOW, XBUTTON1,
                XBUTTON2,
            },
        },
    },
    core::PCSTR,
};

use crate::{Event, PressState};

// ---------------------------------------------------------------------------
// Tier-4 fallback state: low-level global hooks (`WH_KEYBOARD_LL` /
// `WH_MOUSE_LL`).
//
// WHY this exists — Win11 26200 Raw Input failure:
//   On the target machine (Windows 11 build 26200, AMD) the recorder logged
//   `RegisterRawInputDevices` failing at *every* fallback tier (INPUTSINK
//   batch, per-device INPUTSINK, and foreground-only dwFlags=0). The resulting
//   `inputs.jsonl` contained only lifecycle markers with empty `keyboard:[]`
//   and `mouse:[]` — zero real input — even though the user demonstrably
//   played (the MC-mod `game_state.jsonl` captured 2.7 MB of pose changes).
//   This points at a restricted window-station / `schtasks`-launched context
//   (or a 26200 tightening) where Raw Input registration is simply rejected
//   for this process. Missing keyboard/mouse makes the data incomplete and
//   useless for world-model training, so we need a path that does NOT depend
//   on Raw Input registration at all.
//
// HOW the hook proc talks to `run_queue`:
//   `SetWindowsHookEx(WH_*_LL, ..)` takes a bare `extern "system" fn` that the
//   OS invokes *on the thread that installed it, while that thread pumps
//   messages*. That callback cannot capture `self`, the user `event_callback`,
//   or the mouse-delta state. So we bridge through THREAD-LOCAL state: the
//   hook procs decode `KBDLLHOOKSTRUCT` / `MSLLHOOKSTRUCT` into the exact same
//   `Event` shape the WM_INPUT path produces, push them onto a thread-local
//   queue, and `run_queue` drains that queue every pump iteration and feeds
//   the SAME `event_callback`. Because the hooks fire on the very thread that
//   runs the pump, a thread-local is correct (and lock-free) here.
// ---------------------------------------------------------------------------

thread_local! {
    /// Decoded events produced by the LL hook procedures, awaiting drain by
    /// [`KbmCapture::run_queue`]. Only touched on the capture thread, which is
    /// also the thread the hooks are installed on and fire on.
    static LL_HOOK_EVENTS: RefCell<VecDeque<Event>> = const { RefCell::new(VecDeque::new()) };

    /// Shared active-key set, mirrored from the owning [`KbmCapture`] so the LL
    /// hook procs can perform the same autorepeat filtering and pressed-set
    /// bookkeeping the WM_INPUT path does. `None` until the fallback installs.
    static LL_HOOK_ACTIVE_KEYS: RefCell<Option<Arc<Mutex<ActiveKeys>>>> =
        const { RefCell::new(None) };

    /// Last absolute mouse position seen by the mouse LL hook, used to derive
    /// relative deltas (the WM_INPUT path emits relative `MouseMove` deltas, so
    /// we must too). `WH_MOUSE_LL` reports absolute screen coordinates in
    /// `MSLLHOOKSTRUCT::pt`.
    static LL_HOOK_LAST_MOUSE: RefCell<Option<(i32, i32)>> = const { RefCell::new(None) };
}

/// Lock the LL-hook active-key set the same poisoning-tolerant way the rest of
/// this module does. Returns `None` if the fallback was never installed.
fn ll_hook_with_active_keys<R>(f: impl FnOnce(&mut ActiveKeys) -> R) -> Option<R> {
    LL_HOOK_ACTIVE_KEYS.with(|cell| {
        cell.borrow().as_ref().map(|arc| {
            let mut guard = arc.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            f(&mut guard)
        })
    })
}

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
    /// Tier-4 fallback handles. `Some` only when all three Raw Input tiers
    /// failed and we installed `WH_KEYBOARD_LL` / `WH_MOUSE_LL` instead (see
    /// the module-level rationale on the Win11 26200 Raw Input failure). These
    /// MUST be unhooked in `Drop`, on the same thread that installed them.
    keyboard_hook: Option<HHOOK>,
    mouse_hook: Option<HHOOK>,
}
impl Drop for KbmCapture {
    fn drop(&mut self) {
        unsafe {
            // Tier-4 teardown: unhook the low-level hooks before tearing the
            // window down. `UnhookWindowsHookEx` must run on the thread that
            // called `SetWindowsHookEx`; `Drop` runs on the capture thread
            // (the one that owned `run_queue`), which is exactly that thread.
            // Mirrors the DestroyWindow cleanup below.
            if let Some(hook) = self.keyboard_hook.take() {
                if let Err(e) = UnhookWindowsHookEx(hook) {
                    tracing::error!("Failed to unhook WH_KEYBOARD_LL during cleanup: {:?}", e);
                }
            }
            if let Some(hook) = self.mouse_hook.take() {
                if let Err(e) = UnhookWindowsHookEx(hook) {
                    tracing::error!("Failed to unhook WH_MOUSE_LL during cleanup: {:?}", e);
                }
            }
            // Drop our reference to the shared active-key set from the hook
            // thread-local so the `Arc` can be released and the next capture
            // session starts clean.
            LL_HOOK_ACTIVE_KEYS.with(|cell| *cell.borrow_mut() = None);

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
                // Capture the concrete Win32 error code. The `windows` crate's
                // `Result` Display can be empty when the failure originated from
                // a `BOOL`->error conversion, so we read `GetLastError()`
                // ourselves and log the raw code (e.g. 0x80070057 =
                // ERROR_INVALID_PARAMETER, or 0x5 = ERROR_ACCESS_DENIED on a
                // restricted window station) so future diagnosis is possible.
                use windows::Win32::Foundation::GetLastError;
                let last_error = GetLastError();
                tracing::info!(
                    error = ?e,
                    last_error = ?last_error,
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
                            use windows::Win32::Foundation::GetLastError;
                            let last_error = GetLastError();
                            tracing::info!(
                                error = ?e,
                                last_error = ?last_error,
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
                                use windows::Win32::Foundation::GetLastError;
                                let last_error = GetLastError();
                                tracing::debug!(
                                    error = ?e,
                                    last_error = ?last_error,
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

            // Tier-4 fallback hook handles. Populated below only when Raw Input
            // registration failed at every tier and we successfully install the
            // low-level global hooks. Plumbed into the constructor so `Drop` can
            // unhook them on this same (capture) thread.
            let mut keyboard_hook: Option<HHOOK> = None;
            let mut mouse_hook: Option<HHOOK> = None;

            if !registered {
                // All three Raw Input tiers failed (e.g. Win11 26200 restricted
                // window station / schtasks context — see the module-level
                // rationale). Rather than ship an empty input stream, fall back
                // to TIER 4: low-level global hooks (`WH_KEYBOARD_LL` /
                // `WH_MOUSE_LL`). These do not depend on RegisterRawInputDevices
                // at all; the OS calls our hook procs on THIS thread while it
                // pumps messages in `run_queue`, and they push decoded events
                // onto a thread-local queue we drain there.
                tracing::warn!(
                    "RegisterRawInputDevices failed at all fallback tiers \
                     (preferred INPUTSINK batch, per-device INPUTSINK, and \
                     foreground-only) — installing tier-4 low-level hook \
                     fallback (WH_KEYBOARD_LL / WH_MOUSE_LL) to capture \
                     keyboard/mouse without Raw Input. Video recording and \
                     gamepad input are unaffected."
                );

                // Share the active-key set with the hook procs so they can
                // perform the same autorepeat suppression and pressed-set
                // bookkeeping the WM_INPUT path does. Must be set BEFORE the
                // hooks are installed, since a hook can fire immediately.
                LL_HOOK_ACTIVE_KEYS.with(|cell| {
                    *cell.borrow_mut() = Some(active_keys.clone());
                });

                use windows::Win32::Foundation::GetLastError;

                // WH_KEYBOARD_LL: global low-level keyboard hook. dwThreadId = 0
                // installs it for the whole desktop on the calling thread, which
                // is exactly the thread that runs `run_queue`'s message pump
                // (required for low-level hooks to be serviced).
                match SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(Self::keyboard_ll_proc),
                    Some(h_instance),
                    0,
                ) {
                    Ok(hook) => {
                        tracing::info!("tier-4 fallback: WH_KEYBOARD_LL installed");
                        keyboard_hook = Some(hook);
                    }
                    Err(e) => {
                        let last_error = GetLastError();
                        tracing::error!(
                            error = ?e,
                            last_error = ?last_error,
                            "tier-4 fallback: failed to install WH_KEYBOARD_LL"
                        );
                    }
                }

                // WH_MOUSE_LL: global low-level mouse hook, same threading rules.
                match SetWindowsHookExW(WH_MOUSE_LL, Some(Self::mouse_ll_proc), Some(h_instance), 0)
                {
                    Ok(hook) => {
                        tracing::info!("tier-4 fallback: WH_MOUSE_LL installed");
                        mouse_hook = Some(hook);
                    }
                    Err(e) => {
                        let last_error = GetLastError();
                        tracing::error!(
                            error = ?e,
                            last_error = ?last_error,
                            "tier-4 fallback: failed to install WH_MOUSE_LL"
                        );
                    }
                }

                // If neither hook installed, drop our shared reference to the
                // active-key set so it does not linger in the thread-local.
                if keyboard_hook.is_none() && mouse_hook.is_none() {
                    LL_HOOK_ACTIVE_KEYS.with(|cell| *cell.borrow_mut() = None);
                    tracing::error!(
                        "tier-4 fallback: both WH_KEYBOARD_LL and WH_MOUSE_LL \
                         failed to install — continuing without keyboard/mouse. \
                         Downstream validators will flag the empty input stream."
                    );
                }
            }

            Ok(Self {
                hwnd,
                class_name,
                h_instance,
                active_keys,
                keyboard_hook,
                mouse_hook,
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

                // Tier-4 fallback drain. When the LL hooks are installed (all
                // Raw Input tiers failed), the OS invokes `keyboard_ll_proc` /
                // `mouse_ll_proc` ON THIS THREAD during the message dispatch
                // above, and they enqueue decoded events onto the thread-local
                // `LL_HOOK_EVENTS`. Drain them here and feed the SAME
                // `event_callback` the WM_INPUT path uses, so downstream sees an
                // identical event stream regardless of which capture tier won.
                // This is a no-op (empty queue) on the Raw Input happy path.
                loop {
                    let next = LL_HOOK_EVENTS.with(|q| q.borrow_mut().pop_front());
                    match next {
                        Some(event) => {
                            if !event_callback(event) {
                                return Ok(());
                            }
                        }
                        None => break,
                    }
                }

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

    /// Tier-4 fallback: low-level keyboard hook procedure (`WH_KEYBOARD_LL`).
    ///
    /// Installed only when every `RegisterRawInputDevices` tier failed (see the
    /// module-level Win11 26200 rationale). The OS invokes this on the capture
    /// thread while it pumps messages, so it cannot capture `self` or the user
    /// callback — instead it decodes into the EXACT same [`Event`] shape the
    /// WM_INPUT path (`parse_wm_input`, `RIM_TYPEKEYBOARD`) produces and pushes
    /// onto the thread-local [`LL_HOOK_EVENTS`] queue, which `run_queue` drains.
    ///
    /// Mirroring contract with `parse_wm_input`:
    /// * `key` is the virtual-key code as `u16` (`parse_wm_input` reads
    ///   `RAWKEYBOARD::VKey`, a `u16`; here we narrow `KBDLLHOOKSTRUCT::vkCode`,
    ///   a `u32`, to `u16`).
    /// * Press vs. release is decided by the message id in `wparam`
    ///   (`WM_KEYDOWN`/`WM_SYSKEYDOWN` => pressed, `WM_KEYUP`/`WM_SYSKEYUP` =>
    ///   released), matching the RI_KEY_BREAK split.
    /// * On press we emit ONLY if the key was not already in the active set
    ///   (`HashSet::insert` returns `true`), reproducing the autorepeat
    ///   suppression. On release we always emit and remove from the set.
    unsafe extern "system" fn keyboard_ll_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: Windows hook callback — unsafe required for the FFI boundary
        // and for dereferencing the OS-provided KBDLLHOOKSTRUCT pointer.
        unsafe {
            // Per MSDN, only process the event when code == HC_ACTION; for any
            // negative code we MUST pass it straight to the next hook untouched.
            if code == HC_ACTION as i32 {
                let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
                let key = kb.vkCode as u16;
                let message = wparam.0 as u32;
                // WM_KEYDOWN/WM_SYSKEYDOWN => pressed, *_UP => released. This is
                // the LL-hook equivalent of parse_wm_input's RI_KEY_BREAK split.
                let press_state = match message {
                    WM_KEYDOWN | WM_SYSKEYDOWN => PressState::Pressed,
                    WM_KEYUP | WM_SYSKEYUP => PressState::Released,
                    // Not a key transition we model; defer to the next hook.
                    _ => return CallNextHookEx(None, code, wparam, lparam),
                };

                match press_state {
                    PressState::Pressed => {
                        // `insert` returns true only if the key was NOT already
                        // pressed — exactly parse_wm_input's autorepeat filter.
                        let newly_pressed = ll_hook_with_active_keys(|active_keys| {
                            active_keys.keyboard.insert(key)
                        })
                        // If the active-key set was never installed we cannot
                        // dedupe; default to emitting (true) so input is not
                        // silently dropped. In practice the set is always set
                        // alongside the hooks, so this is belt-and-suspenders.
                        .unwrap_or(true);
                        if newly_pressed {
                            LL_HOOK_EVENTS.with(|q| {
                                q.borrow_mut()
                                    .push_back(Event::KeyPress { key, press_state });
                            });
                        }
                    }
                    PressState::Released => {
                        ll_hook_with_active_keys(|active_keys| {
                            active_keys.keyboard.remove(&key);
                        });
                        LL_HOOK_EVENTS.with(|q| {
                            q.borrow_mut()
                                .push_back(Event::KeyPress { key, press_state });
                        });
                    }
                }
            }

            // ALWAYS chain to the next hook in the chain (MSDN requirement).
            CallNextHookEx(None, code, wparam, lparam)
        }
    }

    /// Tier-4 fallback: low-level mouse hook procedure (`WH_MOUSE_LL`).
    ///
    /// Companion to [`keyboard_ll_proc`]; same threading and queue contract.
    /// Decodes `MSLLHOOKSTRUCT` into the EXACT same [`Event`] shapes the
    /// WM_INPUT path (`parse_wm_input`, `RIM_TYPEMOUSE`) produces:
    ///
    /// * **Movement** — `parse_wm_input` emits a RELATIVE `MouseMove([dx, dy])`.
    ///   `WH_MOUSE_LL` only reports ABSOLUTE screen coordinates in
    ///   `MSLLHOOKSTRUCT::pt`, so we derive the delta from the previous absolute
    ///   position held in [`LL_HOOK_LAST_MOUSE`] (saturating, like the absolute
    ///   branch of `parse_wm_input`) and update it afterwards. We push only when
    ///   the delta is non-zero, matching `parse_wm_input`.
    /// * **Buttons** — `Event::MousePress { key, press_state }` with the SAME
    ///   `VK_*BUTTON` codes `parse_wm_input` uses (`VK_LBUTTON.0`, etc.), plus
    ///   the same `active_keys.mouse` insert/remove bookkeeping. The X button is
    ///   selected from the high word of `mouseData` (XBUTTON1 => VK_XBUTTON1,
    ///   XBUTTON2 => VK_XBUTTON2), mirroring RI_MOUSE_BUTTON_4/5.
    /// * **Wheel** — `Event::MouseScroll { scroll_amount }`. `parse_wm_input`
    ///   uses `usButtonData as i16` (signed WHEEL_DELTA multiples, ±120). For
    ///   `WH_MOUSE_LL` the wheel delta is the high word of `mouseData`, read as a
    ///   signed `i16`, preserving the identical encoding.
    unsafe extern "system" fn mouse_ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: Windows hook callback — unsafe for the FFI boundary and for
        // dereferencing the OS-provided MSLLHOOKSTRUCT pointer.
        unsafe {
            if code == HC_ACTION as i32 {
                let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
                let message = wparam.0 as u32;

                match message {
                    WM_MOUSEMOVE => {
                        // Relative delta from the last absolute position, then
                        // update the stored position. Mirrors the absolute
                        // branch of parse_wm_input (saturating_sub, push only on
                        // non-zero delta).
                        let (cur_x, cur_y) = (ms.pt.x, ms.pt.y);
                        let delta = LL_HOOK_LAST_MOUSE.with(|cell| {
                            let last = *cell.borrow();
                            *cell.borrow_mut() = Some((cur_x, cur_y));
                            last.map(|(lx, ly)| {
                                (cur_x.saturating_sub(lx), cur_y.saturating_sub(ly))
                            })
                        });
                        if let Some((dx, dy)) = delta
                            && (dx != 0 || dy != 0)
                        {
                            LL_HOOK_EVENTS.with(|q| {
                                q.borrow_mut().push_back(Event::MouseMove([dx, dy]));
                            });
                        }
                    }
                    WM_LBUTTONDOWN => Self::ll_push_mouse_button(VK_LBUTTON.0, PressState::Pressed),
                    WM_LBUTTONUP => Self::ll_push_mouse_button(VK_LBUTTON.0, PressState::Released),
                    WM_RBUTTONDOWN => Self::ll_push_mouse_button(VK_RBUTTON.0, PressState::Pressed),
                    WM_RBUTTONUP => Self::ll_push_mouse_button(VK_RBUTTON.0, PressState::Released),
                    WM_MBUTTONDOWN => Self::ll_push_mouse_button(VK_MBUTTON.0, PressState::Pressed),
                    WM_MBUTTONUP => Self::ll_push_mouse_button(VK_MBUTTON.0, PressState::Released),
                    WM_XBUTTONDOWN | WM_XBUTTONUP => {
                        // Which X button is in the HIGH word of mouseData.
                        // parse_wm_input maps RI_MOUSE_BUTTON_4 => VK_XBUTTON1
                        // and BUTTON_5 => VK_XBUTTON2; do the same here.
                        let xbutton = (ms.mouseData >> 16) as u16;
                        let key = if xbutton == XBUTTON1 {
                            VK_XBUTTON1.0
                        } else if xbutton == XBUTTON2 {
                            VK_XBUTTON2.0
                        } else {
                            // Unknown X button; chain on without emitting.
                            return CallNextHookEx(None, code, wparam, lparam);
                        };
                        let press_state = if message == WM_XBUTTONDOWN {
                            PressState::Pressed
                        } else {
                            PressState::Released
                        };
                        Self::ll_push_mouse_button(key, press_state);
                    }
                    WM_MOUSEWHEEL => {
                        // High word of mouseData is the signed wheel delta
                        // (±WHEEL_DELTA). parse_wm_input stores usButtonData as
                        // i16, so the same ±120 values flow through unchanged.
                        let scroll_amount = (ms.mouseData >> 16) as i16;
                        LL_HOOK_EVENTS.with(|q| {
                            q.borrow_mut()
                                .push_back(Event::MouseScroll { scroll_amount });
                        });
                    }
                    _ => {}
                }
            }

            CallNextHookEx(None, code, wparam, lparam)
        }
    }

    /// Push a mouse button press/release Event and update the shared active-key
    /// set the same way `parse_wm_input` does. Factored out so the LL mouse hook
    /// stays readable; called only from `mouse_ll_proc` on the capture thread.
    fn ll_push_mouse_button(key: u16, press_state: PressState) {
        match press_state {
            PressState::Pressed => {
                ll_hook_with_active_keys(|active_keys| {
                    active_keys.mouse.insert(key);
                });
            }
            PressState::Released => {
                ll_hook_with_active_keys(|active_keys| {
                    active_keys.mouse.remove(&key);
                });
            }
        }
        LL_HOOK_EVENTS.with(|q| {
            q.borrow_mut()
                .push_back(Event::MousePress { key, press_state });
        });
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
