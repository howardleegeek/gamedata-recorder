use embed_manifest::{embed_manifest, manifest::DpiAwareness, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // Declare per-monitor DPI awareness V2 so Windows does NOT virtualize our
        // monitor-capture resolution on high-DPI displays (125% / 150% / 200%).
        //
        // Without this, a 4K monitor scaled at 150% hands us a 2560x1440 virtualized
        // surface and input coordinates, so we record a lower-res video than the
        // user actually sees and mouse coords are off. See MEGA_AUDIT R39 and
        // TRIAGE "DPI-awareness manifest for the whole app".
        //
        // `PerMonitorV2` emits BOTH the modern `<dpiAwareness>permonitorv2,permonitor</dpiAwareness>`
        // (Windows 10 1607+) and the legacy `<dpiAware>true/pm</dpiAware>` fallback
        // (Windows 8.1 + older 10 builds), which is what we want for broad coverage.
        embed_manifest(
            new_manifest("WayfarerLabs.OwlControl").dpi_awareness(DpiAwareness::PerMonitorV2),
        )
        .expect("unable to embed manifest file");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
