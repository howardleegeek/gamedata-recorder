//! Disk Type Detection
//!
//! Inspects the physical drive backing a given path and classifies it as
//! NVMe SSD, SATA SSD, SATA HDD, USB, or Unknown.
//!
//! Implementation:
//!   1. `GetVolumePathNameW` to find the volume root for the recording path
//!      (e.g. `C:\`), since `GetDriveTypeW` wants a root path, not a file.
//!   2. `GetDriveTypeW` as a cheap first filter (removable vs fixed).
//!   3. Convert the volume to a `\\.\X:` handle and query
//!      `IOCTL_STORAGE_QUERY_PROPERTY` twice:
//!        - `StorageAdapterProperty` → bus type (NVMe, SATA, USB, ...)
//!        - `StorageDeviceSeekPenaltyProperty` → seek penalty (HDD vs SSD)
//!
//! Previously the recording_drive field was hardcoded to "NVMe SSD"
//! regardless of the user's actual drive. Per audit: downstream AI training
//! pipelines use this to weight disk-I/O-bound vs CPU-bound quality signals,
//! and the stub was poisoning that signal.
//!
//! On non-Windows or when any Win32 call fails, returns `DiskType::Unknown`.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Physical drive classification for the disk backing a given path.
///
/// Serialization values match the legacy wire format — `"NVMe SSD"`,
/// `"SATA SSD"`, `"SATA HDD"`, `"USB"`, `"Unknown"` — so downstream
/// backend/analyst code keyed on string values keeps working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskType {
    #[serde(rename = "NVMe SSD")]
    NvmeSsd,
    #[serde(rename = "SATA SSD")]
    SataSsd,
    #[serde(rename = "SATA HDD")]
    SataHdd,
    #[serde(rename = "USB")]
    Usb,
    #[serde(rename = "Unknown")]
    Unknown,
}

impl DiskType {
    pub fn as_str(&self) -> &'static str {
        match self {
            DiskType::NvmeSsd => "NVMe SSD",
            DiskType::SataSsd => "SATA SSD",
            DiskType::SataHdd => "SATA HDD",
            DiskType::Usb => "USB",
            DiskType::Unknown => "Unknown",
        }
    }
}

impl std::fmt::Display for DiskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Detect the physical drive type backing `path`.
///
/// Returns `DiskType::Unknown` on any error (missing path, permission denied,
/// non-Windows build) — never panics. The caller can safely use the result
/// directly in metadata without handling `Result`.
#[cfg(windows)]
pub fn detect_disk_type(path: &Path) -> DiskType {
    match detect_disk_type_impl(path) {
        Ok(dt) => dt,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "detect_disk_type failed, returning Unknown");
            DiskType::Unknown
        }
    }
}

/// Non-Windows stub — this application is Windows-only, but CI may lint on Linux.
#[cfg(not(windows))]
pub fn detect_disk_type(_path: &Path) -> DiskType {
    DiskType::Unknown
}

#[cfg(windows)]
fn detect_disk_type_impl(path: &Path) -> Result<DiskType, String> {
    use std::os::windows::ffi::OsStrExt;

    use windows::{
        Win32::{
            Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
            Storage::FileSystem::{
                BusTypeNvme, BusTypeSata, BusTypeUsb, CreateFileW, FILE_FLAGS_AND_ATTRIBUTES,
                FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetVolumePathNameW, OPEN_EXISTING,
                STORAGE_BUS_TYPE,
            },
            System::{
                Ioctl::{STORAGE_PROPERTY_ID, StorageDeviceSeekPenaltyProperty},
                WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE},
            },
        },
        core::PCWSTR,
    };

    // STORAGE_PROPERTY_ID::StorageAdapterProperty — windows-0.62.2 doesn't
    // re-export this constant under the same name convention, but its value
    // is stable: 1. Documented in wdm.h / ntddstor.h.
    const STORAGE_ADAPTER_PROPERTY: STORAGE_PROPERTY_ID = STORAGE_PROPERTY_ID(1);
    // PropertyStandardQuery = 0 (also stable, documented enum value).
    const PROPERTY_STANDARD_QUERY: windows::Win32::System::Ioctl::STORAGE_QUERY_TYPE =
        windows::Win32::System::Ioctl::STORAGE_QUERY_TYPE(0);

    // Canonicalize the path — the caller may hand us a relative or symlinked
    // path. If the path doesn't exist we still try the drive letter since
    // recording_location may not exist yet on first run.
    let canonical = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    // Convert to UTF-16 null-terminated wide string for Windows APIs.
    let wide: Vec<u16> = canonical
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // Get the volume root (e.g. `C:\`) — GetDriveTypeW needs a root path.
    let mut volume_root = [0u16; 260];
    unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut volume_root) }
        .map_err(|e| format!("GetVolumePathNameW failed: {e}"))?;

    // Cheap filter: is this a real fixed/removable drive, not a network share?
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(volume_root.as_ptr())) };
    if drive_type != DRIVE_FIXED && drive_type != DRIVE_REMOVABLE {
        // Network drives, CD-ROMs, RAM disks — we don't classify these.
        return Ok(DiskType::Unknown);
    }

    // Build a `\\.\X:` device path from the drive letter. The volume root
    // looks like `"C:\\\0"`; we want `"\\\\.\\C:\0"`.
    let drive_letter = volume_root
        .iter()
        .take_while(|&&c| c != 0 && c != b':' as u16)
        .copied()
        .next()
        .ok_or_else(|| "Volume root has no drive letter".to_string())?;

    let device_path: Vec<u16> = "\\\\.\\"
        .encode_utf16()
        .chain(std::iter::once(drive_letter))
        .chain(":\0".encode_utf16())
        .collect();

    // Open the volume with no access rights (0) — IOCTL queries don't need
    // read/write, and requiring GENERIC_READ would force elevated privileges.
    let handle: HANDLE = unsafe {
        CreateFileW(
            PCWSTR(device_path.as_ptr()),
            0, // no access needed for IOCTL query
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .map_err(|e| format!("CreateFileW(\\\\.\\{}:) failed: {e}", drive_letter as u8 as char))?;
    if handle.is_invalid() || handle == INVALID_HANDLE_VALUE {
        return Err("CreateFileW returned INVALID_HANDLE_VALUE".to_string());
    }

    // Wrap the handle so it's closed on any exit path.
    struct CloseOnDrop(HANDLE);
    impl Drop for CloseOnDrop {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    let _guard = CloseOnDrop(handle);

    // Query 1: bus type (NVMe / SATA / USB / ...)
    let bus_type = query_adapter_bus_type(handle, STORAGE_ADAPTER_PROPERTY, PROPERTY_STANDARD_QUERY)
        .unwrap_or(STORAGE_BUS_TYPE(0));

    // Query 2: seek penalty (HDD has seek penalty, SSD does not)
    let incurs_seek_penalty = query_seek_penalty(
        handle,
        StorageDeviceSeekPenaltyProperty,
        PROPERTY_STANDARD_QUERY,
    );

    // Classify. Bus type is authoritative for NVMe and USB; for SATA/ATA we
    // also need the seek-penalty flag to distinguish SSD from HDD. Use
    // if/else rather than match-guards because `STORAGE_BUS_TYPE` is a
    // newtype with no `Ord`, so exhaustive pattern matching on consts
    // requires wildcard fallthrough anyway.
    let classification = if bus_type == BusTypeNvme {
        DiskType::NvmeSsd
    } else if bus_type == BusTypeUsb {
        DiskType::Usb
    } else if bus_type == BusTypeSata {
        match incurs_seek_penalty {
            Some(true) => DiskType::SataHdd,
            Some(false) => DiskType::SataSsd,
            None => DiskType::Unknown,
        }
    } else {
        // Unknown bus (RAID, iSCSI, Virtual, etc.) — fall back to seek-
        // penalty to at least distinguish spinning vs solid state.
        match incurs_seek_penalty {
            Some(true) => DiskType::SataHdd,
            Some(false) => DiskType::SataSsd,
            None => DiskType::Unknown,
        }
    };

    Ok(classification)
}

#[cfg(windows)]
fn query_adapter_bus_type(
    handle: windows::Win32::Foundation::HANDLE,
    property_id: windows::Win32::System::Ioctl::STORAGE_PROPERTY_ID,
    query_type: windows::Win32::System::Ioctl::STORAGE_QUERY_TYPE,
) -> Option<windows::Win32::Storage::FileSystem::STORAGE_BUS_TYPE> {
    use windows::Win32::System::{
        IO::DeviceIoControl,
        Ioctl::{IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_ADAPTER_DESCRIPTOR, STORAGE_PROPERTY_QUERY},
    };

    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: property_id,
        QueryType: query_type,
        AdditionalParameters: [0],
    };

    let mut descriptor = STORAGE_ADAPTER_DESCRIPTOR::default();
    let mut bytes_returned: u32 = 0;

    let result = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const core::ffi::c_void),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut descriptor as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<STORAGE_ADAPTER_DESCRIPTOR>() as u32,
            Some(&mut bytes_returned),
            None,
        )
    };

    if result.is_ok() && bytes_returned > 0 {
        // BusType field is a u8 on the descriptor; convert to the typed enum.
        Some(windows::Win32::Storage::FileSystem::STORAGE_BUS_TYPE(
            descriptor.BusType as i32,
        ))
    } else {
        None
    }
}

#[cfg(windows)]
fn query_seek_penalty(
    handle: windows::Win32::Foundation::HANDLE,
    property_id: windows::Win32::System::Ioctl::STORAGE_PROPERTY_ID,
    query_type: windows::Win32::System::Ioctl::STORAGE_QUERY_TYPE,
) -> Option<bool> {
    use windows::Win32::System::{
        IO::DeviceIoControl,
        Ioctl::{
            DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY,
        },
    };

    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: property_id,
        QueryType: query_type,
        AdditionalParameters: [0],
    };

    let mut descriptor = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
    let mut bytes_returned: u32 = 0;

    let result = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const core::ffi::c_void),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut descriptor as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            Some(&mut bytes_returned),
            None,
        )
    };

    if result.is_ok() && bytes_returned > 0 {
        Some(descriptor.IncursSeekPenalty)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_type_serializes_to_legacy_wire_format() {
        // Downstream backend / analyst code keyed on these exact strings.
        // Changing them is a breaking wire change, not allowed by the spec.
        assert_eq!(
            serde_json::to_string(&DiskType::NvmeSsd).unwrap(),
            "\"NVMe SSD\""
        );
        assert_eq!(
            serde_json::to_string(&DiskType::SataSsd).unwrap(),
            "\"SATA SSD\""
        );
        assert_eq!(
            serde_json::to_string(&DiskType::SataHdd).unwrap(),
            "\"SATA HDD\""
        );
        assert_eq!(serde_json::to_string(&DiskType::Usb).unwrap(), "\"USB\"");
        assert_eq!(
            serde_json::to_string(&DiskType::Unknown).unwrap(),
            "\"Unknown\""
        );
    }

    #[test]
    fn disk_type_as_str_matches_serde_name() {
        // Defensive: Display / as_str / serde must agree so log output and
        // serialized output never diverge.
        for variant in [
            DiskType::NvmeSsd,
            DiskType::SataSsd,
            DiskType::SataHdd,
            DiskType::Usb,
            DiskType::Unknown,
        ] {
            let serialized = serde_json::to_string(&variant).unwrap();
            let expected = format!("\"{}\"", variant.as_str());
            assert_eq!(serialized, expected, "mismatch for {variant:?}");
        }
    }

    /// On any modern Windows dev machine the current working directory is
    /// on a real local drive (NVMe, SATA SSD, or SATA HDD) — so the detector
    /// must return a non-Unknown variant. If it does return Unknown on a
    /// dev box, the Win32 IOCTL plumbing is broken and the test should fail
    /// loudly rather than letting bad metadata ship silently.
    #[cfg(windows)]
    #[test]
    fn detect_disk_type_on_cwd_is_real_drive() {
        let cwd = std::env::current_dir().expect("cwd should exist");
        let dt = detect_disk_type(&cwd);
        assert_ne!(
            dt,
            DiskType::Unknown,
            "detect_disk_type returned Unknown for cwd {}; Win32 IOCTL plumbing is broken",
            cwd.display()
        );
    }

    /// On non-Windows the stub must return Unknown — the whole app is
    /// Windows-only but CI may run `cargo check` / `cargo test` on Linux.
    #[cfg(not(windows))]
    #[test]
    fn detect_disk_type_returns_unknown_on_non_windows() {
        let cwd = std::env::current_dir().expect("cwd should exist");
        assert_eq!(detect_disk_type(&cwd), DiskType::Unknown);
    }
}
