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
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
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
                DispatchMessageA, GetMessageA, GetSystemMetrics, HHOOK, KBDLLHOOKSTRUCT, MSG,
                PostQuitMessage, RI_KEY_BREAK, RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP,
                RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP, RI_MOUSE_LEFT_BUTTON_DOWN,
                RI_MOUSE_LEFT_BUTTON_UP, RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP,
                RI_MOUSE_RIGHT_BUTTON_DOWN, RI_MOUSE_RIGHT_BUTTON_UP, RI_MOUSE_WHEEL,
                RegisterClassA, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
                SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SetWindowsHookExW, ShowWindow,
                TranslateMessage, UnhookWindowsHookEx, UnregisterClassA, WH_KEYBOARD_LL,
                WINDOW_EX_STYLE, WINDOW_STYLE, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
                WNDCLASSA, WS_EX_TOOLWINDOW,
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

/// Which `RegisterRawInputDevices` call shape actually succeeded.
///
/// rc16.4: this is reported in `metadata.json` so we can diagnose
/// "HOOK_START but zero input events" in the field. The Win11 26100
/// fallback cascade in [`KbmCapture::initialize`] tries the preferred
/// path first and falls back to per-device registration when batch
/// registration is rejected by a filter driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationTier {
    /// Tier 1: `RIDEV_INPUTSINK` batch with valid hwnd. Both devices
    /// registered in a single call. Preferred — gives global capture.
    InputSinkBatch,
    /// Tier 2: `RIDEV_INPUTSINK` with per-device calls. At least one of
    /// (mouse, keyboard) succeeded. Global capture for the device(s)
    /// that went through.
    InputSinkPerDevice,
    /// Tier 3: `SetWindowsHookExW(WH_KEYBOARD_LL, ...)` low-level
    /// keyboard hook. Used when both INPUTSINK tiers fail (AMD GPU
    /// systems, Ryzen 7 7840HS / Radeon 780M, certain filter drivers).
    /// Keyboard-only — the hook does not capture mouse events; mouse
    /// is silently dropped in this tier. Better than no input than at
    /// all, and Howard's input watchdog needs the keyCode stream alive
    /// for action_camera.json to be meaningful.
    Hook,
    /// No tier succeeded. Raw Input capture is DEAD for the session —
    /// the message pump will spin but never receive `WM_INPUT`.
    None,
}

/// Shared metrics for the running capture thread. Owned by [`KbmCapture`]
/// and exposed through the parent `InputCapture` so the host can:
///
/// * surface "zero events captured" in metadata.json (rc16.4 — bingd's
///   case where HOOK_START fired but `inputs.jsonl` had zero raw input
///   events), and
/// * periodically log capture activity for live diagnosis.
#[derive(Debug)]
pub struct CaptureMetrics {
    /// Total raw-input events observed since [`KbmCapture::initialize`]
    /// returned, including events that produced no [`Event`] (e.g. mouse
    /// move with zero delta). Counts WM_INPUT dispatches, not bytes.
    pub wm_input_total: AtomicU64,
    /// Total `GetRawInputData` failures. A non-zero value here with a
    /// zero `wm_input_total` would indicate a different bug class than
    /// "no hook installed".
    pub get_raw_input_data_failures: AtomicU64,
    /// Which registration tier succeeded. Stored as `usize` so we can
    /// publish it atomically; decoded via [`RegistrationTier::from_raw`].
    pub registration_tier: AtomicUsize,
}

impl Default for CaptureMetrics {
    fn default() -> Self {
        Self {
            wm_input_total: AtomicU64::new(0),
            get_raw_input_data_failures: AtomicU64::new(0),
            registration_tier: AtomicUsize::new(RegistrationTier::None.as_raw()),
        }
    }
}

impl CaptureMetrics {
    /// Read the current registration tier.
    pub fn tier(&self) -> RegistrationTier {
        RegistrationTier::from_raw(self.registration_tier.load(Ordering::Relaxed))
    }
}

impl RegistrationTier {
    const fn as_raw(self) -> usize {
        match self {
            RegistrationTier::InputSinkBatch => 1,
            RegistrationTier::InputSinkPerDevice => 2,
            RegistrationTier::Hook => 3,
            RegistrationTier::None => 0,
        }
    }

    fn from_raw(raw: usize) -> Self {
        match raw {
            1 => RegistrationTier::InputSinkBatch,
            2 => RegistrationTier::InputSinkPerDevice,
            3 => RegistrationTier::Hook,
            _ => RegistrationTier::None,
        }
    }

    /// Stable string identifier for logs and metadata.json. Must remain
    /// stable across versions so the data team can group recordings by
    /// tier without parsing free-form text.
    pub fn as_str(self) -> &'static str {
        match self {
            RegistrationTier::InputSinkBatch => "inputsink_batch",
            RegistrationTier::InputSinkPerDevice => "inputsink_per_device",
            RegistrationTier::Hook => "hook",
            RegistrationTier::None => "none",
        }
    }
}

/// Sender end of the LL hook → message-pump channel.
///
/// `SetWindowsHookExW(WH_KEYBOARD_LL, ...)` requires a `unsafe extern
/// "system" fn` callback — a free function, not a closure. So the
/// callback can't capture any state. We park a `SyncSender<Event>` in
/// a process-global slot and reach for it from inside the callback.
///
/// Single-channel design: the recorder only ever runs one `KbmCapture`
/// per process (the recording subsystem is a single-instance pipeline).
/// If a second `KbmCapture` were ever spawned, the static would be
/// overwritten and events would route to the newer instance — but the
/// `OnceLock` is never reset, so we'd be unable to install a hook for
/// the second capture. Document this constraint here so future
/// multi-recorder refactors know to break the assumption.
static HOOK_EVENT_TX: OnceLock<Mutex<Option<SyncSender<Event>>>> = OnceLock::new();

/// Active key set shared with the LL hook so the hook can update the
/// same data structure that downstream consumers (`active_input()`)
/// observe. Without this, the host's "currently held" snapshot would
/// be stale on hook-fallback systems.
static HOOK_ACTIVE_KEYS: OnceLock<Mutex<Option<Arc<Mutex<ActiveKeys>>>>> = OnceLock::new();

/// Counter shared with the LL hook, parallel to `wm_input_total` for
/// the RawInput path. Bumped on every keyboard event the hook saw,
/// regardless of whether the channel send succeeded — this is the
/// "did the hook fire at all?" signal that distinguishes "hook
/// installed but no keys pressed" from "hook silently dropped".
static HOOK_METRICS: OnceLock<Mutex<Option<Arc<CaptureMetrics>>>> = OnceLock::new();

/// Low-level keyboard hook callback. Invoked by Windows on the thread
/// that installed the hook (our message-pump thread) for every system-
/// wide keyboard event.
///
/// Safety: the function signature is dictated by Win32 (`HOOKPROC`).
/// `code < 0` means "do not process; pass straight through" per MSDN;
/// we honor that. For `code >= 0` we examine `wparam` for the event
/// type and `lparam` for the `KBDLLHOOKSTRUCT` containing the VK code.
///
/// Return value: we must call `CallNextHookEx` to let other hooks in
/// the chain run. Returning a non-zero value would BLOCK the keystroke
/// from reaching the active application — that would be terrible (the
/// user couldn't actually play the game), so we always forward.
unsafe extern "system" fn keyboard_ll_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: Win32 callback boundary. All unsafe blocks below have
    // their preconditions documented.
    unsafe {
        if code < 0 {
            // MSDN: if code < 0, we MUST chain through without doing
            // any work. Skipping CallNextHookEx is a documented bug.
            return CallNextHookEx(None, code, wparam, lparam);
        }

        // wparam is one of WM_KEYDOWN / WM_KEYUP / WM_SYSKEYDOWN /
        // WM_SYSKEYUP. WM_SYSKEY* fires for Alt-modified keys. We
        // treat all four uniformly: down=Pressed, up=Released.
        let press_state = match wparam.0 as u32 {
            WM_KEYDOWN | WM_SYSKEYDOWN => PressState::Pressed,
            WM_KEYUP | WM_SYSKEYUP => PressState::Released,
            _ => {
                // Unknown event in this hook type — shouldn't happen,
                // but chain through if it does.
                return CallNextHookEx(None, code, wparam, lparam);
            }
        };

        // SAFETY: per MSDN, when wparam ∈ {WM_KEYDOWN, WM_KEYUP,
        // WM_SYSKEYDOWN, WM_SYSKEYUP}, lparam is a pointer to a
        // KBDLLHOOKSTRUCT that lives at least until the callback
        // returns. Read but do not retain the pointer.
        let kbd = lparam.0 as *const KBDLLHOOKSTRUCT;
        if kbd.is_null() {
            // Defensive — never seen in the wild, but a null lparam
            // here would be a Windows bug. Chain through.
            return CallNextHookEx(None, code, wparam, lparam);
        }
        let vk = (*kbd).vkCode as u16;

        // Bump the event-observed counter regardless of downstream
        // send success — this is what the host crate inspects when
        // diagnosing "registration_tier=hook but inputs.jsonl empty".
        if let Some(slot) = HOOK_METRICS.get()
            && let Ok(guard) = slot.lock()
            && let Some(metrics) = guard.as_ref()
        {
            metrics.wm_input_total.fetch_add(1, Ordering::Relaxed);
        }

        // Update the shared active-keys set so consumers reading via
        // `active_input()` see the same currently-held key set as on
        // the RawInput path. Autorepeat filtering: only forward the
        // event if it changes the held set (matches RawInput logic).
        let forward = {
            if let Some(slot) = HOOK_ACTIVE_KEYS.get()
                && let Ok(guard) = slot.lock()
                && let Some(active_keys_arc) = guard.as_ref()
            {
                let mut ak = active_keys_arc
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                match press_state {
                    PressState::Pressed => ak.keyboard.insert(vk),
                    PressState::Released => {
                        ak.keyboard.remove(&vk);
                        true
                    }
                }
            } else {
                true
            }
        };

        if forward
            && let Some(slot) = HOOK_EVENT_TX.get()
            && let Ok(guard) = slot.lock()
            && let Some(tx) = guard.as_ref()
        {
            // Use try_send: the message-pump thread is right here and
            // will drain the channel between GetMessage iterations.
            // If the channel is full (10k events buffered) we'd rather
            // drop one keystroke than block the OS keyboard pipeline
            // — blocking here would cause input lag for the entire
            // desktop.
            let _ = tx.try_send(Event::KeyPress {
                key: vk,
                press_state,
            });
        }

        CallNextHookEx(None, code, wparam, lparam)
    }
}

pub struct KbmCapture {
    hwnd: HWND,
    class_name: PCSTR,
    h_instance: HINSTANCE,
    active_keys: Arc<Mutex<ActiveKeys>>,
    metrics: Arc<CaptureMetrics>,
    /// Optional LL keyboard hook handle. `Some` when the
    /// `RegistrationTier::Hook` fallback path was taken (both
    /// INPUTSINK tiers failed). `None` for the preferred RawInput
    /// paths.
    hook: Option<HHOOK>,
    /// Receiver end of the LL hook channel. The hook callback pushes
    /// events into the matching `SyncSender` (stored in the global
    /// `HOOK_EVENT_TX`) and `run_queue` drains via `try_recv` between
    /// GetMessage iterations. `None` when the hook fallback wasn't
    /// taken — `run_queue` skips draining in that case.
    hook_rx: Option<Receiver<Event>>,
}
impl Drop for KbmCapture {
    fn drop(&mut self) {
        unsafe {
            // Unhook FIRST — once the hook is uninstalled, the
            // callback can no longer fire and reach into HOOK_EVENT_TX.
            // Doing this before dropping the sender avoids a race where
            // the OS fires the hook between our drop of the sender and
            // the actual UnhookWindowsHookEx call.
            if let Some(hook) = self.hook.take()
                && let Err(e) = UnhookWindowsHookEx(hook)
            {
                tracing::error!("Failed to unhook WH_KEYBOARD_LL: {:?}", e);
            }
            // Drop the sender side so future hook firings (after
            // unhook but before the OS fully tears down) silently
            // no-op. We rebuild the OnceLock-internal Mutex slot to
            // None so a second KbmCapture (e.g. after a recorder
            // restart) can install a fresh sender.
            if let Some(slot) = HOOK_EVENT_TX.get()
                && let Ok(mut g) = slot.lock()
            {
                *g = None;
            }
            if let Some(slot) = HOOK_ACTIVE_KEYS.get()
                && let Ok(mut g) = slot.lock()
            {
                *g = None;
            }
            if let Some(slot) = HOOK_METRICS.get()
                && let Ok(mut g) = slot.lock()
            {
                *g = None;
            }

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
    ///
    /// `metrics` is shared with the parent [`super::InputCapture`] so the
    /// recorder can observe the registration tier and event counters in
    /// `metadata.json` (rc16.4).
    pub fn initialize(
        active_keys: Arc<Mutex<ActiveKeys>>,
        metrics: Arc<CaptureMetrics>,
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
            // rc16.4 — Fallback cascade revisited.
            //
            // The previous cascade had THREE tiers, with tier 3 falling back
            // to `dwFlags = 0` (foreground-only). That tier is broken-by-
            // design for this codebase: our hwnd is a hidden, zero-size,
            // tool-style message-only window — it *never* owns the foreground
            // while a game is running. So tier-3 "success" was indistinguish-
            // able from "no hook installed at all" from the user's POV: zero
            // events delivered for the entire session, but no error logged.
            // Bingd's rc15.x report ("HOOK_START fires, inputs.jsonl has zero
            // keyboard/mouse events after 5 minutes of Minecraft") matches
            // this failure mode exactly: OBS hooked the game and emitted the
            // lifecycle events through the same channel, but Raw Input
            // delivered nothing because the foreground-only registration
            // never met its precondition.
            //
            // The remaining cascade:
            //
            //   1. RIDEV_INPUTSINK + valid hwnd, both devices in one call
            //      (preferred: global capture regardless of foreground state)
            //   2. RIDEV_INPUTSINK + valid hwnd, registered ONE DEVICE AT A
            //      TIME (works around batch-rejection quirks on some driver
            //      stacks; the mouse and keyboard succeed independently)
            //
            // If both tiers fail, we report it loudly and the host crate
            // surfaces the failure in `metadata.json` so the data team sees
            // "registration_tier=none" instead of a silent empty stream.
            let preferred_flags = RIDEV_INPUTSINK;
            let make_devices = |flags: RAWINPUTDEVICE_FLAGS| {
                [
                    0x02, // Mouse
                    0x06, // Keyboard
                ]
                .map(|usage| RAWINPUTDEVICE {
                    usUsagePage: 0x01, // Generic Desktop Controls
                    usUsage: usage,
                    dwFlags: flags,
                    // Per MSDN: when RIDEV_INPUTSINK is set, hwndTarget MUST
                    // be a valid HWND. Win11 26100 enforces this strictly.
                    // We use our message-only window so we receive WM_INPUT
                    // even when not in foreground.
                    hwndTarget: hwnd,
                })
            };

            // Tier 1: preferred path. Batch register both devices with
            // RIDEV_INPUTSINK and the valid message-only hwnd.
            let tier1 = make_devices(preferred_flags);
            let tier1_result = RegisterRawInputDevices(&tier1, tier1.len() as u32)
                .wrap_err("tier 1: RIDEV_INPUTSINK batch with valid hwnd");
            let tier = if tier1_result.is_ok() {
                tracing::info!(
                    tier = RegistrationTier::InputSinkBatch.as_str(),
                    "RegisterRawInputDevices tier 1 (INPUTSINK batch) succeeded — global Raw Input capture armed"
                );
                RegistrationTier::InputSinkBatch
            } else {
                tracing::info!(
                    error = ?tier1_result.err(),
                    "RegisterRawInputDevices tier 1 (INPUTSINK batch) failed, trying tier 2 (per-device)"
                );
                // Tier 2: same flags, but register devices one at a time.
                // Some filter drivers reject the whole batch if they dislike
                // a single entry; sequential registration lets us succeed for
                // at least one device (the mouse often goes through even when
                // the keyboard does not, or vice versa).
                let mut any_ok = false;
                for dev in &tier1 {
                    let single = [*dev];
                    match RegisterRawInputDevices(&single, 1)
                        .wrap_err("tier 2: RIDEV_INPUTSINK per-device with valid hwnd")
                    {
                        Ok(()) => {
                            tracing::info!(
                                usage = format!("0x{:02X}", dev.usUsage),
                                "tier 2 succeeded for device (INPUTSINK per-device)"
                            );
                            any_ok = true;
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = ?e,
                                usage = format!("0x{:02X}", dev.usUsage),
                                "tier 2 failed for device"
                            );
                        }
                    }
                }

                if any_ok {
                    RegistrationTier::InputSinkPerDevice
                } else {
                    RegistrationTier::None
                }
            };

            // Tier 3: WH_KEYBOARD_LL fallback. Installed only when both
            // INPUTSINK tiers failed.
            //
            // Background: on AMD GPU systems (notably Ryzen 7 7840HS /
            // Radeon 780M and the Win11 26100 AMD driver stack), both
            // INPUTSINK tiers silently fail with `registration_tier=none`
            // and `wm_input_total=0`. The user's recording then has
            // ZERO keyboard events, which kills the buyer plugin's
            // keyCode field and the action_camera.json speed/look join.
            //
            // The low-level keyboard hook is a different OS path: it
            // sits in the OS keyboard pipeline directly and fires on
            // EVERY keystroke regardless of focus or driver filter.
            // It's keyboard-only (mouse stays broken when this path is
            // taken) but it's better than nothing — Howard's input
            // watchdog needs the keyCode stream alive for the recording
            // to validate.
            //
            // Trade-offs accepted by this tier:
            //   - Mouse events are NOT captured by the hook (would need
            //     a separate WH_MOUSE_LL hook; deferred for now since
            //     the customer's PRD is keyboard-centric for movement
            //     and look is sourced from game_state.jsonl on MC).
            //   - The hook callback runs on our message-pump thread,
            //     so any blocking inside the callback would freeze the
            //     OS keyboard pipeline desktop-wide. We use try_send
            //     and a 10k-element ring buffer to avoid that.
            //   - The hook chain must always be forwarded (we return
            //     `CallNextHookEx(...)`) so games still receive
            //     keystrokes.
            let (tier, hook_handle, hook_rx) = if matches!(tier, RegistrationTier::None) {
                let (tx, rx) = sync_channel::<Event>(10_000);
                // Park the sender / active_keys / metrics in process-
                // wide slots that the static hook callback can reach
                // (the callback is `extern "system" fn` — no captures).
                let _ = HOOK_EVENT_TX.set(Mutex::new(None));
                let _ = HOOK_ACTIVE_KEYS.set(Mutex::new(None));
                let _ = HOOK_METRICS.set(Mutex::new(None));
                if let Some(slot) = HOOK_EVENT_TX.get()
                    && let Ok(mut g) = slot.lock()
                {
                    *g = Some(tx);
                }
                if let Some(slot) = HOOK_ACTIVE_KEYS.get()
                    && let Ok(mut g) = slot.lock()
                {
                    *g = Some(active_keys.clone());
                }
                if let Some(slot) = HOOK_METRICS.get()
                    && let Ok(mut g) = slot.lock()
                {
                    *g = Some(metrics.clone());
                }

                // hMod = NULL: this hook is bound to our thread (the
                // current thread that installed it). MSDN: "If hMod is
                // NULL, the lpfn parameter must point to a hook
                // procedure in the code associated with the current
                // process and the dwThreadId parameter must be zero
                // or the identifier of a thread created by the
                // current process." We're using dwThreadId=0, which
                // means the hook applies to ALL threads of the
                // calling process. For a process-only hook that's
                // fine because the OS dispatches LL hooks to the
                // installing thread regardless.
                match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_ll_proc), None, 0)
                    .wrap_err("tier 3: SetWindowsHookExW(WH_KEYBOARD_LL)")
                {
                    Ok(h) => {
                        tracing::info!(
                            tier = RegistrationTier::Hook.as_str(),
                            "INPUTSINK tiers exhausted; installed WH_KEYBOARD_LL hook \
                             as keyboard-only fallback. Mouse capture is DEAD for this \
                             session (no WH_MOUSE_LL hook installed)."
                        );
                        (RegistrationTier::Hook, Some(h), Some(rx))
                    }
                    Err(e) => {
                        tracing::error!(
                            error = ?e,
                            "tier 3 (WH_KEYBOARD_LL hook) also failed. Input capture is \
                             completely dead for this session — recording will contain \
                             only lifecycle events."
                        );
                        // Clear out the slots we just populated so a
                        // future capture instance can retry cleanly.
                        if let Some(slot) = HOOK_EVENT_TX.get()
                            && let Ok(mut g) = slot.lock()
                        {
                            *g = None;
                        }
                        if let Some(slot) = HOOK_ACTIVE_KEYS.get()
                            && let Ok(mut g) = slot.lock()
                        {
                            *g = None;
                        }
                        if let Some(slot) = HOOK_METRICS.get()
                            && let Ok(mut g) = slot.lock()
                        {
                            *g = None;
                        }
                        (RegistrationTier::None, None, None)
                    }
                }
            } else {
                (tier, None, None)
            };

            // Publish the tier so the host can read it. Done before we
            // return so the parent thread sees the value as soon as the
            // initialize call completes.
            metrics
                .registration_tier
                .store(tier.as_raw(), Ordering::Relaxed);

            if matches!(tier, RegistrationTier::None) {
                // All three tiers failed. Log loudly — the capture
                // thread will still pump window messages but will
                // never receive WM_INPUT, so the recording's
                // inputs.jsonl will contain only lifecycle events.
                tracing::error!(
                    tier = tier.as_str(),
                    "RegisterRawInputDevices and WH_KEYBOARD_LL all FAILED. \
                     Input capture is dead for this session — keyboard/mouse \
                     events will NOT be recorded. Video and gamepad input are \
                     unaffected."
                );
            }

            Ok(Self {
                hwnd,
                class_name,
                h_instance,
                active_keys,
                metrics,
                hook: hook_handle,
                hook_rx,
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

                // Drain any events the WH_KEYBOARD_LL hook produced
                // since the last iteration. The LL hook callback runs
                // SYNCHRONOUSLY on this thread when the OS dispatches
                // a keyboard event — it pushes into the channel and
                // returns immediately. We pull whatever's been queued
                // between (or during) GetMessage calls and forward to
                // the same callback the RawInput path uses, so
                // downstream consumers are oblivious to the path
                // taken.
                if let Some(rx) = self.hook_rx.as_ref() {
                    while let Ok(event) = rx.try_recv() {
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
            // rc16.4: bump the WM_INPUT dispatch counter unconditionally
            // here. We want this counter to reflect "Windows delivered a
            // WM_INPUT to us" even when the subsequent GetRawInputData
            // call fails — that ratio is the diagnostic signal.
            self.metrics.wm_input_total.fetch_add(1, Ordering::Relaxed);

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
                // rc16.4: previously this returned silently. The bingd
                // report on Win11 26100 had HOOK_START + zero events but
                // we had no signal whether the hook was dead or the
                // GetRawInputData call was failing — log explicitly so
                // future investigations can distinguish the two cases.
                use windows::Win32::Foundation::GetLastError;
                let error = GetLastError();
                self.metrics
                    .get_raw_input_data_failures
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    "GetRawInputData size query failed: {:?}, dropping input event",
                    error
                );
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
                self.metrics
                    .get_raw_input_data_failures
                    .fetch_add(1, Ordering::Relaxed);
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
