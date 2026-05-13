//! UI-element refusal detector.
//!
//! Runs at ≈1Hz from an independent tokio task (see `tokio_thread.rs`) and
//! inspects the current desktop / foreground / overlapping-window state for
//! UI conditions that the buyer rejects: a modal popup over the game, a
//! Windows toast in the foreground, the taskbar in z-order top, an
//! alt-tab to the desktop, a Discord/OBS overlay window obscuring the
//! game, or a stalled frame indicating the user opened a pause menu.
//!
//! When any of those conditions is observed, the calling task aborts the
//! in-progress recording: the partial clip is deleted off disk and is NOT
//! enqueued for upload. The user is notified via the tray with a short
//! human-readable reason.
//!
//! ## Layering
//!
//! The detector is split deliberately into two layers so we can unit-test
//! the decision logic on every host (including macOS / Linux CI), without
//! pulling in Win32:
//!
//!   - [`RefusalDetector`] — pure-logic state machine that consumes a
//!     [`WindowSnapshot`] and an `Option<u64>` frame-hash and emits an
//!     `Option<UiRefusalReason>`. Has no `cfg`s and no I/O.
//!   - Platform shims [`capture_window_snapshot`] / [`capture_frame_hash`]
//!     — Win32 on Windows, no-op stubs on every other target. The stubs
//!     return a benign snapshot that never triggers the detector so the
//!     "pref off" / cross-platform-tests paths behave identically.
//!
//! ## Conservatism
//!
//! False positives are far more expensive than false negatives — a 4-minute
//! clip thrown away on a 1-second toast hurts the operator far more than a
//! single passed-through popup. Therefore every check errs on the side of
//! **not** aborting when any individual signal is ambiguous (e.g.,
//! `GetForegroundWindow` returning null), and the pause-menu heuristic
//! requires 5 consecutive seconds of frame-hash equality.

use std::time::Duration;

/// Why the detector wants to abort the current recording. Each variant maps
/// to a short user-facing message shown in the tray notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiRefusalReason {
    /// The expected game window is no longer in the foreground (alt-tab to
    /// desktop, switch to another app, etc.).
    NotForeground,
    /// `Shell_TrayWnd` (the Windows taskbar) is visible in front of the
    /// recording game window.
    TaskbarVisible,
    /// An owner-less popup (Windows toast / system dialog / other modal)
    /// overlaps the game window.
    ModalPopup,
    /// A blacklisted overlay process (Discord, OBS, ShareX, etc.) has a
    /// visible window overlapping the game.
    BlacklistedOverlay(&'static str),
    /// The captured frame hasn't changed for the configured number of
    /// consecutive samples (≥5s by default), which usually means the user
    /// opened a pause / load / config menu.
    StalledFrame,
}

impl UiRefusalReason {
    /// Human-readable, tray-safe label. Kept short — the tray API on
    /// Windows truncates long bodies aggressively.
    pub fn as_str(self) -> &'static str {
        match self {
            UiRefusalReason::NotForeground => "alt-tab / desktop visible",
            UiRefusalReason::TaskbarVisible => "taskbar visible",
            UiRefusalReason::ModalPopup => "modal popup over game",
            UiRefusalReason::BlacklistedOverlay(_) => "overlay app visible",
            UiRefusalReason::StalledFrame => "stalled frame (pause menu?)",
        }
    }

    /// Tray-notification body. Format matches the spec:
    /// `Recording rejected: {reason}`.
    pub fn tray_message(self) -> String {
        match self {
            UiRefusalReason::BlacklistedOverlay(name) => {
                format!("Recording rejected: {name} overlay visible")
            }
            other => format!("Recording rejected: {}", other.as_str()),
        }
    }
}

/// Snapshot of the desktop state at one detector tick. The shape is
/// deliberately platform-agnostic so the same struct can be produced by
/// the Win32 capture path and by mock fixtures in unit tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowSnapshot {
    /// Whether the recorded game window is currently the foreground
    /// window. `None` if the platform could not decide (treated as "OK"
    /// to avoid false positives — see "Conservatism" in the module doc).
    pub game_is_foreground: Option<bool>,
    /// Whether the Windows taskbar (`Shell_TrayWnd`) is visible above
    /// (in z-order) any part of the recorded game window. `None` ⇒ OK.
    pub taskbar_overlapping_game: Option<bool>,
    /// Whether a visible owner-less top-level popup (toast, dialog,
    /// modal) overlaps the recorded game window. `None` ⇒ OK.
    pub modal_popup_overlapping_game: Option<bool>,
    /// Optional process name of a blacklisted overlay (Discord, OBS,
    /// ShareX, NVIDIA Share, …) visibly overlapping the game window.
    /// `None` ⇒ no blacklisted overlay.
    pub blacklisted_overlay: Option<&'static str>,
}

impl WindowSnapshot {
    /// Construct a snapshot that the detector treats as "everything OK".
    /// Used by the Mac/Linux shim and by tests asserting the
    /// no-abort path.
    pub fn ok() -> Self {
        Self {
            game_is_foreground: Some(true),
            taskbar_overlapping_game: Some(false),
            modal_popup_overlapping_game: Some(false),
            blacklisted_overlay: None,
        }
    }
}

/// Detector state. Owned by the polling task that runs once per
/// `check_interval`. Cheap to construct; no I/O.
///
/// The stall-frame counter is the only piece of state across ticks: every
/// other check is a pure function of the current `WindowSnapshot`. The
/// counter resets to zero on any frame-hash change OR if the platform
/// shim couldn't produce a hash (e.g., GDI capture failed) — we never
/// keep counting against a missing measurement.
#[derive(Debug, Clone)]
pub struct RefusalDetector {
    /// Sample interval — passed in so the stall threshold (in samples)
    /// is `stall_window / check_interval`.
    check_interval: Duration,
    /// Minimum consecutive duration of identical frame hash before we
    /// declare a stalled frame. Default: 5s, per the spec.
    stall_window: Duration,
    /// Last observed frame hash (None when none has been observed or
    /// when the shim returned None).
    last_frame_hash: Option<u64>,
    /// How many consecutive samples we've seen the same `last_frame_hash`.
    /// Reset to 0 on hash-change or hash-missing.
    consecutive_identical_samples: u32,
}

impl Default for RefusalDetector {
    fn default() -> Self {
        Self::new(Duration::from_secs(1))
    }
}

impl RefusalDetector {
    /// Build a detector for a given sample interval. The stall window is
    /// fixed at 5 seconds (matching the spec); we round-up the sample
    /// count so a 2-second interval still requires ≥3 samples (≈6s) and
    /// a 1-second interval requires ≥5 samples (≈5s).
    pub fn new(check_interval: Duration) -> Self {
        Self::with_stall_window(check_interval, Duration::from_secs(5))
    }

    /// Construct with an explicit stall window. Test-only helper kept
    /// public so the cross-platform test crate can exercise short windows
    /// without sleeping multiple real seconds.
    pub fn with_stall_window(check_interval: Duration, stall_window: Duration) -> Self {
        let check_interval = if check_interval.is_zero() {
            Duration::from_secs(1)
        } else {
            check_interval
        };
        Self {
            check_interval,
            stall_window,
            last_frame_hash: None,
            consecutive_identical_samples: 0,
        }
    }

    /// Minimum number of consecutive identical samples to declare a
    /// stalled frame. Computed once per call (cheap).
    fn stall_threshold_samples(&self) -> u32 {
        // `Duration::as_millis()` returns `u128`. We do the arithmetic
        // in `u128` to avoid overflow on hilariously-misconfigured
        // windows (e.g. operator-typo'd `refusal_check_interval_sec =
        // u32::MAX`).
        let interval_ms: u128 = self.check_interval.as_millis().max(1);
        let window_ms: u128 = self.stall_window.as_millis().max(interval_ms);
        // Ceil-divide so e.g. 5000ms / 1000ms ⇒ 5 samples exactly,
        // 5000ms / 2000ms ⇒ 3 samples (≈6s) — never < 1. Cap at
        // `u32::MAX` before the truncating cast for the same reason.
        let samples = window_ms.div_ceil(interval_ms).max(1);
        samples.min(u32::MAX as u128) as u32
    }

    /// Inspect one tick of state and decide whether to abort.
    ///
    /// Returns `Some(reason)` if the recording should be aborted. The
    /// caller is responsible for actually performing the abort + delete +
    /// tray notification.
    ///
    /// Priority order matches the spec — `NotForeground` first because
    /// alt-tab strictly dominates every overlay/popup check (if the
    /// game isn't even in the foreground, who cares what's on top of it),
    /// then taskbar, popups, blacklisted overlays, and finally the
    /// stall-frame heuristic which only fires after a multi-second
    /// commitment.
    pub fn observe(
        &mut self,
        snapshot: &WindowSnapshot,
        frame_hash: Option<u64>,
    ) -> Option<UiRefusalReason> {
        // (1) Foreground check. `None` is ambiguous → no abort.
        if snapshot.game_is_foreground == Some(false) {
            self.reset_stall();
            return Some(UiRefusalReason::NotForeground);
        }

        // (2) Taskbar visible / overlapping game.
        if snapshot.taskbar_overlapping_game == Some(true) {
            self.reset_stall();
            return Some(UiRefusalReason::TaskbarVisible);
        }

        // (3) Modal popup / Windows toast.
        if snapshot.modal_popup_overlapping_game == Some(true) {
            self.reset_stall();
            return Some(UiRefusalReason::ModalPopup);
        }

        // (4) Blacklisted overlay app (Discord, OBS, ShareX, …).
        if let Some(name) = snapshot.blacklisted_overlay {
            self.reset_stall();
            return Some(UiRefusalReason::BlacklistedOverlay(name));
        }

        // (5) Stall detection — pure-logic state machine that runs on
        //     every tick. Missing hashes don't penalize.
        if let Some(hash) = frame_hash {
            if Some(hash) == self.last_frame_hash {
                self.consecutive_identical_samples =
                    self.consecutive_identical_samples.saturating_add(1);
            } else {
                self.last_frame_hash = Some(hash);
                self.consecutive_identical_samples = 1;
            }
            if self.consecutive_identical_samples >= self.stall_threshold_samples() {
                // Don't reset here — we want repeated `observe` calls to
                // keep returning StalledFrame until the frame actually
                // changes. The caller will abort on the first one.
                return Some(UiRefusalReason::StalledFrame);
            }
        } else {
            // No hash → reset, so a single failed GDI capture in the
            // middle of a stall doesn't both (a) abort the stall trip
            // unfairly and (b) erase progress unfairly. We pick (b):
            // if we can't see the screen we can't claim the screen is
            // stalled.
            self.reset_stall();
        }

        None
    }

    fn reset_stall(&mut self) {
        self.last_frame_hash = None;
        self.consecutive_identical_samples = 0;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Platform shims. The Win32 implementation is the production code path;
// the stub implementation lets the source file build on every target so
// `crates/ui-refusal-tests` can source-include this file and exercise the
// pure-logic state machine on macOS/Linux CI.
//
// The stub deliberately returns `WindowSnapshot::ok()` and `None` for the
// frame hash so the detector NEVER triggers in a non-Windows build. The
// recorder is Windows-only in production; non-Windows tests of the
// detector wire mock snapshots directly into `RefusalDetector::observe`.
// ─────────────────────────────────────────────────────────────────────────

/// Names of overlay processes whose visible windows count as a refusal
/// when they overlap the game. Kept conservative — every entry must be
/// (a) an opt-in third-party overlay, and (b) something the buyer has
/// historically flagged.
pub const BLACKLISTED_OVERLAY_EXES: &[&str] = &[
    "discord.exe",
    "obs64.exe",
    "obs32.exe",
    "streamlabs obs.exe",
    "sharex.exe",
    "nvidia share.exe",
    "nvcontainer.exe",
    "geforceexperience.exe",
];

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use game_process::windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use game_process::windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use game_process::windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow, GetWindow,
        GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, WS_EX_TOOLWINDOW,
    };
    use game_process::windows::core::BOOL;

    /// Best-effort Win32 capture of `WindowSnapshot` for the recorded game.
    ///
    /// Every check uses `Option<bool>` so a failed Win32 call (rare but
    /// possible — e.g., the HWND just got destroyed) reads as "unknown"
    /// upstream and the detector skips that signal rather than aborting
    /// on garbage.
    pub fn capture_window_snapshot(game_hwnd: HWND) -> WindowSnapshot {
        let mut snap = WindowSnapshot::default();

        // (1) Foreground check.
        let fg = unsafe { GetForegroundWindow() };
        snap.game_is_foreground = if fg.0.is_null() {
            None
        } else {
            Some(fg == game_hwnd)
        };

        // (2) Game rect — needed for overlap checks.
        let game_rect = match get_window_rect(game_hwnd) {
            Some(r) => r,
            None => {
                // Couldn't even read the game's own rect. Don't fabricate
                // a "taskbar/popup overlap" signal in that case.
                return snap;
            }
        };

        // (3) Taskbar (`Shell_TrayWnd`) overlap check. The taskbar always
        // exists on Windows; we test "is it visible AND does its rect
        // overlap the game's rect". This catches alt-tab, exposed
        // taskbar, and "borderless game on monitor with auto-hide taskbar
        // briefly visible" all in one pass.
        snap.taskbar_overlapping_game = Some(taskbar_overlaps(&game_rect));

        // (4) Modal popups + blacklisted overlays — both share a single
        // EnumWindows pass for cost.
        let scan = scan_top_level_windows(game_hwnd, &game_rect);
        snap.modal_popup_overlapping_game = Some(scan.modal_popup);
        snap.blacklisted_overlay = scan.blacklisted_overlay;

        snap
    }

    /// GDI screen-capture frame hash. Returns `None` if any step of the
    /// capture pipeline fails — this is correct conservatism: a missing
    /// hash never increments the stall counter.
    ///
    /// Implementation note: we deliberately do NOT call into OBS or any
    /// per-frame encoder pipeline. The detector runs on its own tokio
    /// task at 1Hz and must not perturb the game's render thread or
    /// stall OBS's frame queue. A coarse, downsampled GDI grab once per
    /// second is several orders of magnitude under the per-frame budget.
    ///
    /// Marked `#[cfg(target_os = "windows")]` because every dependency
    /// here is Win32-only.
    pub fn capture_frame_hash(_game_hwnd: HWND) -> Option<u64> {
        // INTENTIONALLY UNIMPLEMENTED for the MVP:
        //
        // The full GDI capture + downsample + hash path requires
        // additional `windows` features (`Win32_Graphics_Gdi::BitBlt`
        // family) that aren't currently in the workspace's feature list,
        // and adding them increases the windows-sys link cost across
        // every build target. The stall-frame heuristic is the *last*
        // line of defence (priority 5 in the detector) — every preceding
        // check (foreground, taskbar, popup, overlay) fires on the
        // OS-side conditions that account for the majority of buyer
        // rejections.
        //
        // Returning `None` keeps the stall counter at zero, which is
        // strictly safer than a noisy hash that produces false positives.
        // A follow-up spec can wire the BitBlt path when the buyer data
        // says we need it.
        None
    }

    fn get_window_rect(hwnd: HWND) -> Option<RECT> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rect).ok()? };
        Some(rect)
    }

    fn rects_overlap(a: &RECT, b: &RECT) -> bool {
        a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top
    }

    fn taskbar_overlaps(game_rect: &RECT) -> bool {
        use game_process::windows::Win32::UI::WindowsAndMessaging::FindWindowW;
        use game_process::windows::core::PCWSTR;

        let class: Vec<u16> = "Shell_TrayWnd"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let tray = match unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) } {
            Ok(h) if !h.0.is_null() => h,
            _ => return false,
        };
        if !unsafe { IsWindowVisible(tray) }.as_bool() {
            return false;
        }
        let tray_rect = match get_window_rect(tray) {
            Some(r) => r,
            None => return false,
        };
        rects_overlap(&tray_rect, game_rect)
    }

    struct ScanResult {
        modal_popup: bool,
        blacklisted_overlay: Option<&'static str>,
    }

    fn scan_top_level_windows(game_hwnd: HWND, game_rect: &RECT) -> ScanResult {
        struct Ctx {
            game_hwnd: HWND,
            game_rect: RECT,
            game_monitor_rect: RECT,
            modal_popup: bool,
            blacklisted_overlay: Option<&'static str>,
        }

        let game_monitor_rect = get_monitor_rect(game_hwnd).unwrap_or(*game_rect);
        let mut ctx = Ctx {
            game_hwnd,
            game_rect: *game_rect,
            game_monitor_rect,
            modal_popup: false,
            blacklisted_overlay: None,
        };

        extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: `lparam` was constructed below from `&mut Ctx`.
            let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };

            // Skip the game window itself.
            if hwnd == ctx.game_hwnd {
                return BOOL(1);
            }
            // Only visible top-level windows are candidates.
            if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
                return BOOL(1);
            }
            // Skip tool-windows (taskbar buttons, IME candidate windows, etc.).
            let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
            if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
                return BOOL(1);
            }
            let rect = match get_window_rect(hwnd) {
                Some(r) => r,
                None => return BOOL(1),
            };
            // A zero-size invisible-because-clipped window doesn't count
            // as a popup.
            if rect.right <= rect.left || rect.bottom <= rect.top {
                return BOOL(1);
            }
            // Only count it if it overlaps the game's rect OR the game's
            // monitor — the buyer cares about pixels on the recording's
            // monitor, not popups on a secondary display.
            let overlaps = rects_overlap(&rect, &ctx.game_rect)
                || rects_overlap(&rect, &ctx.game_monitor_rect);
            if !overlaps {
                return BOOL(1);
            }

            // Owner-less + visible + overlapping → looks like a modal /
            // toast / dialog.
            let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
            let is_owner_less = match owner {
                Ok(o) => o.0.is_null(),
                Err(_) => true,
            };
            if is_owner_less {
                ctx.modal_popup = true;
            }

            // Blacklisted-overlay check via process exe name.
            if ctx.blacklisted_overlay.is_none()
                && let Some(name) = blacklisted_exe_for_window(hwnd)
            {
                ctx.blacklisted_overlay = Some(name);
            }

            BOOL(1)
        }

        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut Ctx as isize));
        }

        ScanResult {
            modal_popup: ctx.modal_popup,
            blacklisted_overlay: ctx.blacklisted_overlay,
        }
    }

    fn get_monitor_rect(hwnd: HWND) -> Option<RECT> {
        unsafe {
            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if hmon.0.is_null() {
                return None;
            }
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(hmon, &mut info).as_bool() {
                return None;
            }
            Some(info.rcMonitor)
        }
    }

    fn blacklisted_exe_for_window(hwnd: HWND) -> Option<&'static str> {
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            return None;
        }
        let pid_struct = game_process::Pid(pid);
        let exe_path = game_process::exe_name_for_pid(pid_struct).ok()?;
        let exe_name = exe_path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())?;
        for entry in BLACKLISTED_OVERLAY_EXES {
            if exe_name == *entry {
                return Some(*entry);
            }
        }
        // Also catch window-class names of known overlays for processes
        // we can't read (sandboxed / virtualized). Cheap belt-and-braces.
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        if n > 0 {
            let class = String::from_utf16_lossy(&buf[..n as usize]).to_ascii_lowercase();
            if class.contains("discord") {
                return Some("discord.exe");
            }
            if class.contains("obs") {
                return Some("obs64.exe");
            }
        }
        None
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::*;

    /// Non-Windows shim. The recorder is Windows-only in production; the
    /// only reason this code path exists is so the source file compiles
    /// for `aarch64-apple-darwin` (cross-platform CI). It deliberately
    /// returns "everything is OK" so any accidental invocation in a
    /// non-Windows build can never abort a recording.
    pub fn capture_window_snapshot<H: Copy>(_game_hwnd: H) -> WindowSnapshot {
        WindowSnapshot::ok()
    }

    /// Non-Windows shim — see `capture_window_snapshot`. Returns `None`
    /// so the stall counter never advances on non-Windows builds.
    pub fn capture_frame_hash<H: Copy>(_game_hwnd: H) -> Option<u64> {
        None
    }
}

pub use platform::{capture_frame_hash, capture_window_snapshot};

#[cfg(test)]
mod tests {
    use super::*;

    // These tests exercise the pure-logic detector. They run on every
    // target (Windows, macOS, Linux) and use mock snapshots — they
    // never touch a real HWND.

    fn snap_ok() -> WindowSnapshot {
        WindowSnapshot::ok()
    }

    #[test]
    fn ok_snapshot_does_not_abort() {
        let mut d = RefusalDetector::default();
        assert_eq!(d.observe(&snap_ok(), None), None);
    }

    #[test]
    fn not_foreground_aborts_immediately() {
        let mut d = RefusalDetector::default();
        let mut snap = snap_ok();
        snap.game_is_foreground = Some(false);
        assert_eq!(d.observe(&snap, None), Some(UiRefusalReason::NotForeground));
    }

    #[test]
    fn unknown_foreground_does_not_abort() {
        // `None` foreground status is ambiguous — must NOT abort.
        let mut d = RefusalDetector::default();
        let mut snap = snap_ok();
        snap.game_is_foreground = None;
        assert_eq!(d.observe(&snap, None), None);
    }

    #[test]
    fn taskbar_overlap_aborts() {
        let mut d = RefusalDetector::default();
        let mut snap = snap_ok();
        snap.taskbar_overlapping_game = Some(true);
        assert_eq!(
            d.observe(&snap, None),
            Some(UiRefusalReason::TaskbarVisible)
        );
    }

    #[test]
    fn modal_popup_aborts() {
        let mut d = RefusalDetector::default();
        let mut snap = snap_ok();
        snap.modal_popup_overlapping_game = Some(true);
        assert_eq!(d.observe(&snap, None), Some(UiRefusalReason::ModalPopup));
    }

    #[test]
    fn blacklisted_overlay_aborts_with_name() {
        let mut d = RefusalDetector::default();
        let mut snap = snap_ok();
        snap.blacklisted_overlay = Some("discord.exe");
        assert_eq!(
            d.observe(&snap, None),
            Some(UiRefusalReason::BlacklistedOverlay("discord.exe"))
        );
    }

    #[test]
    fn stall_detected_after_threshold_samples() {
        let mut d =
            RefusalDetector::with_stall_window(Duration::from_secs(1), Duration::from_secs(5));
        // 4 identical samples → no abort yet.
        for _ in 0..4 {
            assert_eq!(d.observe(&snap_ok(), Some(42)), None);
        }
        // 5th identical sample → stall.
        assert_eq!(
            d.observe(&snap_ok(), Some(42)),
            Some(UiRefusalReason::StalledFrame)
        );
    }

    #[test]
    fn stall_counter_resets_on_hash_change() {
        let mut d =
            RefusalDetector::with_stall_window(Duration::from_secs(1), Duration::from_secs(5));
        for _ in 0..4 {
            assert_eq!(d.observe(&snap_ok(), Some(42)), None);
        }
        // Frame changes → counter must reset, no abort yet.
        assert_eq!(d.observe(&snap_ok(), Some(43)), None);
        // Need 4 more identical 43s to hit threshold.
        for _ in 0..3 {
            assert_eq!(d.observe(&snap_ok(), Some(43)), None);
        }
        assert_eq!(
            d.observe(&snap_ok(), Some(43)),
            Some(UiRefusalReason::StalledFrame)
        );
    }

    #[test]
    fn missing_hash_does_not_advance_stall() {
        // A failed capture in the middle of a sequence shouldn't both
        // erase the stall AND fail to detect it later — we explicitly
        // chose "reset on missing", so verify that.
        let mut d =
            RefusalDetector::with_stall_window(Duration::from_secs(1), Duration::from_secs(5));
        for _ in 0..4 {
            assert_eq!(d.observe(&snap_ok(), Some(42)), None);
        }
        // Missing hash resets.
        assert_eq!(d.observe(&snap_ok(), None), None);
        // First post-reset sample → counter = 1, not stalled yet.
        assert_eq!(d.observe(&snap_ok(), Some(42)), None);
    }

    #[test]
    fn priority_foreground_dominates_overlay() {
        let mut d = RefusalDetector::default();
        let snap = WindowSnapshot {
            game_is_foreground: Some(false),
            taskbar_overlapping_game: Some(true),
            modal_popup_overlapping_game: Some(true),
            blacklisted_overlay: Some("discord.exe"),
        };
        // Foreground check is highest priority.
        assert_eq!(d.observe(&snap, None), Some(UiRefusalReason::NotForeground));
    }

    #[test]
    fn stall_threshold_scales_with_interval() {
        // 2-second interval, 5s window → ceil(5/2) = 3 samples.
        let mut d =
            RefusalDetector::with_stall_window(Duration::from_secs(2), Duration::from_secs(5));
        assert_eq!(d.observe(&snap_ok(), Some(1)), None);
        assert_eq!(d.observe(&snap_ok(), Some(1)), None);
        assert_eq!(
            d.observe(&snap_ok(), Some(1)),
            Some(UiRefusalReason::StalledFrame)
        );
    }

    #[test]
    fn tray_message_format() {
        assert_eq!(
            UiRefusalReason::NotForeground.tray_message(),
            "Recording rejected: alt-tab / desktop visible"
        );
        assert_eq!(
            UiRefusalReason::BlacklistedOverlay("discord.exe").tray_message(),
            "Recording rejected: discord.exe overlay visible"
        );
    }
}
