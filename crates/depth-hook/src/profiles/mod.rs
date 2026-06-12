//! Per-title depth-hook profiles.
//!
//! Each module here corresponds to exactly one game engine + title
//! combination. Profiles declare their detection heuristic, expected
//! depth format, and matrix conventions; the `crate::dx12` layer is
//! profile-agnostic and just does what the active profile tells it to.
//!
//! Adding a new profile is a three-step process; see the crate README
//! for the template and for a worked Alan Wake 2 example.

pub mod common;
pub mod cyberpunk2077;

use crate::types::{DepthFormat, DetectionHeuristic, Matrix4};

/// Trait each game profile implements.
///
/// All methods must be cheap and pure — they are called from the DX12
/// hook thread once per captured frame, and any allocation or syscall
/// there shows up as a pacing hitch inside the target process. Anything
/// expensive belongs in the profile's constructor (or as a `const`).
pub trait DepthHookProfile: Send + Sync {
    /// Human-readable name of this profile, e.g. `"Cyberpunk 2077 (REDengine 4, DX12)"`.
    /// Used only for logs and telemetry; not parsed anywhere.
    fn name(&self) -> &'static str;

    /// Lowercase executable stems (no `.exe`) that this profile applies to.
    ///
    /// Must match the casing convention used by
    /// `constants::GAME_WHITELIST` and by the recorder's
    /// `tokio_thread::get_foregrounded_game`, both of which normalise via
    /// `file_stem().to_lowercase()` before comparison.
    fn game_exe_stems(&self) -> &[&str];

    /// Heuristic the capture layer should use to pick the canonical
    /// depth buffer out of the many DSVs bound per frame. See
    /// [`DetectionHeuristic`] for the full contract.
    fn detection_heuristic(&self) -> DetectionHeuristic;

    /// Typed depth format of the canonical depth buffer for this title.
    /// Used to size the CPU-readback heap and to reject unrelated DSVs
    /// (shadow atlases, decal masks, etc.) that happened to pass the
    /// aspect-ratio and clear-count checks.
    fn depth_format(&self) -> DepthFormat;

    /// Whether this title renders with reverse-Z + infinite far plane.
    /// Downstream training tooling needs this to reconstruct world-space
    /// positions from `(u, v, depth)` tuples correctly.
    fn reverse_z(&self) -> bool;

    /// Extract `(near, far)` from a captured projection matrix.
    ///
    /// For profiles where `reverse_z() == true` this will usually just
    /// delegate to `common::reverse_z_infinite_far_near`. Profiles with
    /// unusual matrix conventions (reverse-Z with finite far, oblique
    /// near plane, tilt-shift projections) override this.
    fn near_far_from_matrix(&self, proj: &Matrix4) -> (f32, f32);
}

/// Registry of all known profiles.
///
/// Single source of truth for "does the recorder have depth-capture for
/// this game?". The recorder consults this by executable stem each time
/// a new foreground game is detected; a `Some(profile)` return means
/// depth capture can be turned on for that title.
pub struct ProfileRegistry {
    profiles: Vec<std::sync::Arc<dyn DepthHookProfile>>,
}

impl ProfileRegistry {
    /// Build a registry containing every profile shipped in this crate.
    ///
    /// This is where new profile implementations get wired in. Order
    /// does not matter for correctness (lookup is by exe stem, and exe
    /// stems are unique across titles) but earlier entries are scanned
    /// first, so put the hottest / most-played titles near the top.
    pub fn with_builtin_profiles() -> Self {
        let profiles: Vec<std::sync::Arc<dyn DepthHookProfile>> =
            vec![std::sync::Arc::new(cyberpunk2077::Cyberpunk2077)];
        Self { profiles }
    }

    /// Empty registry, useful for tests.
    pub fn empty() -> Self {
        Self {
            profiles: Vec::new(),
        }
    }

    /// Manually register a profile. Primarily for tests and for
    /// downstream users who want to ship additional profiles without
    /// forking this crate.
    pub fn register(&mut self, profile: std::sync::Arc<dyn DepthHookProfile>) {
        self.profiles.push(profile);
    }

    /// Look up a profile by executable stem (lowercase, no `.exe`).
    ///
    /// Returns `None` if no registered profile claims this stem.
    pub fn find_for_exe_stem(&self, stem: &str) -> Option<std::sync::Arc<dyn DepthHookProfile>> {
        let stem_lower = stem.to_lowercase();
        self.profiles
            .iter()
            .find(|p| p.game_exe_stems().iter().any(|s| *s == stem_lower))
            .cloned()
    }

    /// Total number of registered profiles. Used by tests and by the
    /// recorder's startup log ("depth-hook ready, N profiles loaded").
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// True if this registry has no profiles registered.
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self::with_builtin_profiles()
    }
}
