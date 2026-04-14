use windows::{
    core::HSTRING,
    Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, MessageBoxW, MB_ICONERROR, MB_ICONWARNING, MB_SETFOREGROUND,
        MB_TOPMOST,
    },
};

fn get_parent_window() -> Option<windows::Win32::Foundation::HWND> {
    unsafe {
        // Try to get the current foreground window
        let foreground = GetForegroundWindow();
        if !foreground.is_invalid() {
            Some(foreground)
        } else {
            // If no foreground window, return None (will use desktop)
            None
        }
    }
}

pub fn error_message_box(body: &str) {
    unsafe {
        let parent = get_parent_window();
        // MB_SETFOREGROUND flag makes the message box the foreground window
        let _ = MessageBoxW(
            parent,
            &HSTRING::from(body),
            &HSTRING::from("GameData Recorder - Error"),
            MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

pub fn warning_message_box(body: &str) {
    unsafe {
        let parent = get_parent_window();
        // MB_SETFOREGROUND flag makes the message box the foreground window
        let _ = MessageBoxW(
            parent,
            &HSTRING::from(body),
            &HSTRING::from("GameData Recorder - Warning"),
            MB_ICONWARNING | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}
