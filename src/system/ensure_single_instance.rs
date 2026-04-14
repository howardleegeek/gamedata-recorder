use color_eyre::eyre;

/// Ensures only one instance of the application is running.
///
/// Uses a named Windows mutex. The correct pattern is:
/// 1. CreateMutexW with bInitialOwner = false
/// 2. Check GetLastError() == ERROR_ALREADY_EXISTS immediately after
/// 3. Store the handle so it lives for the process lifetime (dropped = released)
///
/// The previous implementation used bInitialOwner = true + WaitForSingleObject,
/// which never detected a second instance because mutexes are recursive on Windows.
#[cfg(target_os = "windows")]
pub fn ensure_single_instance() -> eyre::Result<()> {
    use windows::{
        core::PCWSTR,
        Win32::{Foundation::ERROR_ALREADY_EXISTS, System::Threading::CreateMutexW},
    };

    let mutex_name = "GameData-Recorder-SingleInstance";
    let mutex_name_wide: Vec<u16> = mutex_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        // bInitialOwner = false — we don't want to own it yet, just check if it exists
        let mutex_result = CreateMutexW(None, false, PCWSTR(mutex_name_wide.as_ptr()));

        match mutex_result {
            Ok(_handle) => {
                // Check GetLastError() IMMEDIATELY after CreateMutexW, before any other
                // operations that could overwrite the last error code. This is the correct
                // pattern to detect if the mutex already existed (another instance running).
                let last_error = GetLastError();

                // Check if the mutex already existed (another instance created it)
                if last_error == ERROR_ALREADY_EXISTS {
                    use crate::ui::notification::error_message_box;

                    error_message_box(concat!(
                        "Another instance of GameData Recorder is already running.\n\n",
                        "Only one instance can run at a time."
                    ));
                    eyre::bail!("Another instance of GameData Recorder is already running.");
                }

                // We're the first instance. The handle is intentionally leaked (not dropped)
                // so the mutex stays alive for the process lifetime. Windows automatically
                // releases it when the process exits.
                std::mem::forget(_handle);
            }
            Err(e) => {
                // Fail closed — prevent multiple instances from corrupting recordings.
                // Mutex failure (permissions, anti-cheat, resource exhaustion) is fatal
                // to ensure recording integrity and prevent OBS hook conflicts.
                eyre::bail!(
                    "Failed to create single-instance mutex (recording integrity safeguard): {e}"
                );
            }
        }
    }

    Ok(())
}

/// Ensures only one instance of the application is running.
#[cfg(not(target_os = "windows"))]
pub fn ensure_single_instance() -> eyre::Result<()> {
    Ok(())
}
