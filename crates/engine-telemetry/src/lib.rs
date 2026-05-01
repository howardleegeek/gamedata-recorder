//! `engine-telemetry` — per-title engine-state sidecar capture.
//!
//! # Why this crate exists
//!
//! `crates/depth-hook` gives us GPU-side ground truth (depth + projection +
//! view matrices). That is half the moat: it lets a downstream training
//! pipeline unproject `(u, v, depth)` into camera-space points. The other
//! half — the half that turns a pile of camera-space points into a
//! coherent **world-space** trajectory — is engine state: where the player
//! avatar is in the world, how the third-person camera offsets from that
//! avatar (the "Follow Offset"), and the engine's metric scale (engine
//! units → meters).
//!
//! Per `docs/CAPTURE_PERFORMANCE_INVESTIGATION.md` the encoder pipeline
//! already runs at a steady 30 fps with depth capture turned on. The
//! missing axis is engine telemetry: a per-frame snapshot of player +
//! camera transforms aligned with the depth/video frame index, written
//! as a sidecar JSON next to `recording.mp4` so the training pipeline
//! can fuse them by `frame_index`.
//!
//! # Architecture
//!
//! - [`EngineFrame`] — the platform-agnostic per-frame snapshot. Pure
//!   data, `Serialize` + `Deserialize`, compiles everywhere.
//! - [`EngineHook`] — trait every per-title hook implements. The shape
//!   matches `crates/depth-hook`'s `DepthHookProfile` so the recorder
//!   can hold one of each per active title.
//! - [`CyberpunkHook`] — first concrete implementation, scaffolded with
//!   docstrings describing exactly which RTTI struct paths to read.
//!   Today it returns deterministic mock frames so the cross-platform
//!   tests can validate the rest of the plumbing; the puffydev
//!   hand-off swaps the mock body for a real RED4ext / RTTI walker
//!   under `#[cfg(windows)]`.
//! - [`write_telemetry_sidecar`] — top-level I/O entry point that
//!   serialises a slice of `EngineFrame` to a JSON array on disk.
//!   Mirrors the buyer wire contract used by
//!   `src/record/action_camera_writer.rs`: top-level array, snake_case
//!   field names, atomic write semantics handled by the caller (the
//!   recorder calls this through `durable_write` in production).
//!
//! # Public API example
//!
//! ```no_run
//! use engine_telemetry::{CyberpunkHook, EngineHook, write_telemetry_sidecar};
//! use std::path::Path;
//!
//! let mut hook = CyberpunkHook::new();
//! let mut frames = Vec::new();
//! for _ in 0..3 {
//!     // In production this is called once per swap-chain Present.
//!     let frame = hook.capture_frame().expect("capture");
//!     frames.push(frame);
//! }
//! write_telemetry_sidecar(&frames, Path::new("/tmp/engine_telemetry.json"))
//!     .expect("write sidecar");
//! ```
//!
//! # Coordinate-frame conventions
//!
//! All positions are world-space, expressed in **meters** (the recorder
//! multiplies engine units by [`EngineFrame::metric_scale`] before
//! writing — REDengine 4's internal unit is the meter, so for Cyberpunk
//! the scale is `1.0`, but the field is kept explicit per-frame so a
//! later UE5 / idTech 7 hook with cm- or inch-based units stays
//! interoperable). Quaternions are stored as `[x, y, z, w]` with `w`
//! last, matching the wire format Decart's Oasis training pipeline
//! consumes (see `docs/RECORDER_BUYER_SPEC_FEATURES.md`). Angles in
//! [`EngineFrame::fov_degrees`] are vertical FOV in degrees.

#![warn(missing_docs)]

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// Per-frame snapshot of the engine's transform state.
///
/// Field names are snake_case to match the buyer wire contract used by
/// the rest of `gamedata-recorder` (see `action_camera_writer.rs`). One
/// `EngineFrame` is emitted per rendered video frame and aligned to the
/// recording's `frame_index` so downstream tooling can fuse depth +
/// telemetry by index without timestamp drift.
///
/// Storage type for positions is `f64` (not `f32`) on purpose: open-world
/// titles like Cyberpunk 2077 push player coordinates well past `2^23`
/// engine units (Night City spans ~6 km), where `f32` precision starts
/// to fall apart at sub-meter distances from the origin. Quaternions
/// are also `f64` to keep round-trip composition exact through long
/// sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineFrame {
    /// World-space position of the player avatar (or vehicle) in meters,
    /// `[x, y, z]`. For Cyberpunk 2077 this is the position read from
    /// `gamePuppetEntity::GetWorldPosition()`. Already metric-scaled —
    /// callers should not multiply by `metric_scale` again.
    pub player_position: [f64; 3],

    /// Player rotation as a unit quaternion `[x, y, z, w]`, `w` last.
    /// World-space orientation of the avatar's root bone.
    pub player_rotation_quaternion: [f64; 4],

    /// World-space camera position in meters, `[x, y, z]`. For the
    /// third-person camera this equals
    /// `player_position + (Follow Offset rotated by camera orientation)`.
    /// For first-person it usually coincides with the head bone.
    pub camera_position: [f64; 3],

    /// Camera rotation as a unit quaternion `[x, y, z, w]`, `w` last.
    /// World-space orientation of the camera (i.e. the view direction).
    pub camera_rotation_quaternion: [f64; 4],

    /// Camera "Follow Offset" — the local-space offset from the avatar
    /// pivot to the camera in third-person modes, in meters
    /// `[right, up, back]` per REDengine 4 convention. Stays meaningful
    /// on first-person frames too: it then collapses to the head-bone
    /// offset, which downstream tooling uses to detect FP↔TP transitions.
    pub camera_follow_offset: [f64; 3],

    /// Engine units → meters scale factor. For REDengine 4 this is
    /// `1.0` (engine unit IS the meter). Stored per-frame because
    /// vehicles in Cyberpunk re-scale their physics rigs at runtime,
    /// and because future profiles (UE5: cm; idTech 7: inches) will
    /// not be `1.0`. Position fields above are already in meters; this
    /// field exists so consumers can sanity-check / re-derive raw
    /// engine units if they need to.
    pub metric_scale: f64,

    /// Vertical field-of-view in degrees, as read from the engine's
    /// projection state. Used by the training pipeline to reconstruct
    /// the projection matrix when only depth and FOV are kept.
    pub fov_degrees: f64,

    /// Index of the matching color frame. Equals the `idx` field in
    /// `frames.jsonl` (see `constants::filename::recording::FRAMES_JSONL`)
    /// — that is what makes per-frame fusion possible without timestamps.
    pub frame_index: u64,

    /// Wall-clock time since recording start, in milliseconds. Same
    /// epoch as the rest of the recording's per-frame timestamps. Kept
    /// in addition to `frame_index` so a recording with a dropped frame
    /// can still align to other timestamped streams (input, audio).
    pub timestamp_ms: u64,
}

impl EngineFrame {
    /// Identity / zero frame. Useful as a default for unit tests and
    /// for the leading frame before the engine has reported a state.
    /// Identity quaternion is `[0, 0, 0, 1]` (rotation by zero radians).
    pub fn zeroed() -> Self {
        Self {
            player_position: [0.0; 3],
            player_rotation_quaternion: [0.0, 0.0, 0.0, 1.0],
            camera_position: [0.0; 3],
            camera_rotation_quaternion: [0.0, 0.0, 0.0, 1.0],
            camera_follow_offset: [0.0; 3],
            metric_scale: 1.0,
            fov_degrees: 60.0,
            frame_index: 0,
            timestamp_ms: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Hook trait
// ---------------------------------------------------------------------------

/// Errors raised by an [`EngineHook`].
///
/// The variants are coarse on purpose — the recorder only needs to know
/// "transient (skip this frame, retry next tick)" vs. "fatal (the hook
/// must be re-installed)". Profile-specific detail goes in the
/// `String` payload for log triage.
#[derive(Debug)]
pub enum HookError {
    /// The target process is not yet attached, or the RTTI offsets have
    /// not been resolved. Transient — recorder retries on the next tick.
    NotAttached(String),
    /// A pointer dereference / RTTI walk read past the end of a valid
    /// region. Possibly transient (engine swapping a substructure mid-
    /// frame); recorder skips this frame and retries.
    InvalidRead(String),
    /// The profile thinks it should be running but the engine reported
    /// state that violates an invariant (e.g. non-finite quaternion).
    /// Fatal-ish: recorder logs and pauses telemetry capture for the
    /// rest of the session.
    InvariantViolation(String),
    /// Generic I/O failure underlying a sidecar write. Wraps the inner
    /// error so the recorder can decide retry vs. abort.
    Io(io::Error),
}

impl std::fmt::Display for HookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAttached(s) => write!(f, "engine hook not attached: {s}"),
            Self::InvalidRead(s) => write!(f, "engine hook invalid read: {s}"),
            Self::InvariantViolation(s) => write!(f, "engine hook invariant violation: {s}"),
            Self::Io(e) => write!(f, "engine hook io error: {e}"),
        }
    }
}

impl std::error::Error for HookError {}

impl From<io::Error> for HookError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Per-title engine-state hook.
///
/// Implementations must keep `capture_frame` cheap — it runs once per
/// rendered frame on a hot path and is allowed to allocate at most the
/// returned [`EngineFrame`] itself. Anything that requires a syscall or
/// a process-memory read should be cached at hook-install time.
///
/// `metric_scale` is split out as its own method (instead of being read
/// off the last `EngineFrame`) because the recorder may want it before
/// the first frame is captured — for example, to write a header into
/// the sidecar describing the unit convention.
pub trait EngineHook: Send {
    /// Capture a snapshot of the engine's transform state for the
    /// current frame. Called from the recorder's per-frame tick.
    fn capture_frame(&mut self) -> Result<EngineFrame, HookError>;

    /// Engine units → meters factor for this profile. Constant for the
    /// life of the hook (subclasses that vary it per-frame override
    /// this and call through to `capture_frame().metric_scale`).
    fn metric_scale(&self) -> f64;
}

// ---------------------------------------------------------------------------
// Cyberpunk 2077 placeholder hook
// ---------------------------------------------------------------------------

/// Cyberpunk 2077 (REDengine 4) engine-state hook.
///
/// # Status
///
/// **Scaffold only.** The body emits a deterministic mock frame so the
/// cross-platform plumbing (sidecar writer, frame queue, JSON contract)
/// is unit-testable from the Mac developer box. The real implementation
/// is the puffydev hand-off, gated behind `#[cfg(windows)]` once the
/// `windows-rs` block is added to `Cargo.toml`.
///
/// # RTTI walk reference (for puffydev)
///
/// REDengine 4 exposes a typed RTTI runtime; the canonical paths the
/// real implementation must read on each frame are:
///
/// - `gameInstance` (singleton root) →
///   `gameInstance::GetPlayerSystem()` → `gamePlayerSystem` →
///   `gamePlayerSystem::GetLocalPlayerControlledGameObject()` →
///   `gamePuppetEntity` (the player avatar).
///   - `gamePuppetEntity::GetWorldPosition()` — `Vector4 { x, y, z, w }`,
///     world-space, REDengine units (= meters).
///   - `gamePuppetEntity::GetWorldOrientation()` — `Quaternion { i, j,
///     k, r }` (REDengine quat order; map to `[x, y, z, w]` on the way
///     out).
///
/// - `gameInstance::GetCameraSystem()` →
///   `gameCameraSystem::GetActiveCameraWorldTransform()` →
///   `WorldTransform { Position, Orientation }`. Same conventions as
///   above; this is what fills `camera_position` /
///   `camera_rotation_quaternion`.
///
/// - The Follow Offset lives on the active camera component, which for
///   third-person camera modes is reachable via
///   `gameCameraSystem::GetActiveCameraComponent()` →
///   `gameCameraComponent` →
///   `gameCameraComponent::followOffset` (`Vector3 { x, y, z }`,
///   REDengine convention `[right, up, -forward]`; the negate-forward
///   step is intentional — REDengine reports the offset in camera-local
///   space pointing *backward* from the avatar, but our wire format
///   wants `[right, up, back]` so the existing convention passes
///   through unchanged).
///
/// - `gameInstance::GetTimeSystem()` → `gameTimeSystem::GetSimTime()`
///   (or the engine's frame counter). Use this for `frame_index` only
///   if the recorder doesn't already supply it; the recorder normally
///   wins because it is the source of truth for `frames.jsonl`.
///
/// - `metric_scale` for REDengine 4 is the constant `1.0`. Future
///   non-REDengine profiles (UE5, idTech 7) need to read their world-
///   settings actor's `WorldToMeters` (UE5) or unit-system enum
///   (idTech 7). For Cyberpunk specifically: do not derive this from
///   anything — hard-code `1.0` and document.
///
/// - `fov_degrees` lives on the same `gameCameraComponent`:
///   `gameCameraComponent::fov` (vertical FOV in degrees, already in
///   the user-facing convention so no conversion needed). Cyberpunk
///   exposes both a base FOV and a multiplier for cinematics; read the
///   *effective* value (post-multiplier) to match what the player saw.
///
/// # Attach surface
///
/// Two viable attach surfaces, in order of preference:
///
/// 1. **In-process via RED4ext / Cyber Engine Tweaks (CET) plugin.**
///    Reads RTTI directly without `ReadProcessMemory`, no anti-cheat
///    risk (Cyberpunk has no online anti-cheat — see
///    `docs/MULTI_GAME_ROADMAP.md`), and the offsets are looked up by
///    name through RED4ext's RTTI registry, so they survive game
///    patches without re-scanning. Estimated effort: 1.5–2 days.
///
/// 2. **Out-of-process via `ReadProcessMemory` + AOB scan.** Higher
///    maintenance burden (signatures break on patches) but avoids the
///    user having to install RED4ext. Estimated effort: 3–4 days +
///    ~half a day per major patch.
///
/// Pick option 1 unless legal flags an issue with shipping a RED4ext
/// dependency.
///
/// # DX12 swap-chain timing
///
/// The recorder samples one `EngineFrame` per call to
/// `IDXGISwapChain::Present`. The depth-hook (see `crates/depth-hook`)
/// already hooks `ID3D12CommandQueue::ExecuteCommandLists`; the
/// engine-telemetry hook attaches to `IDXGISwapChain::Present` (or
/// piggybacks on the depth-hook's own present-wrapper) so that
/// `EngineFrame::frame_index` is guaranteed to match the GPU frame
/// the depth buffer was captured on. **Never** sample telemetry off
/// the recorder's tokio tick — async drift will desync depth from
/// transform within a few minutes of recording.
pub struct CyberpunkHook {
    /// Monotonically increasing frame index emitted by the mock body.
    /// In the real implementation this is replaced with the recorder's
    /// global frame counter — the field stays for ABI compatibility
    /// when the mock and the real impl coexist behind `#[cfg(windows)]`.
    next_frame_index: u64,
    /// Wall-clock origin for `timestamp_ms`. Set on first call to
    /// `capture_frame`. Mock-only; the real impl reads from the
    /// recorder's clock.
    epoch: Option<std::time::Instant>,
}

impl CyberpunkHook {
    /// Construct a hook in the not-yet-attached state.
    ///
    /// In the real implementation this resolves the RED4ext / RTTI
    /// offsets lazily on the first `capture_frame` call (so the
    /// recorder doesn't have to wait for the game to fully boot before
    /// installing the hook). The mock implementation simply zeroes the
    /// counter.
    pub fn new() -> Self {
        Self {
            next_frame_index: 0,
            epoch: None,
        }
    }

    /// REDengine 4's metric scale. See the `metric_scale` field
    /// docs on [`EngineFrame`] — REDengine units are meters, so this
    /// is the constant `1.0`. Hard-coded; do **not** derive at runtime.
    pub const METRIC_SCALE: f64 = 1.0;
}

impl Default for CyberpunkHook {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineHook for CyberpunkHook {
    /// Mock implementation. Emits a deterministic frame so the
    /// cross-platform tests can validate the JSON contract end-to-end
    /// without a running game. Replace the body with the RED4ext / RTTI
    /// walk described in the struct-level docs above.
    fn capture_frame(&mut self) -> Result<EngineFrame, HookError> {
        // Establish epoch on first frame so timestamps are relative to
        // hook-install rather than process-start.
        let epoch = *self.epoch.get_or_insert_with(std::time::Instant::now);
        let timestamp_ms = epoch.elapsed().as_millis() as u64;

        let i = self.next_frame_index;
        self.next_frame_index = self.next_frame_index.wrapping_add(1);

        // Deterministic mock values: a slowly-advancing player walking
        // along +X, a fixed Follow Offset, identity rotation. Picked so
        // a serde round-trip test can assert the *exact* values
        // without floating-point fuzz.
        let frame = EngineFrame {
            player_position: [i as f64 * 0.1, 0.0, 0.0],
            player_rotation_quaternion: [0.0, 0.0, 0.0, 1.0],
            camera_position: [i as f64 * 0.1, 1.7, -3.0],
            camera_rotation_quaternion: [0.0, 0.0, 0.0, 1.0],
            camera_follow_offset: [0.0, 1.7, -3.0],
            metric_scale: Self::METRIC_SCALE,
            fov_degrees: 70.0,
            frame_index: i,
            timestamp_ms,
        };

        // Sanity check the invariant "quaternion is roughly unit length".
        // The real RTTI walker will hit this branch if the engine reports
        // a degenerate quaternion (rare, but observed during loading
        // screens where the camera component is mid-reinit).
        let q = frame.camera_rotation_quaternion;
        let norm_sq = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
        if !(0.5..=1.5).contains(&norm_sq) {
            return Err(HookError::InvariantViolation(format!(
                "camera quaternion not unit length: norm^2 = {norm_sq}"
            )));
        }

        Ok(frame)
    }

    fn metric_scale(&self) -> f64 {
        Self::METRIC_SCALE
    }
}

// ---------------------------------------------------------------------------
// Sidecar writer
// ---------------------------------------------------------------------------

/// Serialise a slice of [`EngineFrame`]s to a JSON array on disk.
///
/// The on-disk format is a top-level JSON array (not JSON-Lines, not an
/// envelope object) of objects with snake_case field names. This is the
/// same shape used by `src/record/action_camera_writer.rs`'s
/// `action_camera.json`, so the buyer plugin can consume telemetry and
/// action records with the same parser. See
/// `docs/RECORDER_BUYER_SPEC_FEATURES.md` for the wire contract.
///
/// Empty `frames` produces `[]` (literally two bytes), not `null` and
/// not a missing file — the buyer plugin treats absence as "recording
/// failed", and we want absence to mean exactly that.
///
/// This function does **not** perform an atomic rename: the recorder
/// wraps it through `crate::util::durable_write` in production. Tests
/// call it directly on a tempdir.
pub fn write_telemetry_sidecar(frames: &[EngineFrame], path: &Path) -> Result<(), HookError> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, frames).map_err(|e| {
        // Map serde errors to HookError::Io with the underlying cause
        // preserved. `serde_json::Error::io_error_kind` returns Some for
        // IO failures and None for syntactic ones; here we always wrap
        // because an in-memory `Vec<EngineFrame>` can only fail to
        // serialise via the writer.
        HookError::Io(io::Error::other(e.to_string()))
    })?;
    writer.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// In-file unit tests (hot-path / private surface).
// Public-API integration tests live in `tests/integration.rs`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zeroed_frame_has_identity_quaternions() {
        let f = EngineFrame::zeroed();
        assert_eq!(f.player_rotation_quaternion, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(f.camera_rotation_quaternion, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(f.metric_scale, 1.0);
        assert_eq!(f.frame_index, 0);
    }

    #[test]
    fn cyberpunk_hook_metric_scale_is_one() {
        let hook = CyberpunkHook::new();
        assert_eq!(hook.metric_scale(), 1.0);
        assert_eq!(CyberpunkHook::METRIC_SCALE, 1.0);
    }

    #[test]
    fn cyberpunk_hook_advances_frame_index() {
        let mut hook = CyberpunkHook::new();
        let f0 = hook.capture_frame().unwrap();
        let f1 = hook.capture_frame().unwrap();
        let f2 = hook.capture_frame().unwrap();
        assert_eq!(f0.frame_index, 0);
        assert_eq!(f1.frame_index, 1);
        assert_eq!(f2.frame_index, 2);
    }
}
