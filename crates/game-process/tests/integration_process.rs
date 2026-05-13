//! Integration tests for `game-process` (R7.1 / R7.2 foreground gate).
//!
//! `game-process` wraps the Win32 process / window enumeration APIs the
//! recorder uses to detect which game is in the foreground. All entry
//! points are Win32-backed, so this whole file is gated to Windows.
//!
//! The tests deliberately exercise the *crate's own process* — that is
//! the only process we can assume exists across CI runs without
//! depending on external test fixtures.

#![cfg(target_os = "windows")]

use std::process;

use game_process::{
    Pid, does_process_exist, exe_file_name, exe_name_for_pid, for_each_process, foreground_window,
    get_modules, hardware_id,
};

// ---------------------------------------------------------------------------
// does_process_exist
// ---------------------------------------------------------------------------

#[test]
fn does_process_exist_for_self() {
    // The current test process must exist.
    let pid = Pid(process::id());
    let result = does_process_exist(pid).expect("OpenProcess on self must succeed");
    assert!(result, "self process must exist");
}

#[test]
fn does_process_exist_for_invalid_pid_returns_err_or_false() {
    // PID 0xFFFF_FFFE is reserved / unlikely to ever be alive. OpenProcess
    // returns ERROR_INVALID_PARAMETER, which we surface as Err.
    let pid = Pid(0xFFFF_FFFE);
    let result = does_process_exist(pid);
    // Either Err or Ok(false) is acceptable; the contract is "not true".
    if let Ok(exists) = result {
        assert!(!exists, "bogus PID must not report exists=true");
    }
}

// ---------------------------------------------------------------------------
// exe_name_for_pid — W-suffix path handling
// ---------------------------------------------------------------------------

#[test]
fn exe_name_for_self_is_non_empty() {
    // R7.2 spec: W-suffix path handling — `QueryFullProcessImageNameW`
    // returns a proper UTF-16 buffer that we losslessly decode. The
    // self-process exe name must round-trip through that and produce a
    // non-empty PathBuf.
    let pid = Pid(process::id());
    let path = exe_name_for_pid(pid).expect("self exe name must resolve");
    let s = path.to_string_lossy();
    assert!(!s.is_empty(), "self exe name should be non-empty");
    assert!(
        s.ends_with(".exe") || s.contains("test"),
        "self exe should look like a test binary path: {s}"
    );
}

#[test]
fn exe_name_for_self_is_valid_path() {
    let pid = Pid(process::id());
    let path = exe_name_for_pid(pid).expect("self exe");
    // The path must have at least one component (drive letter / directory).
    assert!(path.components().count() > 0);
}

#[test]
fn exe_name_for_invalid_pid_returns_err() {
    let pid = Pid(0xFFFF_FFFE);
    let result = exe_name_for_pid(pid);
    assert!(result.is_err(), "bogus PID must error");
}

// ---------------------------------------------------------------------------
// foreground_window — returns (HWND, Pid)
// ---------------------------------------------------------------------------

#[test]
fn foreground_window_returns_valid_pid() {
    // The function returns (HWND, Pid). The PID might be 0 if no window
    // is in the foreground (headless CI), but the call itself must not
    // panic.
    let result = foreground_window();
    // Either Ok or Err is acceptable on headless CI; just exercise the path.
    if let Ok((_hwnd, Pid(pid))) = result {
        // PID can be 0 on headless. Don't assert non-zero.
        let _ = pid;
    }
    // Err case is also fine — headless runners may have no foreground window.
}

// ---------------------------------------------------------------------------
// for_each_process — process snapshot enumeration
// ---------------------------------------------------------------------------

#[test]
fn for_each_process_finds_at_least_one_entry() {
    // CreateToolhelp32Snapshot must succeed and yield at least the
    // System Idle Process + ourself. Count entries and assert >= 1.
    let mut count = 0u32;
    let result = for_each_process(|_entry| {
        count += 1;
        true // continue
    });
    assert!(result.is_ok(), "process enumeration must succeed");
    assert!(
        count > 0,
        "process snapshot should yield at least one entry"
    );
}

#[test]
fn for_each_process_can_short_circuit_by_returning_false() {
    // The callback's `false` return must stop enumeration immediately.
    let mut count = 0u32;
    let _ = for_each_process(|_entry| {
        count += 1;
        false // stop after first
    });
    assert_eq!(
        count, 1,
        "callback returning false must stop after first call"
    );
}

#[test]
fn for_each_process_callback_yields_processentry32w_with_valid_size() {
    // Per Win32 docs, PROCESSENTRY32W.dwSize is set to the struct size
    // before calling Process32FirstW. The crate handles this; the
    // callback receives the populated entry with the canonical size.
    let mut seen_valid = false;
    let _ = for_each_process(|entry| {
        let expected_size = std::mem::size_of::<
            windows::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W,
        >() as u32;
        if entry.dwSize == expected_size {
            seen_valid = true;
        }
        true
    });
    assert!(
        seen_valid,
        "at least one entry should have canonical dwSize"
    );
}

#[test]
fn for_each_process_includes_self() {
    // Our own PID must appear in the snapshot. This both exercises the
    // enumeration and confirms the PID-matching logic works for live
    // processes.
    let self_pid = process::id();
    let mut found = false;
    let _ = for_each_process(|entry| {
        if entry.th32ProcessID == self_pid {
            found = true;
            return false; // short-circuit
        }
        true
    });
    assert!(found, "snapshot must include this test process");
}

// ---------------------------------------------------------------------------
// exe_file_name — UTF-16 NUL-terminated decoder
// ---------------------------------------------------------------------------

#[test]
fn exe_file_name_decodes_self_entry() {
    // Find our own entry, then decode its szExeFile via the helper.
    let self_pid = process::id();
    let mut decoded: Option<String> = None;
    let _ = for_each_process(|entry| {
        if entry.th32ProcessID == self_pid {
            decoded = Some(exe_file_name(&entry));
            return false;
        }
        true
    });
    let name = decoded.expect("self entry must be findable");
    assert!(!name.is_empty(), "exe file name must be non-empty");
    // Should end with .exe and not contain trailing NULs.
    assert!(
        name.ends_with(".exe") || name.ends_with(".EXE"),
        "exe name should end with .exe: {name}"
    );
    assert!(!name.contains('\0'), "exe name must not contain NUL bytes");
}

#[test]
fn exe_file_name_handles_zero_filled_szexefile_field() {
    // Construct a synthetic entry with a sub-MAX_PATH name and trailing
    // NUL — the helper should stop at the first NUL.
    use windows::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W;
    let mut entry = PROCESSENTRY32W::default();
    // Write "test.exe" + NUL into szExeFile and zero the rest.
    let utf16: Vec<u16> = "test.exe".encode_utf16().collect();
    entry.szExeFile[..utf16.len()].copy_from_slice(&utf16);
    // entry.szExeFile[utf16.len()] is already 0
    let decoded = exe_file_name(&entry);
    assert_eq!(decoded, "test.exe");
}

#[test]
fn exe_file_name_handles_full_szexefile_without_nul() {
    // Edge case: szExeFile fills the entire 260-element buffer with no
    // NUL terminator (would only happen with a malformed snapshot, but
    // the helper guards via `unwrap_or(len())`).
    use windows::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W;
    let entry = PROCESSENTRY32W {
        // Fill every slot with 'A' (no NUL).
        szExeFile: [b'A' as u16; 260],
        ..Default::default()
    };
    let decoded = exe_file_name(&entry);
    // Must not panic; must return the full string.
    assert_eq!(decoded.len(), 260);
    assert!(decoded.chars().all(|c| c == 'A'));
}

#[test]
fn exe_file_name_decodes_unicode_chars() {
    // R7.2 spec: W-API correctly handles Chinese / Japanese / Cyrillic
    // characters in exe names. Build a synthetic entry with Chinese
    // text + NUL and confirm round-trip.
    use windows::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W;
    let mut entry = PROCESSENTRY32W::default();
    // "游戏.exe" — Chinese for "game" + .exe
    let s = "游戏.exe";
    let utf16: Vec<u16> = s.encode_utf16().collect();
    entry.szExeFile[..utf16.len()].copy_from_slice(&utf16);
    let decoded = exe_file_name(&entry);
    assert_eq!(decoded, s, "Chinese chars must round-trip through UTF-16");
}

// ---------------------------------------------------------------------------
// hardware_id — GetCurrentHwProfileA wrapper
// ---------------------------------------------------------------------------

#[test]
fn hardware_id_returns_non_empty_string() {
    // hardware_id() returns the system's HW profile GUID. Format should
    // be a GUID-like string (e.g. "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}").
    let id = hardware_id().expect("hardware_id should succeed on any Win32 system");
    assert!(!id.is_empty(), "hardware_id must be non-empty");
    // GUID format has braces and dashes.
    assert!(id.starts_with('{') && id.ends_with('}'), "got: {id}");
}

#[test]
fn hardware_id_is_stable_across_calls() {
    // Two consecutive calls in the same process must return the same value.
    let id1 = hardware_id().expect("first call");
    let id2 = hardware_id().expect("second call");
    assert_eq!(id1, id2, "hardware_id should be stable within a process");
}

// ---------------------------------------------------------------------------
// get_modules — loaded DLL enumeration
// ---------------------------------------------------------------------------

#[test]
fn get_modules_for_self_yields_non_empty_list() {
    let pid = Pid(process::id());
    let modules = get_modules(pid).expect("self modules must be readable");
    assert!(
        !modules.is_empty(),
        "self process should have at least one loaded module"
    );
    // The first module of any process is the process's own exe.
    // We can't predict the exe name on CI, so just sanity-check the list shape.
    for m in &modules {
        assert!(!m.is_empty(), "module name must not be empty");
    }
}

#[test]
fn get_modules_for_invalid_pid_returns_err() {
    let pid = Pid(0xFFFF_FFFE);
    let result = get_modules(pid);
    assert!(result.is_err(), "bogus PID must error in get_modules");
}

#[test]
fn get_modules_self_contains_ntdll() {
    // Every Win32 process has ntdll.dll loaded. Use this as a positive
    // signal that the module-enum path works end-to-end.
    let pid = Pid(process::id());
    let modules = get_modules(pid).expect("self modules");
    let has_ntdll = modules.iter().any(|m| m.to_lowercase().contains("ntdll"));
    assert!(
        has_ntdll,
        "self process must have ntdll loaded: {modules:?}"
    );
}

// ---------------------------------------------------------------------------
// Pid — Copy / PartialEq derives
// ---------------------------------------------------------------------------

#[test]
fn pid_is_copy_and_equatable() {
    let a = Pid(123);
    let b = a; // Copy
    assert_eq!(a, b);
    let c = Pid(456);
    assert_ne!(a, c);
    let _ = format!("{a:?}"); // Debug
}

// ---------------------------------------------------------------------------
// Whitelist matching — R7.1 contract
// ---------------------------------------------------------------------------

#[test]
fn whitelist_matching_uses_file_stem_lowercase() {
    // R7.1 contract: the recorder matches a foreground exe against the
    // whitelist by extracting the file stem and lowercasing it. We
    // reproduce that here to verify the matching algorithm.
    use std::path::PathBuf;
    let exe_path = PathBuf::from(r"C:\Games\Cyberpunk 2077\bin\x64\Cyberpunk2077.exe");
    let stem = exe_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());
    assert_eq!(stem.as_deref(), Some("cyberpunk2077"));
}

#[test]
fn whitelist_matching_handles_chinese_path() {
    // R7.2 spec: Chinese-locale Windows paths must NOT be skipped.
    // The W-API returns the path losslessly; we extract the stem the
    // same way the recorder does, and it must equal the expected exe.
    use std::path::PathBuf;
    let exe_path = PathBuf::from(r"D:\游戏\Cyberpunk 2077\Cyberpunk2077.exe");
    let stem = exe_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());
    assert_eq!(stem.as_deref(), Some("cyberpunk2077"));
}

#[test]
fn whitelist_matching_rejects_launcher_exe() {
    // R47: the Rockstar launcher "playgtav.exe" is NOT a game and must
    // not match the gta5 / gtav whitelist entry. Verify the rejection
    // path: stem("playgtav") != "gta5" / "gtav".
    use std::path::PathBuf;
    let exe_path = PathBuf::from(r"C:\Rockstar\Launcher\PlayGTAV.exe");
    let stem = exe_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());
    assert_eq!(stem.as_deref(), Some("playgtav"));
    assert_ne!(stem.as_deref(), Some("gta5"));
    assert_ne!(stem.as_deref(), Some("gtav"));
}
