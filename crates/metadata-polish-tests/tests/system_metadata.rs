//! Integration tests for R5.2 — system metadata extension.
//!
//! Exercises the public surface from `hardware_specs` end-to-end on macOS/Linux.
//! Windows-only enrichment paths are exercised by the in-tree unit tests via
//! the `#[cfg(target_os = "windows")]` branches; here we cover the pure-logic
//! cross-platform code that downstream tooling depends on for parsing.

use metadata_polish_tests::{
    CpuSpecs, GpuSpecs, HardwareSpecs, SystemSpecs, compute_os_build,
    enrich_gpu_specs_with_driver_version, get_primary_monitor_resolution,
    parse_driver_version_from_device_string,
};

#[test]
fn os_build_extraction_handles_real_windows_build_string() {
    // Real-world Windows 11 23H2 build, as sysinfo emits it.
    assert_eq!(
        compute_os_build(&Some("22631".to_string())),
        Some("22631".to_string())
    );
    // Windows 10 1809 LTSC.
    assert_eq!(
        compute_os_build(&Some("17763".to_string())),
        Some("17763".to_string())
    );
    // Insider preview build.
    assert_eq!(
        compute_os_build(&Some("26100".to_string())),
        Some("26100".to_string())
    );
}

#[test]
fn os_build_rejects_non_windows_kernel_strings() {
    // Darwin (macOS 14 Sonoma).
    assert_eq!(compute_os_build(&Some("23.5.0".to_string())), None);
    // Linux kernel release.
    assert_eq!(
        compute_os_build(&Some("6.5.0-44-generic".to_string())),
        None
    );
    // FreeBSD kernel.
    assert_eq!(compute_os_build(&Some("13.2-RELEASE".to_string())), None);
}

#[test]
fn driver_version_parser_handles_vendor_string_patterns() {
    // NVIDIA shipping pattern in DEVMODE — driver build inside parens.
    assert_eq!(
        parse_driver_version_from_device_string("NVIDIA GeForce RTX 4090 [32.0.15.5612]"),
        Some("32.0.15.5612".to_string())
    );
    // AMD shipping pattern with "Driver" prefix.
    assert_eq!(
        parse_driver_version_from_device_string("Radeon RX 7900 XTX Driver 24.10.1"),
        Some("24.10.1".to_string())
    );
    // Intel Arc compact version.
    assert_eq!(
        parse_driver_version_from_device_string("Intel Arc A770 Driver 31.0.101.5333"),
        Some("31.0.101.5333".to_string())
    );
}

#[test]
fn driver_version_parser_rejects_pure_model_numbers() {
    // Bare "RTX 4090" looks like a number but isn't a driver version.
    assert_eq!(
        parse_driver_version_from_device_string("NVIDIA GeForce RTX 4090"),
        None
    );
    // "A770" — no dot.
    assert_eq!(
        parse_driver_version_from_device_string("Intel Arc A770"),
        None
    );
}

#[test]
fn enrich_gpu_specs_is_noop_on_mac_and_linux() {
    // The function has a #[cfg(not(target_os = "windows"))] no-op branch on
    // non-Windows hosts. Confirm we don't panic and don't mutate the input.
    let mut gpus = vec![
        GpuSpecs {
            name: "Apple M3".to_string(),
            vendor: "Apple".to_string(),
            driver_version: None,
        },
        GpuSpecs {
            name: "NVIDIA GeForce RTX 4090".to_string(),
            vendor: "NVIDIA".to_string(),
            driver_version: None,
        },
    ];
    enrich_gpu_specs_with_driver_version(&mut gpus);
    // No driver version filled in on macOS/Linux — these hosts can't ship
    // game recordings anyway, so None is the honest answer.
    assert_eq!(gpus[0].driver_version, None);
    assert_eq!(gpus[1].driver_version, None);
}

#[test]
fn primary_monitor_resolution_returns_none_on_non_windows() {
    // Cross-platform façade returns None on macOS/Linux so callers don't
    // need a #[cfg] at every call site.
    assert_eq!(get_primary_monitor_resolution(), None);
}

#[test]
fn full_hardware_specs_emits_r5_2_required_keys() {
    // Construct the shape we'd write to system.json — verify the buyer's
    // R5.2 required keys all appear. This is a wire-contract test against
    // the PRD: `{ CPU model, GPU model + driver version, RAM, OS build,
    // primary monitor resolution }`.
    let hs = HardwareSpecs {
        cpu: CpuSpecs {
            name: "cpu0".to_string(),
            cores: 16,
            frequency_mhz: 4500,
            vendor: "GenuineIntel".to_string(),
            brand: "Intel Core i7-13700K".to_string(),
        },
        gpus: vec![GpuSpecs {
            name: "NVIDIA GeForce RTX 4090".to_string(),
            vendor: "NVIDIA".to_string(),
            driver_version: Some("32.0.15.5612".to_string()),
        }],
        system: SystemSpecs {
            os_name: "Windows".to_string(),
            os_version: "11".to_string(),
            kernel_version: "22631".to_string(),
            hostname: "GAMERIG".to_string(),
            total_memory_gb: 32.0,
            os_build: Some("22631".to_string()),
        },
        primary_monitor_resolution: Some((3840, 2160)),
    };
    let json = serde_json::to_string(&hs).unwrap();
    // R5.2 contract: each PRD key MUST be present in the wire format.
    assert!(json.contains("\"brand\":\"Intel Core i7-13700K\""), "CPU");
    assert!(
        json.contains("\"name\":\"NVIDIA GeForce RTX 4090\""),
        "GPU name"
    );
    assert!(
        json.contains("\"driver_version\":\"32.0.15.5612\""),
        "GPU driver"
    );
    assert!(json.contains("\"total_memory_gb\":32.0"), "RAM");
    assert!(json.contains("\"os_build\":\"22631\""), "OS build");
    assert!(
        json.contains("\"primary_monitor_resolution\":[3840,2160]"),
        "monitor resolution"
    );
}

#[test]
fn full_hardware_specs_round_trips_through_json() {
    let hs = HardwareSpecs {
        cpu: CpuSpecs {
            name: "cpu0".to_string(),
            cores: 16,
            frequency_mhz: 4500,
            vendor: "GenuineIntel".to_string(),
            brand: "Intel Core i7-13700K".to_string(),
        },
        gpus: vec![GpuSpecs {
            name: "NVIDIA GeForce RTX 4090".to_string(),
            vendor: "NVIDIA".to_string(),
            driver_version: Some("32.0.15.5612".to_string()),
        }],
        system: SystemSpecs {
            os_name: "Windows".to_string(),
            os_version: "11".to_string(),
            kernel_version: "22631".to_string(),
            hostname: "GAMERIG".to_string(),
            total_memory_gb: 32.0,
            os_build: Some("22631".to_string()),
        },
        primary_monitor_resolution: Some((3840, 2160)),
    };
    let json = serde_json::to_string(&hs).unwrap();
    let parsed: HardwareSpecs = serde_json::from_str(&json).unwrap();
    assert_eq!(
        parsed.gpus[0].driver_version.as_deref(),
        Some("32.0.15.5612")
    );
    assert_eq!(parsed.system.os_build.as_deref(), Some("22631"));
    assert_eq!(parsed.primary_monitor_resolution, Some((3840, 2160)));
}
