//! Cross-platform shim around the R5 metadata-polish modules so the
//! cross-platform parts compile + run on macOS/Linux CI.
//!
//! This crate source-includes modules from the top-level `gamedata-recorder`
//! crate via `#[path = ...]`:
//!
//!   - `src/system/hardware_specs.rs` (extended HardwareSpecs — R5.2)
//!   - `src/output_types/fps_stats.rs` (percentile math — R5.3)
//!
//! The hardware_specs source uses `#[cfg(target_os = "windows")]` on every
//! Win32 import, so it compiles cleanly on macOS — the Windows-only
//! `get_primary_monitor_resolution` / `enrich_gpu_specs_with_driver_version`
//! paths become no-ops, while the pure-logic helpers
//! (`compute_os_build`, `parse_driver_version_from_device_string`) compile
//! the same on both targets.
//!
//! The fps_stats source is fully cross-platform — pure numerical code with
//! no Win32 / OBS deps.
//!
//! A subsequent commit in this branch extends this lib.rs to also
//! source-include `src/util/durable_write.rs` (R5.6) via the same
//! `#[path = ...]` pattern.

#[path = "../../../src/system/hardware_specs.rs"]
pub mod hardware_specs;

#[path = "../../../src/output_types/fps_stats.rs"]
pub mod fps_stats;

// Re-export the public surface integration tests use so they don't have to
// drill into module paths.
pub use fps_stats::FpsStats;
pub use hardware_specs::{
    CpuSpecs, GpuSpecs, HardwareSpecs, SystemSpecs, compute_os_build,
    enrich_gpu_specs_with_driver_version, get_primary_monitor_resolution,
    parse_driver_version_from_device_string,
};
