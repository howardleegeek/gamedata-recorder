use windows::{
    Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_SETFOREGROUND, MB_TOPMOST, MESSAGEBOX_STYLE, MessageBoxW,
    },
    core::HSTRING,
};

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
