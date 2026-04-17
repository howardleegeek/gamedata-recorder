use windows::{
    Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_SETFOREGROUND,
        MB_TOPMOST, MessageBoxW, MESSAGEBOX_STYLE, SetForegroundWindow,
    },
    core::HSTRING,
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

fn show_message_box(body: &str, title: &str, icon: u32) {
    unsafe {
        let parent = get_parent_window();
        // Force the message box to become the foreground window
        let result = MessageBoxW(
            parent,
            &HSTRING::from(body),
            &HSTRING::from(title),
            MESSAGEBOX_STYLE(icon | MB_TOPMOST.0 | MB_SETFOREGROUND.0),
        );
        // Ensure the message box window gets focus
        let _ = SetForegroundWindow(GetForegroundWindow());
        result
    };
}

pub fn error_message_box(body: &str) {
    show_message_box(body, "GameData Recorder - Error", MB_ICONERROR.0);
}

pub fn warning_message_box(body: &str) {
    show_message_box(body, "GameData Recorder - Warning", MB_ICONWARNING.0);
}

pub fn info_message_box(body: &str) {
    show_message_box(
        body,
        "GameData Recorder - Information",
        MB_ICONINFORMATION.0,
    );
}
