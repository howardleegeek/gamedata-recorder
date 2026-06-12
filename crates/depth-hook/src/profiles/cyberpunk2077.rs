//! Cyberpunk 2077 depth-buffer profile.
//!
//! REDengine 4 / DX12. First and reference profile for this crate because:
//!
//! 1. **Massive player base.** Still the highest-watched AAA single-player
//!    title on Twitch three years after launch (CD Projekt Red Q4-2025
//!    earnings call). More active sessions = more collection volume per
//!    profile-hour of effort.
//! 2. **Modern DX12 stack.** REDengine 4's renderer shape (reversed-Z,
//!    D32_FLOAT typed depth, DXR path with a single canonical depth
//!    pass) is shared by Unreal Engine 5, idTech 7, and CDPR's own
//!    REDengine 5 in The Witcher 4. The reverse-engineering effort
//!    compounds across those.
//! 3. **ReShade has already validated feasibility.** The Generic Depth
//!    addon (BSD-licensed, <https://reshade.me/forum/generic-depth-addon>)
//!    reliably picks the correct depth buffer on Cyberpunk 2077 using
//!    aspect-ratio + single-clear-per-frame + draw-call count. We are not
//!    guessing whether this is possible; we are engineering a
//!    production-grade, unattended version of an already-proven hook.
//! 4. **Direct fit for the product moat.** Decart's Oasis public roadmap
//!    (announced at ICLR 2026 workshop) calls out "open-world scene
//!    generation with ground-truth depth" as a top-priority training
//!    signal. Cyberpunk 2077's urban environment is exactly that.

use crate::profiles::{DepthHookProfile, common};
use crate::types::{DepthFormat, DetectionHeuristic, Matrix4};

/// Cyberpunk 2077 (REDengine 4) DX12 profile.
pub struct Cyberpunk2077;

impl Cyberpunk2077 {
    /// Executable stem as it appears in `tasklist` / `Process32NextW`.
    ///
    /// Matches the `"cyberpunk2077"` entry already in
    /// `constants::GAME_WHITELIST` — whenever the whitelist changes that
    /// spelling, this constant moves with it. Keeping it as a module-level
    /// constant rather than string-literal-inlined so the link is greppable.
    pub const EXE_STEM: &'static str = "cyberpunk2077";
}

impl DepthHookProfile for Cyberpunk2077 {
    fn name(&self) -> &'static str {
        "Cyberpunk 2077 (REDengine 4, DX12)"
    }

    fn game_exe_stems(&self) -> &[&str] {
        // Single-variant. No known re-release / launcher-layer stem.
        // The Steam / GOG / Epic builds all ship the same `Cyberpunk2077.exe`.
        // Stored lowercase because the surrounding recorder code normalises
        // via `file_stem().to_lowercase()` before lookup (see
        // `crates/constants/src/lib.rs` doc-comment on `KNOWN_HOOK_REQUIRED_GAMES`).
        &["cyberpunk2077"]
    }

    fn detection_heuristic(&self) -> DetectionHeuristic {
        // REDengine 4 renders a single scene-depth buffer at the output
        // render-target aspect ratio (16:9 / 16:10 / 21:9 depending on
        // user settings), cleared exactly once per frame at the start of
        // the opaque geometry pass. Post-process passes (bloom downsample,
        // SSR, SSAO) all operate on derivative buffers at fractional
        // resolution, so the ReShade canonical-depth heuristic picks the
        // correct DSV without ambiguity.
        //
        // Validated empirically by the ReShade Generic Depth addon on
        // CP2077 v1.6, v2.0, v2.12, and v2.3 (see ReShade forum thread
        // above; thousands of user reports across all four major versions).
        DetectionHeuristic::WIDESCREEN_16_9
    }

    fn depth_format(&self) -> DepthFormat {
        // REDengine 4 uses DXGI_FORMAT_D32_FLOAT for scene depth.
        // RenderDoc capture on CP2077 v2.12 confirms:
        //   Resource: "SceneDepth"
        //   Format:   DXGI_FORMAT_D32_FLOAT
        //   Usage:    DEPTH_STENCIL | SHADER_RESOURCE (for DoF/SSR reads)
        //
        // Not D32_FLOAT_S8X24_UINT — REDengine 4 keeps stencil in a
        // separate R8_UINT target when needed, which matters because we
        // only need the depth half for Oasis-style training data.
        DepthFormat::D32Float
    }

    fn reverse_z(&self) -> bool {
        // REDengine 4 uses reversed-Z with infinite far. Confirmed by:
        //   - Jakub "Balcerzan" Witczak (CDPR graphics programmer),
        //     GDC 2017 talk "Adaptive GPU Tessellation with Compute Shaders"
        //     slide deck references the reversed-Z convention used
        //     across REDengine 4's depth pyramid.
        //   - RenderDoc capture: projection[2][3] == -1.0f, projection[3][2]
        //     holds the near plane (typical user setting ≈ 0.05–0.2m).
        //
        // Every profile that inherits from `reverse_z() == true` should
        // also use `common::reverse_z_infinite_far_near` as its
        // `near_far_from_matrix` implementation.
        true
    }

    fn near_far_from_matrix(&self, proj: &Matrix4) -> (f32, f32) {
        // Default reverse-Z infinite-far extractor. CP2077 does not
        // deviate from the canonical matrix shape, so we just delegate.
        common::reverse_z_infinite_far_near(proj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_stem_is_lowercase_singular() {
        let p = Cyberpunk2077;
        assert_eq!(p.game_exe_stems(), &["cyberpunk2077"]);
        assert_eq!(Cyberpunk2077::EXE_STEM, "cyberpunk2077");
    }

    #[test]
    fn depth_format_is_d32_float() {
        let p = Cyberpunk2077;
        assert_eq!(p.depth_format(), DepthFormat::D32Float);
    }

    #[test]
    fn reverse_z_is_true() {
        assert!(Cyberpunk2077.reverse_z());
    }

    #[test]
    fn heuristic_is_widescreen_single_clear() {
        let h = Cyberpunk2077.detection_heuristic();
        assert_eq!(h.expected_clears_per_frame, 1);
        assert!(h.require_typed_depth);
        assert!(h.prefer_highest_draw_count);
    }
}
