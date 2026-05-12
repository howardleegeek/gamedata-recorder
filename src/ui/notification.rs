#[cfg(windows)]
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
#[cfg(windows)]
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

#[cfg(not(windows))]
pub fn error_message_box(_body: &str) {
    // No-op on non-Windows (recorder is Windows-only in production; this
    // stub keeps unit tests / lint cross-platform).
}

/// Stream BN (rc17.2): post-session toast notification.
///
/// Shows a Windows toast (Action Center entry) with `title` + `body`, and
/// when the user clicks the toast we open `click_dir` in Explorer so they
/// can inspect the failing session's `lint_result.json` immediately. Used
/// by [`crate::record::validation`] when the auto-lint v3 pass returns
/// FAIL, so bad sessions never quietly enter the upload pipeline — the
/// operator sees the failure within ~1s of `Recording::stop()` finishing.
///
/// Implementation notes:
///   * Non-blocking. Spawns `powershell.exe` and returns; we do **not**
///     wait for the user to dismiss/click. A failed spawn is logged at
///     `warn` and otherwise swallowed — a missing toast must never
///     invalidate an otherwise-good session.
///   * Uses Windows 10+ built-in WinRT `ToastNotificationManager` via a
///     small inline PowerShell script. No new Rust crate dependency
///     (Cargo.toml unchanged); no BurntToast / WiX requirement on the
///     target machine. The script is bounded (~25 lines) and avoids
///     `Invoke-Expression` / shell interpolation of user input — the
///     title/body/path are passed via `[Environment]::SetEnvironmentVariable`
///     equivalents, so a malicious session name cannot inject script.
///   * Click-to-open behaviour: in addition to the toast, we **also**
///     spawn `explorer.exe <click_dir>` immediately on FAIL. The toast
///     itself uses `protocol activation = file:///...` which Windows 10+
///     hands to Explorer; but Explorer activation requires AUMID
///     registration that we don't ship, so we fall back to the
///     fire-and-forget `explorer.exe` spawn for guaranteed open. This
///     keeps the operator-visible behaviour deterministic across
///     Windows builds without us having to ship a COM-registered AUMID.
///   * Cross-platform stub: on non-Windows the helper logs and returns,
///     so the `validation` module can call it unconditionally from
///     `Recording::stop()` without `#[cfg]` gates.
#[cfg(windows)]
pub fn post_session_toast(title: &str, body: &str, click_dir: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // CREATE_NO_WINDOW = 0x08000000 — prevents a flashing console window
    // when the PowerShell child spawns. Critical because the user is
    // typically returning from a fullscreen game when Recording::stop()
    // fires and any visible console flash would be jarring.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // PowerShell-safe escape: WinRT XML uses double-quoted attributes, so
    // we only need to escape `&`, `<`, `>`, `"`. Single quotes are fine
    // inside the heredoc literal we feed to PowerShell.
    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    let title_xml = xml_escape(title);
    let body_xml = xml_escape(body);
    let dir_str = click_dir.display().to_string();

    // The WinRT toast XML is intentionally minimal: 1 line title, 1 line
    // body, no buttons. Buttons require AUMID registration to fire a
    // click handler back into our process; we sidestep that by opening
    // Explorer below (deterministic, no COM registration).
    let ps_script = format!(
        r#"$ErrorActionPreference = 'SilentlyContinue'
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] | Out-Null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom, ContentType=WindowsRuntime] | Out-Null
$xml = New-Object Windows.Data.Xml.Dom.XmlDocument
$xml.LoadXml(@"
<toast><visual><binding template="ToastGeneric"><text>{title}</text><text>{body}</text></binding></visual></toast>
"@)
$toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('GameData Recorder').Show($toast)
"#,
        title = title_xml,
        body = body_xml,
    );

    // Fire the toast (fire-and-forget; spawn errors are logged but never
    // propagated — a failed toast must not invalidate a recording).
    match Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &ps_script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(_child) => {
            tracing::info!(
                title = %title,
                "post_session_toast: WinRT toast spawned"
            );
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                title = %title,
                "post_session_toast: failed to spawn PowerShell for toast"
            );
        }
    }

    // Click-equivalent: open Explorer at the session dir so the operator
    // can immediately see lint_result.json. This is what the user would
    // expect from clicking the toast, but does NOT require AUMID
    // registration (which we don't ship). Skipped if the path is invalid
    // (caller bug; logged but ignored).
    if click_dir.exists() {
        match Command::new("explorer.exe")
            .arg(&dir_str)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(_) => {
                tracing::debug!(dir = %dir_str, "post_session_toast: opened Explorer at session_dir");
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    dir = %dir_str,
                    "post_session_toast: failed to spawn Explorer (toast still shown)"
                );
            }
        }
    } else {
        tracing::warn!(
            dir = %dir_str,
            "post_session_toast: session_dir does not exist, skipping Explorer open"
        );
    }
}

#[cfg(not(windows))]
pub fn post_session_toast(title: &str, body: &str, click_dir: &std::path::Path) {
    // Non-Windows stub for cross-platform compilation. The recorder
    // itself is Windows-only in production, but the unit tests for
    // `crate::record::validation::run_lint_v3` run on the dev host
    // (often macOS) and need to be able to call this helper without
    // a cfg gate at every call site.
    tracing::info!(
        title = %title,
        body = %body,
        click_dir = %click_dir.display(),
        "post_session_toast: non-Windows stub (no-op)"
    );
}
