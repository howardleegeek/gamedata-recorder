use color_eyre::Result;
use serde::{Deserialize, Serialize};
use sysinfo::System;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CpuSpecs {
    pub name: String,
    pub cores: usize,
    pub frequency_mhz: u64,
    pub vendor: String,
    pub brand: String,
}

/// GPU information. `driver_version` is `None` on platforms where we can't
/// query it cheaply (currently: anything that isn't Windows). On Windows the
/// caller populates it from DXGI / Win32 — `windows::Win32::Devices::Display`
/// — via [`enrich_gpu_specs_with_driver_version`] before this struct is
/// serialized.
///
/// Wire-format compatibility: old recordings serialized without
/// `driver_version`. The `skip_serializing_if`/`default` combo means missing
/// fields round-trip cleanly, so v2.5.x consumers don't break and v2.5.x
/// recordings still deserialize into the new shape.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GpuSpecs {
    pub name: String,
    pub vendor: String,
    /// Driver version string as reported by the OS (e.g. `"31.0.15.5222"` on
    /// Windows). `None` when the platform/host doesn't expose this, when
    /// querying failed, or for historical recordings predating R5.2. PRD R5.2
    /// — "GPU model + driver version" — is satisfied by populating this on
    /// Windows; non-Windows hosts emit name+vendor only.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub driver_version: Option<String>,
}
impl GpuSpecs {
    pub fn from_name(name: &str) -> Self {
        let name_lower = name.to_lowercase();
        let vendor = if name_lower.contains("nvidia") {
            "NVIDIA"
        } else if name_lower.contains("amd") || name_lower.contains("radeon") {
            "AMD"
        } else if name_lower.contains("intel") {
            "Intel"
        } else {
            "Unknown"
        };

        Self {
            name: name.to_string(),
            vendor: vendor.to_string(),
            driver_version: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SystemSpecs {
    pub os_name: String,
    pub os_version: String,
    pub kernel_version: String,
    pub hostname: String,
    pub total_memory_gb: f64,
    /// Build number portion of the OS version (e.g. `"22631"` for Windows 11
    /// 23H2). PRD R5.2 — "OS build" — distinguishes between user-facing
    /// version (already captured in `os_version`) and the kernel build number
    /// that drivers and crash-bucket analytics key on. `None` on platforms
    /// where build-number identification is meaningless (macOS major builds
    /// are already in `os_version`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub os_build: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HardwareSpecs {
    pub cpu: CpuSpecs,
    pub gpus: Vec<GpuSpecs>,
    pub system: SystemSpecs,
    /// Primary monitor resolution captured at recording start (`(width,
    /// height)` in pixels). PRD R5.2 — "primary monitor resolution" — covers
    /// the workspace-DPI-aware display the user is gaming on. Distinct from
    /// `game_resolution` (the game's backbuffer, set per-title) and
    /// `capture_resolution` (what we encode). `None` when monitor enumeration
    /// failed or this is a non-Windows build.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub primary_monitor_resolution: Option<(u32, u32)>,
}

pub fn get_hardware_specs(gpus: Vec<GpuSpecs>) -> Result<HardwareSpecs> {
    let mut sys = System::new_all();
    sys.refresh_all();

    // CPU info
    let cpu_info = sys
        .cpus()
        .first()
        .ok_or_else(|| color_eyre::eyre::eyre!("No CPU information available"))?;

    let cpu_specs = CpuSpecs {
        name: cpu_info.name().to_string(),
        cores: sys.cpus().len(),
        frequency_mhz: cpu_info.frequency(),
        vendor: cpu_info.vendor_id().to_string(),
        brand: cpu_info.brand().to_string(),
    };

    // OS build number (PRD R5.2). On Windows the `kernel_version` from
    // sysinfo *is* the build number string (e.g. "22631" for 23H2). On
    // non-Windows we emit `None` so downstream sees "we don't know" rather
    // than a recycled kernel version with mismatched semantics.
    let os_build = compute_os_build(&System::kernel_version());

    // System info
    let system_specs = SystemSpecs {
        os_name: System::name().unwrap_or_else(|| "Unknown".to_string()),
        os_version: System::os_version().unwrap_or_else(|| "Unknown".to_string()),
        kernel_version: System::kernel_version().unwrap_or_else(|| "Unknown".to_string()),
        hostname: System::host_name().unwrap_or_else(|| "Unknown".to_string()),
        total_memory_gb: sys.total_memory() as f64 / (1024.0 * 1024.0 * 1024.0),
        os_build,
    };

    Ok(HardwareSpecs {
        cpu: cpu_specs,
        gpus,
        system: system_specs,
        primary_monitor_resolution: get_primary_monitor_resolution(),
    })
}

/// Derive the OS build number from the kernel-version string.
///
/// Cross-platform pure function so it can be tested without a Win32 import.
///   - On Windows, `sysinfo::System::kernel_version()` returns just the build
///     number, e.g. `"22631"`. We pass it through verbatim.
///   - On macOS the kernel version is a Darwin string like `"23.4.0"` — that
///     isn't a Windows-style "build number" in the sense the buyer asked for,
///     so we return `None` to be honest.
///   - On Linux it's the kernel release (e.g. `"6.1.0-23-amd64"`); same
///     reasoning — return `None`.
///
/// Inputs from real captures motivated this shape: keep the Windows path
/// trivial, refuse to fabricate on others.
pub fn compute_os_build(kernel_version: &Option<String>) -> Option<String> {
    let v = kernel_version.as_ref()?;
    // Windows kernel_version from sysinfo is a bare integer string (no dots,
    // no dashes). Validate that shape — if it doesn't match, return None so
    // we never emit a Darwin or Linux version under an `os_build` key that
    // implies Windows semantics.
    if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) {
        Some(v.clone())
    } else {
        None
    }
}

/// Enrich GPU entries with their driver version on Windows. No-op on other
/// platforms.
///
/// On Windows we walk the `EnumDisplayDevicesW` chain and substring-match
/// each entry's `DeviceString` against the friendly name supplied in
/// `gpu.name`. When we hit, we attempt to parse a version-shaped substring
/// (`X.Y` / `X.Y.Z.W`) out of the `DeviceString` itself.
///
/// Why this strategy: `windows`-crate v0.62 in this workspace doesn't enable
/// the `Win32_Graphics_Dxgi` feature (see the audit note in
/// `metadata_writer.rs::GpuInfo::from_adapters`), so we can't go through
/// `IDXGIFactory6::EnumAdapters1` to read the driver UMD version directly.
/// Reading the registry is also possible but pulls in `Win32_System_Registry`
/// — same audit constraint says no new features. The `DeviceString`-pattern
/// path is conservative: it never fabricates and returns `None` (`gpu.driver_version`
/// stays `None`) when no version pattern is found, so downstream consumers
/// see "we tried but couldn't tell" rather than a fake number.
///
/// Failures are silent — the field stays `None`. Caller is the
/// `local_recording.rs` metadata path which runs once at session stop;
/// throwing here would invalidate an otherwise-good recording over a
/// cosmetic field.
#[cfg(target_os = "windows")]
pub fn enrich_gpu_specs_with_driver_version(gpus: &mut [GpuSpecs]) {
    use windows::Win32::Graphics::Gdi::{DISPLAY_DEVICEW, EnumDisplayDevicesW};
    use windows::core::PCWSTR;

    for gpu in gpus.iter_mut() {
        if gpu.driver_version.is_some() {
            continue;
        }
        let mut device_index: u32 = 0;
        loop {
            let mut dd = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            // `EnumDisplayDevicesW` returns a `windows::Win32::Foundation::BOOL`
            // which is just `i32`-newtyped; `.as_bool()` is the canonical
            // truthiness check on this `windows`-crate version.
            let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), device_index, &mut dd, 0) };
            if !ok.as_bool() {
                break;
            }
            device_index += 1;

            let device_string = wide_to_string(&dd.DeviceString);
            // Case-insensitive substring match either way so a friendly name
            // "NVIDIA GeForce RTX 4090" matches a DeviceString
            // "NVIDIA GeForce RTX 4090 Laptop GPU" and vice versa.
            let name_lc = gpu.name.to_lowercase();
            let device_lc = device_string.to_lowercase();
            if !name_lc.contains(&device_lc) && !device_lc.contains(&name_lc) {
                continue;
            }

            // Parse "vX.Y.Z[.W]" out of the DeviceString. Many vendors embed
            // the driver build (e.g. AMD includes it in some channels;
            // Intel includes the UMD build for Arc cards). When the
            // DeviceString doesn't carry a version, we leave the field as
            // None and break out — falling back to "we don't know" is
            // strictly more honest than emitting a packed `dmDriverVersion`
            // which is a Win9x relic most modern drivers leave at 0.
            if let Some(parsed) = parse_driver_version_from_device_string(&device_string) {
                gpu.driver_version = Some(parsed);
            }
            break;
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn enrich_gpu_specs_with_driver_version(_gpus: &mut [GpuSpecs]) {
    // No-op: non-Windows hosts can't ship recordings to the buyer anyway,
    // and we'd rather emit a clean `null` than fabricate a fake version.
}

/// Find an `X.Y.Z[.W]` pattern in `s`. Used as a fallback for GPU driver
/// version extraction when neither DXGI nor `DEVMODEW.dmDriverVersion`
/// surfaces a useful number.
pub fn parse_driver_version_from_device_string(s: &str) -> Option<String> {
    // Scan for any digits-and-dots run; pick the FIRST one that contains a
    // dot. A pure-digit run with no dot is a model number ("RTX 4090"); only
    // runs with at least one dot count as a version.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip non-digit prefix.
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // Capture the digits-and-dots run starting at `i`.
        let start = i;
        let mut end = i;
        let mut dot_count = 0;
        while end < bytes.len() {
            let b = bytes[end];
            if b.is_ascii_digit() {
                end += 1;
            } else if b == b'.' {
                dot_count += 1;
                end += 1;
            } else {
                break;
            }
        }
        // Found a run; keep it only if it has at least one dot.
        if dot_count >= 1 && end > start {
            let slice = &s[start..end];
            // Trim a trailing dot if matched (e.g. "10.10.").
            let trimmed = slice.trim_end_matches('.');
            if trimmed.contains('.') {
                return Some(trimmed.to_string());
            }
        }
        // Otherwise advance past this digit run and keep scanning for
        // another candidate.
        i = end.max(i + 1);
    }
    None
}

#[cfg(target_os = "windows")]
fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Cross-platform façade over the Windows monitor enumeration. On non-Windows
/// hosts we return `None` so callers don't need a `cfg!` at every site —
/// `HardwareSpecs::primary_monitor_resolution` is optional by design.
#[cfg(not(target_os = "windows"))]
pub fn get_primary_monitor_resolution() -> Option<(u32, u32)> {
    None
}

#[cfg(target_os = "windows")]
/// Returns the resolution of the primary monitor
pub fn get_primary_monitor_resolution() -> Option<(u32, u32)> {
    use windows::{
        Win32::{
            Foundation::POINT,
            Graphics::Gdi::{
                DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW,
                MONITORINFO, MONITORINFOEXW, MonitorFromPoint,
            },
        },
        core::PCWSTR,
    };

    // Get the primary monitor handle
    let primary_monitor = unsafe {
        MonitorFromPoint(
            POINT { x: 0, y: 0 },
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY,
        )
    };
    if primary_monitor.is_invalid() {
        return None;
    }

    // Get the monitor info
    let mut monitor_info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        GetMonitorInfoW(
            primary_monitor,
            &mut monitor_info as *mut _ as *mut MONITORINFO,
        )
    }
    .ok()
    .ok()?;

    // Get the display mode
    let mut devmode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    unsafe {
        EnumDisplaySettingsW(
            PCWSTR(monitor_info.szDevice.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut devmode,
        )
    }
    .ok()
    .ok()?;

    Some((devmode.dmPelsWidth, devmode.dmPelsHeight))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_os_build_returns_windows_build_number_verbatim() {
        // sysinfo on Windows returns the build number as a bare integer string.
        // 22631 corresponds to Windows 11 23H2 — the canonical example the PRD
        // calls out for "OS build".
        let ker = Some("22631".to_string());
        assert_eq!(compute_os_build(&ker), Some("22631".to_string()));
    }

    #[test]
    fn compute_os_build_rejects_darwin_kernel_version() {
        // macOS kernel_version is the Darwin release. We refuse to emit it
        // under an `os_build` key — the field implies Windows semantics, and
        // a 24.x string would mislead downstream tooling.
        let ker = Some("24.0.0".to_string());
        assert_eq!(compute_os_build(&ker), None);
    }

    #[test]
    fn compute_os_build_rejects_linux_kernel_release() {
        let ker = Some("6.1.0-23-amd64".to_string());
        assert_eq!(compute_os_build(&ker), None);
    }

    #[test]
    fn compute_os_build_handles_missing_kernel_version() {
        // sysinfo returns None for kernel_version on sandboxed/jailed hosts.
        assert_eq!(compute_os_build(&None), None);
    }

    #[test]
    fn compute_os_build_rejects_empty_string() {
        let ker = Some(String::new());
        assert_eq!(compute_os_build(&ker), None);
    }

    #[test]
    fn parse_driver_version_extracts_simple_version() {
        // Common vendor pattern: device name embeds the driver build.
        let s = "NVIDIA GeForce RTX 4090 (32.0.15.5612)";
        assert_eq!(
            parse_driver_version_from_device_string(s),
            Some("32.0.15.5612".to_string())
        );
    }

    #[test]
    fn parse_driver_version_extracts_two_part_version() {
        let s = "AMD Radeon RX 7900 XTX 24.10";
        assert_eq!(
            parse_driver_version_from_device_string(s),
            Some("24.10".to_string())
        );
    }

    #[test]
    fn parse_driver_version_rejects_pure_model_number() {
        // "RTX 4090" alone is a model number, not a version — no dot.
        let s = "NVIDIA GeForce RTX 4090";
        assert_eq!(parse_driver_version_from_device_string(s), None);
    }

    #[test]
    fn parse_driver_version_returns_none_when_no_digits() {
        let s = "Intel UHD Graphics";
        assert_eq!(parse_driver_version_from_device_string(s), None);
    }

    #[test]
    fn parse_driver_version_handles_trailing_dot() {
        // Defensive: matched a "10.10." pattern at end of string.
        let s = "Driver build 10.10.";
        assert_eq!(
            parse_driver_version_from_device_string(s),
            Some("10.10".to_string())
        );
    }

    /// Regression: legacy `GpuSpecs` (no `driver_version` field) must
    /// deserialize into the new shape with `driver_version: None`. Old
    /// recordings in the wild predate R5.2 and must not break ingestion.
    #[test]
    fn gpu_specs_deserializes_legacy_wire_shape() {
        let legacy = r#"{"name":"NVIDIA GeForce RTX 4090","vendor":"NVIDIA"}"#;
        let gpu: GpuSpecs = serde_json::from_str(legacy).unwrap();
        assert_eq!(gpu.name, "NVIDIA GeForce RTX 4090");
        assert_eq!(gpu.driver_version, None);
    }

    /// Regression: when `driver_version` is `None`, the serialized JSON must
    /// not contain the field. Matches the wire shape v2.5.x consumers expect
    /// from the upstream OWL Control recorder.
    #[test]
    fn gpu_specs_skips_driver_version_when_none() {
        let gpu = GpuSpecs {
            name: "AMD Radeon RX 7900".to_string(),
            vendor: "AMD".to_string(),
            driver_version: None,
        };
        let json = serde_json::to_string(&gpu).unwrap();
        assert!(
            !json.contains("driver_version"),
            "driver_version should be absent when None, got: {json}"
        );
    }

    /// When the driver version IS populated (R5.2 path on Windows), it must
    /// serialize with its snake_case field name.
    #[test]
    fn gpu_specs_emits_driver_version_when_populated() {
        let gpu = GpuSpecs {
            name: "NVIDIA GeForce RTX 4090".to_string(),
            vendor: "NVIDIA".to_string(),
            driver_version: Some("32.0.15.5612".to_string()),
        };
        let json = serde_json::to_string(&gpu).unwrap();
        assert!(
            json.contains("\"driver_version\":\"32.0.15.5612\""),
            "got: {json}"
        );
    }

    /// Regression: legacy `SystemSpecs` (no `os_build` field) must round-trip.
    #[test]
    fn system_specs_deserializes_legacy_wire_shape() {
        let legacy = r#"{
            "os_name": "Windows",
            "os_version": "11 (10.0.22631)",
            "kernel_version": "22631",
            "hostname": "GAMERIG",
            "total_memory_gb": 32.0
        }"#;
        let ss: SystemSpecs = serde_json::from_str(legacy).unwrap();
        assert_eq!(ss.os_name, "Windows");
        assert_eq!(ss.os_build, None);
    }

    /// Regression: legacy `HardwareSpecs` (no `primary_monitor_resolution`)
    /// must round-trip.
    #[test]
    fn hardware_specs_deserializes_legacy_wire_shape() {
        let legacy = r#"{
            "cpu": {
                "name": "cpu0",
                "cores": 16,
                "frequency_mhz": 4500,
                "vendor": "GenuineIntel",
                "brand": "Intel Core i7-13700K"
            },
            "gpus": [
                {"name": "NVIDIA GeForce RTX 4090", "vendor": "NVIDIA"}
            ],
            "system": {
                "os_name": "Windows",
                "os_version": "11",
                "kernel_version": "22631",
                "hostname": "GAMERIG",
                "total_memory_gb": 32.0
            }
        }"#;
        let hs: HardwareSpecs = serde_json::from_str(legacy).unwrap();
        assert_eq!(hs.cpu.cores, 16);
        assert_eq!(hs.gpus.len(), 1);
        assert_eq!(hs.gpus[0].driver_version, None);
        assert_eq!(hs.system.os_build, None);
        assert_eq!(hs.primary_monitor_resolution, None);
    }

    /// Regression: `primary_monitor_resolution: None` must not serialize the
    /// field. v2.5.x consumers ignore unknown fields but treat presence as
    /// authoritative — we won't emit a fake `(0, 0)`.
    #[test]
    fn hardware_specs_skips_primary_monitor_resolution_when_none() {
        let hs = HardwareSpecs {
            cpu: CpuSpecs {
                name: "cpu0".to_string(),
                cores: 8,
                frequency_mhz: 3600,
                vendor: "AuthenticAMD".to_string(),
                brand: "AMD Ryzen 7".to_string(),
            },
            gpus: vec![],
            system: SystemSpecs {
                os_name: "Linux".to_string(),
                os_version: "6.1".to_string(),
                kernel_version: "6.1.0".to_string(),
                hostname: "ci".to_string(),
                total_memory_gb: 16.0,
                os_build: None,
            },
            primary_monitor_resolution: None,
        };
        let json = serde_json::to_string(&hs).unwrap();
        assert!(
            !json.contains("primary_monitor_resolution"),
            "field should be absent when None, got: {json}"
        );
    }

    /// When populated, `primary_monitor_resolution` must serialize as a JSON
    /// array `[width, height]` — same shape `game_resolution` and
    /// `capture_resolution` already use in `output_types::Metadata`.
    #[test]
    fn hardware_specs_emits_primary_monitor_resolution_as_array() {
        let hs = HardwareSpecs {
            cpu: CpuSpecs {
                name: "cpu0".to_string(),
                cores: 8,
                frequency_mhz: 3600,
                vendor: "AuthenticAMD".to_string(),
                brand: "AMD Ryzen 7".to_string(),
            },
            gpus: vec![],
            system: SystemSpecs {
                os_name: "Windows".to_string(),
                os_version: "11".to_string(),
                kernel_version: "22631".to_string(),
                hostname: "rig".to_string(),
                total_memory_gb: 32.0,
                os_build: Some("22631".to_string()),
            },
            primary_monitor_resolution: Some((3840, 2160)),
        };
        let json = serde_json::to_string(&hs).unwrap();
        // serde serializes tuples as JSON arrays.
        assert!(
            json.contains("\"primary_monitor_resolution\":[3840,2160]"),
            "got: {json}"
        );
        assert!(json.contains("\"os_build\":\"22631\""));
    }
}
