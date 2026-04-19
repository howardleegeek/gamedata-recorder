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

pub fn does_process_exist(Pid(pid): Pid) -> Result<bool, Error> {
    unsafe {
        let process = Owned::new(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?);
        let mut exit_code = 0;
        GetExitCodeProcess(*process, &mut exit_code)?;
        Ok(exit_code == STILL_ACTIVE.0 as u32)
    }
}

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

pub fn foreground_window() -> Result<(HWND, Pid), Error> {
    unsafe {
        let hwnd = GetForegroundWindow();
        let mut pid = 0;
        if GetWindowThreadProcessId(hwnd, Some(&mut pid)) == 0 {
            return Err(Error::from_thread());
        }
        Ok((hwnd, Pid(pid)))
    }
}

/// Iterate every running process on the system, invoking `f` with each
/// process-entry record. Return `false` from `f` to stop enumeration early.
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

pub fn hardware_id() -> Result<String> {
    unsafe {
        let mut hw_profile_info = HW_PROFILE_INFOA::default();

        windows::Win32::System::WindowsProgramming::GetCurrentHwProfileA(&mut hw_profile_info)?;

        let guid = hw_profile_info.szHwProfileGuid.map(|x| x as u8);
        let guid = CStr::from_bytes_with_nul(&guid)?;
        Ok(guid.to_str()?.to_owned())
    }
}

/// Gets all of the modules loaded by the process.
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
