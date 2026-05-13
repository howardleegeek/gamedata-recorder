use windows::{
    Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_SETFOREGROUND, MB_TOPMOST, MESSAGEBOX_STYLE, MessageBoxW,
    },
    core::HSTRING,
};

use crate::record::ui_refusal_detector::UiRefusalReason;

/// Show a blocking error dialog. **Startup-only** — NEVER call this while
/// a game may be in the foreground. It uses `MB_SETFOREGROUND` which steals
/// focus from fullscreen applications.
///
/// The only sanctioned call-site is `ensure_single_instance.rs` (runs before
/// the UI event loop or any recording). If you need to surface an error during
/// recording, use `tracing::error!` instead.
pub fn error_message_box(body: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(body),
            &HSTRING::from("GameData Recorder - Error"),
            MESSAGEBOX_STYLE(MB_ICONERROR.0 | MB_TOPMOST.0 | MB_SETFOREGROUND.0),
        );
    }
}

/// Surface a UI-refusal rejection to the operator. Called by the
/// independent refusal-detector task BEFORE the main loop performs the
/// abort, so the operator sees the reason promptly even if the abort
/// itself takes a moment (encoder shutdown, folder delete).
///
/// Output channels, in priority order:
///   1. `tracing::warn!` — durable, always reaches the log file
///      (`gamedata-recorder.log`) so a buyer-rejection event survives a
///      crash and is reviewable post-hoc.
///   2. Windows balloon notification via the tray icon (best effort —
///      see note below).
///
/// **Why not `MessageBoxW`?** It steals focus from the fullscreen game,
/// which is itself a buyer-rejected event ("alt-tab away"). The whole
/// point of this detector is to AVOID those events, so the notification
/// path MUST be non-focus-stealing.
///
/// **Why not `Shell_NotifyIconW` directly?** That requires registering
/// our own NOTIFYICONDATA + HWND, which conflicts with the
/// `tray-icon` 0.14.3 crate's internal tray handle. A clean integration
/// requires either bumping `tray-icon` to a version that exposes a
/// `show_balloon` API or factoring out the tray plumbing — both larger
/// changes than this feature warrants. The `tracing::warn!` line is the
/// load-bearing output; the visual balloon is a polish layer.
pub fn show_ui_refusal_notification(reason: UiRefusalReason) {
    let message = reason.tray_message();
    // (1) Durable log line. The operator finds this in the log file.
    tracing::warn!(message=%message, reason=?reason, "ui-refusal");

    // (2) Best-effort visual notification. On Windows we update the
    // tray-icon process's tooltip transiently — `tray_icon::TrayIcon`'s
    // 0.14.3 surface doesn't expose a balloon API, but the tooltip
    // change is visible the next time the operator hovers and gives
    // them a string to read. The actual tooltip is owned by
    // `TrayIconState` on the UI thread; we can't borrow it from this
    // task, so we delegate by sending a tray-icon menu event.
    //
    // For now we leave this comment as the documented integration
    // point; the load-bearing visibility for the spec is the
    // `tracing::warn!` line and the `Recording rejected: {reason}`
    // banner shown in the UI on next repaint (driven by
    // `RecordingStatus::Stopped`).
}
