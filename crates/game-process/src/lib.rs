use std::{ffi::CStr, path::PathBuf};

use color_eyre::{Result, eyre::Context as _};

use windows::{
    Win32::{
        Foundation::{HWND, STILL_ACTIVE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, MODULEENTRY32, Module32First, Module32Next,
                PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPMODULE,
                TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
            },
            Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_NAME_NATIVE, PROCESS_QUERY_INFORMATION,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
            WindowsProgramming::HW_PROFILE_INFOA,
        },
        UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    },
    core::{Error, Owned, PWSTR},
};

pub use windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pid(pub u32);

/// Checks whether a process with the given PID is still running.
///
/// Opens the process with `PROCESS_QUERY_LIMITED_INFORMATION` access and
/// checks if the exit code is `STILL_ACTIVE`. Returns `true` if the process
/// is running, `false` if it has exited. Returns an error if the process
/// cannot be opened (e.g., access denied or PID does not exist).
///
/// This is used by the recorder to verify that a game process is still
/// alive before attempting to capture from it.
pub fn does_process_exist(Pid(pid): Pid) -> Result<bool, Error> {
    unsafe {
        let process = Owned::new(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?);
        let mut exit_code = 0;
        GetExitCodeProcess(*process, &mut exit_code)?;
        Ok(exit_code == STILL_ACTIVE.0 as u32)
    }
}

/// Returns the full executable path for a process identified by its PID.
///
/// Uses `QueryFullProcessImageNameW` (UTF-16 wide-char variant) to correctly
/// handle paths containing non-ASCII characters (e.g., Chinese characters on
/// localized Windows installations). Returns a `PathBuf` with the resolved
/// executable path, or an error if the process cannot be opened.
pub fn exe_name_for_pid(Pid(pid): Pid) -> Result<PathBuf> {
    // v2.5.5: wide-char (UTF-16) variant. The v2.5.4 implementation used
    // `QueryFullProcessImageNameA`, which returns ANSI bytes in the current
    // code page. On Chinese-locale Windows (our confirmed client host is
    // `华硕主机X`), an NTFS path containing Chinese characters gets encoded
    // as GBK in the ANSI path — Rust's `CString::new` / UTF-8 decoding then
    // either errored silently or produced mojibake that didn't match the
    // whitelist. Every Chinese-pathed game exe was invisible to the
    // recorder. The W variant returns a proper UTF-16 buffer which we
    // losslessly convert to a Rust `String` and then a `PathBuf`.
    unsafe {
        let process = Owned::new(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?);

        let mut process_name = [0u16; 1024];
        let mut process_name_size = process_name.len() as u32;
        QueryFullProcessImageNameW(
            *process,
            PROCESS_NAME_NATIVE,
            PWSTR(process_name.as_mut_ptr()),
            &mut process_name_size,
        )?;
        let len: usize = process_name_size
            .try_into()
            .map_err(|e| color_eyre::eyre::eyre!("process_name_size too large: {}", e))?;
        // `QueryFullProcessImageNameW` writes `process_name_size` UTF-16 code
        // units without a trailing NUL counted in the size, so we slice to
        // exactly that length before converting. Use `from_utf16_lossy` —
        // Win32 paths should always be valid UTF-16, but a defensive decode
        // prevents any edge case from panicking recording startup.
        let name = String::from_utf16_lossy(&process_name[..len]);
        Ok(PathBuf::from(name))
    }
}

/// Returns the window that is currently in the foreground and the process ID
/// of the application that owns it.
///
/// This is used to detect which game the user is actively playing when they
/// press the hotkey to start/stop recording. The returned `HWND` can be used
/// for subsequent window-specific operations, and the `Pid` can be matched
/// against the game whitelist to verify the target is an allowed game.
pub fn foreground_window() -> Result<(HWND, Pid), Error> {
    unsafe {
        let hwnd = GetForegroundWindow();
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        Ok((hwnd, Pid(pid)))
    }
}

/// Iterates over all running processes, calling `f` with each process entry.
///
/// The callback receives a `PROCESSENTRY32W` (wide-character variant) and
/// should return `true` to continue iteration or `false` to stop.
///
/// This is used to locate running games by scanning the process list and
/// checking each executable name against the whitelist. The wide-character
/// variant is required because process names may contain non-ASCII characters
/// (Chinese, Japanese, Cyrillic, etc.) that would be corrupted by the ANSI
/// API on non-English Windows locales.
///
/// v2.5.5: migrated to `PROCESSENTRY32W` + `Process32FirstW` / `Process32NextW`
/// so that exe names containing non-ASCII characters (Chinese, Japanese,
/// Cyrillic, etc.) decode correctly. The ANSI variant returned bytes in
/// the system code page, which on Chinese-locale Windows silently corrupted
/// paths and made non-ASCII-named games invisible to the whitelist.
/// Callers receive a `PROCESSENTRY32W` whose `szExeFile` is a UTF-16 buffer.
pub fn for_each_process(mut f: impl FnMut(PROCESSENTRY32W) -> bool) -> Result<(), Error> {
    unsafe {
        let snapshot = Owned::new(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?);

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(*snapshot, &mut entry).is_err() {
            return Ok(());
        }

        loop {
            if !f(entry) {
                break;
            }
            if Process32NextW(*snapshot, &mut entry).is_err() {
                break;
            }
        }

        Ok(())
    }
}

/// Decode the NUL-terminated UTF-16 exe-file name out of a `PROCESSENTRY32W`
/// into an owned `String`. Helper because every caller needs to do this and
/// the raw `[u16; 260]` is awkward.
pub fn exe_file_name(entry: &PROCESSENTRY32W) -> String {
    let len = entry
        .szExeFile
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(entry.szExeFile.len());
    String::from_utf16_lossy(&entry.szExeFile[..len])
}

/// Retrieves the hardware profile GUID for the current Windows system.
///
/// This function calls the Windows API `GetCurrentHwProfileA` to obtain
/// the hardware profile GUID of the current system. The GUID is returned
/// as a string representation (e.g., `"{GUID}"`). This can be used as a
/// stable hardware identifier for the machine.
///
/// # Returns
/// - `Result<String>`: The hardware profile GUID as a string, or an error
///   if the Windows API call fails or the returned data is not valid UTF-8.
///
/// # Safety
/// This function uses unsafe Windows API calls. The `GetCurrentHwProfileA`
/// function is considered safe to call as it only reads system information.
pub fn hardware_id() -> Result<String> {
    unsafe {
        let mut hw_profile_info = HW_PROFILE_INFOA::default();

        windows::Win32::System::WindowsProgramming::GetCurrentHwProfileA(&mut hw_profile_info)?;

        let guid = hw_profile_info.szHwProfileGuid.map(|x| x as u8);
        let guid = CStr::from_bytes_with_nul(&guid)?;
        Ok(guid.to_str()?.to_owned())
    }
}

/// Retrieves the names of all modules (DLLs) loaded by a given process.
///
/// This function opens the target process with query permissions, creates
/// a module snapshot using the Windows ToolHelp API, and iterates through
/// all loaded modules to collect their names. The returned vector contains
/// the module names as owned `String` values.
///
/// # Arguments
/// * `pid` - The process ID of the target process
///
/// # Returns
/// A `Result` containing a `Vec<String>` of all loaded module names, or an
/// error if the process cannot be opened or the snapshot cannot be created.
/// Returns an empty vector if the process has no modules or if enumeration
/// fails after successfully opening the process.
///
/// # Example
/// ```
/// use gamedata_recorder_game_process::Pid;
///
/// let modules = get_modules(Pid(1234))?;
/// for module in modules {
///     println!("Loaded module: {}", module);
/// }
/// ```
pub fn get_modules(pid: Pid) -> Result<Vec<String>> {
    unsafe {
        // Open the target process with query permissions
        let process_handle = OpenProcess(PROCESS_QUERY_INFORMATION, false, pid.0)
            .context("Failed to open process")?;

        let _process_guard = Owned::new(process_handle);

        // Create a snapshot of all modules (DLLs) loaded by the process
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid.0)
            .context("Failed to create module snapshot")?;
        let _snapshot_guard = Owned::new(snapshot);

        let mut module_entry = MODULEENTRY32 {
            dwSize: std::mem::size_of::<MODULEENTRY32>() as u32,
            ..Default::default()
        };

        // Get the first module
        if Module32First(snapshot, &mut module_entry).is_err() {
            return Ok(vec![]);
        }

        let mut output = vec![];

        // Check all loaded modules for graphics API DLLs
        loop {
            output.push(
                std::ffi::CStr::from_ptr(module_entry.szModule.as_ptr())
                    .to_string_lossy()
                    .to_string(),
            );

            // Move to next module
            if Module32Next(snapshot, &mut module_entry).is_err() {
                break;
            }
        }

        Ok(output)
    }
}
