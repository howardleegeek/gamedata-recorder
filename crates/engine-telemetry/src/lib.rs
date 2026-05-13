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
//! - [`GtaVHook`] — second concrete implementation, sibling to
//!   [`CyberpunkHook`]. Validates that the per-title scaffold pattern
//!   generalises to a non-REDengine title. Uses ScriptHookV native
//!   invokes against RAGE rather than RTTI walking; same mock-frame
//!   strategy on cross-platform builds.
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
// Cyberpunk 2077 hook
// ---------------------------------------------------------------------------

// Platform split:
//
// - `cyberpunk_windows` ships the real RED4ext-backed RTTI walker.
//   `LoadLibrary("red4ext.dll")` via `libloading`, name-keyed accessors
//   through the `Red4ExtRegistry` trait, no static link to RED4ext, no
//   hard-coded offsets.
// - `cyberpunk_mock` ships the deterministic walking-along-`+X` mock
//   from the original scaffold. Identical behaviour to the pre-split
//   body so the cross-platform tests in `tests/integration.rs` keep
//   passing byte-for-byte.
//
// The selection is `#[cfg(target_os = "windows")]` so a Mac developer
// box and Linux CI run the mock; a real Windows recorder build runs the
// RTTI walker. Both implementations export the same `CyberpunkHook`
// type name and the same public surface (`new`, `default`,
// `METRIC_SCALE`, plus `EngineHook` impl), so the recorder can hold an
// engine-telemetry `CyberpunkHook` without knowing which half it got.

#[cfg(target_os = "windows")]
mod cyberpunk_windows;
#[cfg(target_os = "windows")]
pub use cyberpunk_windows::{
    CyberpunkHook, Red4ExtDllRegistry, Red4ExtRegistry, Red4Quaternion, Red4Vector4,
};

#[cfg(not(target_os = "windows"))]
mod cyberpunk_mock;
#[cfg(not(target_os = "windows"))]
pub use cyberpunk_mock::CyberpunkHook;

/// Cyberpunk 2077 (REDengine 4) engine-state hook — RTTI walk reference.
///
/// > **Note**: the canonical type is `CyberpunkHook`, re-exported from
/// > either [`cyberpunk_mock`] (on Mac / Linux) or `cyberpunk_windows`
/// > (on Windows). This module-level doc block documents the contract
/// > both implementations honour. The Windows variant is the real
/// > RTTI walker; the Mac/Linux variant is the deterministic mock used
/// > for cross-platform testing.
///
/// # RTTI walk paths
///
/// REDengine 4 exposes a typed RTTI runtime; the canonical paths the
/// real implementation reads on each frame are:
///
/// - `gameInstance` (singleton root) →
///   `gameInstance::GetPlayerSystem()` → `gamePlayerSystem` →
///   `gamePlayerSystem::GetLocalPlayerControlledGameObject()` →
///   `gamePuppetEntity` (the player avatar).
///   - `gamePuppetEntity::GetWorldPosition()` — `Vector4 { x, y, z, w }`,
///     world-space, REDengine units (= meters).
///   - `gamePuppetEntity::GetWorldOrientation()` — `Quaternion { i, j,
///     k, r }` (REDengine quat order; maps to wire `[x, y, z, w]` as
///     `[i, j, k, r]`).
///
/// - `gameInstance::GetCameraSystem()` →
///   `gameCameraSystem::GetActiveCameraWorldTransform()` →
///   `WorldTransform { Position, Orientation }`. Same conventions as
///   above; fills `camera_position` / `camera_rotation_quaternion`.
///
/// - `gameCameraSystem::GetActiveCameraComponent()` →
///   `gameCameraComponent::followOffset` (`Vector3 { x, y, z }`,
///   REDengine convention `[right, up, -forward]`). The hook negates
///   `z` on the way out to produce the wire format `[right, up, back]`
///   — see the runbook for the rationale.
///
/// - `gameCameraComponent::fov` — vertical FOV in degrees, **post-
///   multiplier**. Cyberpunk separately exposes a `fovMultiplier` for
///   cinematics; the real impl reads the effective value, not the
///   base.
///
/// - `metric_scale` is hard-coded `1.0`. REDengine units are meters.
///
/// # Attach surface
///
/// On Windows: in-process load of `red4ext.dll` (a third-party plugin
/// loader the user installs separately). The hook does **not** inject;
/// it `LoadLibrary`s whatever is already in the process, then resolves
/// symbols by name. Cyberpunk has no online anti-cheat (multiplayer is
/// indefinitely shelved as of CDPR Q4-2025); the in-process RED4ext
/// path is the documented safe one.
///
/// On Mac/Linux: no real attach is possible. The hook returns the
/// deterministic mock frames from [`cyberpunk_mock::CyberpunkHook`] so
/// the cross-platform test suite can validate the JSON contract.
///
/// # DX12 swap-chain timing
///
/// The recorder samples one `EngineFrame` per call to
/// `IDXGISwapChain::Present`. The depth-hook (`crates/depth-hook`)
/// already hooks `ID3D12CommandQueue::ExecuteCommandLists`; the
/// engine-telemetry hook piggybacks on the depth-hook's present
/// wrapper so `EngineFrame::frame_index` matches the GPU frame the
/// depth buffer was captured on. **Never** sample telemetry off the
/// recorder's tokio tick — async drift will desync depth from
/// transform within minutes.
///
/// # Failure mode
///
/// Returns `HookError::NotAttached` when RED4ext is missing or the
/// player is not yet spawned. The recorder treats this as transient
/// and skips the frame; the hook does not panic or abort the
/// recording. See [`HookError`] for the full enum.
#[doc(alias = "RED4ext")]
#[doc(alias = "REDengine")]
pub mod cyberpunk_hook_docs {}

// ---------------------------------------------------------------------------
// GTA V Enhanced placeholder hook
// ---------------------------------------------------------------------------

/// GTA V Enhanced (RAGE engine) engine-state hook.
///
/// # Status
///
/// **Scaffold only.** Sibling to [`CyberpunkHook`] — same shape, different
/// engine. The body emits a deterministic mock frame so the cross-platform
/// plumbing stays unit-testable from the Mac developer box. The real
/// implementation is the puffydev hand-off, gated behind `#[cfg(windows)]`
/// once the ScriptHookV bridge is added.
///
/// # ScriptHookV native call reference (for puffydev)
///
/// RAGE does not expose a typed RTTI runtime the way REDengine 4 does.
/// Instead, the canonical attach surface is **ScriptHookV** (Alexander
/// Blade's library), which exposes a stable, well-documented native
/// function table indexed by hash. All paths below are **native invokes**
/// against the ScriptHookV `nativeCall` ABI, not raw memory reads.
///
/// ## Player avatar
///
/// ```text
/// PLAYER::PLAYER_PED_ID()                       → Ped (handle)
///   ├─> ENTITY::GET_ENTITY_COORDS(ped, alive)   → Vector3 { x, y, z }   (RAGE units ≈ meters)
///   └─> ENTITY::GET_ENTITY_HEADING(ped)         → Float (degrees, 0..360, world-space yaw)
/// ```
///
/// - `GET_ENTITY_COORDS` returns the ped origin in world-space. The
///   `alive` boolean argument should be `true`; if the player ped is
///   dead, RAGE returns the last known position rather than `(0,0,0)`.
/// - `GET_ENTITY_HEADING` returns a single yaw angle in degrees. There
///   is no native that returns a full quaternion for a ped, so the real
///   implementation must construct one from heading: `q = Quat::from_z_axis(deg_to_rad(heading))`,
///   producing `[0, 0, sin(yaw/2), cos(yaw/2)]`. RAGE is right-handed
///   `X=east, Y=north, Z=up` — yaw rotates around `+Z`.
///
/// ## Camera
///
/// ```text
/// CAM::GET_GAMEPLAY_CAM_COORD()                 → Vector3 { x, y, z }   (world-space, meters)
/// CAM::GET_GAMEPLAY_CAM_ROT(rotation_order=2)   → Vector3 { pitch, roll, yaw } (degrees)
/// CAM::_GET_GAMEPLAY_CAM_FOV()                  → Float (vertical FOV degrees)
/// ```
///
/// - `GET_GAMEPLAY_CAM_ROT` returns Euler angles in `(pitch, roll, yaw)`
///   order when `rotation_order = 2` (the default and the only order
///   ScriptHookV's docs guarantee stable). Convert to quaternion via
///   `Quat::from_euler(pitch, roll, yaw)` with the standard right-hand
///   rotation. Store as `[x, y, z, w]` with `w` last to match the wire
///   format (same convention as [`CyberpunkHook`]).
/// - `_GET_GAMEPLAY_CAM_FOV` is an **unnamed native** (the leading
///   underscore signals ScriptHookV community naming). Its hash is
///   `0x5F35F6732C3FBBA0` and it is stable across all GTA V Enhanced
///   patches as of the 2024 Enhanced Edition release. If exposing the
///   unnamed native is a concern, the safe fallback is the constant
///   `50.0` degrees (RAGE's default gameplay FOV) — but consumers will
///   then see no FOV variation during sniper-zoom or first-person camera.
///
/// ## Camera follow offset
///
/// RAGE's third-person camera follow offset is **not directly readable**
/// through the public ScriptHookV native table (it lives on internal
/// `CCamera` C++ objects). The pragmatic implementation derives it
/// post-hoc by subtracting the player position from the camera position
/// and rotating into the player's local frame:
///
/// ```text
/// world_offset = camera_position - player_position
/// camera_follow_offset = inverse(player_rotation) * world_offset
/// ```
///
/// This is what the buyer plugin actually wants — the relative pose, not
/// a tuneable game-internal field. Express the result as `[right, up, back]`
/// per the wire format documented on [`EngineFrame::camera_follow_offset`].
///
/// ## Frame index + timestamp
///
/// As with [`CyberpunkHook`], the recorder owns the global `frame_index`
/// (it must match `frames.jsonl` for the buyer plugin's join key to
/// resolve). RAGE does expose `MISC::GET_FRAME_COUNT()`, but the value
/// resets on save-game reload and pauses on the pause menu — do not use
/// it for `frame_index`.
///
/// `timestamp_ms` is the recorder's wall-clock time since recording
/// start, not RAGE's `MISC::GET_GAME_TIMER()` (which also pauses with
/// the menu and would desync from the depth/video stream).
///
/// ## Metric scale
///
/// Hard-code `1.0`. RAGE world units are nominally meters — confirmed
/// empirically by Rockstar's own physics constants (gravity = 9.81
/// units/s², matching real-world m/s²). Validate via the operator's
/// 100m walk test described in `docs/GTA_V_HOOK_RUNBOOK.md`. Do not
/// derive `metric_scale` at runtime.
///
/// # Attach surface
///
/// Two viable attach surfaces, in order of preference:
///
/// 1. **ScriptHookV (recommended).** Mature C++ library, simple `asi`
///    plugin model, well-documented native function table. Estimated
///    effort: 2 days for the basic hook, 1 day for the present-wrapper
///    integration. **Single-player only** — see anti-cheat note below.
///
/// 2. **RAGEPluginHook.** Higher-level C# framework on top of
///    ScriptHookV. Easier to prototype but adds a managed-runtime
///    dependency this crate doesn't otherwise need. Skip unless puffydev
///    is more comfortable in C# than C++.
///
/// # Anti-cheat compatibility
///
/// **GTA Online rejects ScriptHookV** — the BattlEye anti-cheat shipped
/// with GTA V Enhanced will kick (and historically ban) any session
/// where ScriptHookV is detected. **The recorder must only attach in
/// offline single-player mode (Story Mode).** Detect this by checking
/// `NETWORK::NETWORK_IS_GAME_IN_PROGRESS()` at hook-install time — if
/// it returns `true`, abort the install and surface a recorder-side
/// error message ("GTA V Online is not supported; switch to Story Mode
/// to record"). Do not attempt to bypass: it's both a TOS violation and
/// a ban risk for the user.
///
/// # DX11/DX12 swap-chain timing
///
/// GTA V Enhanced uses DX12 (the Enhanced Edition's headline upgrade
/// over the legacy DX11 build). Same hook surface as Cyberpunk — sample
/// one `EngineFrame` per `IDXGISwapChain::Present`. Piggyback on the
/// `crates/depth-hook` present-wrapper if depth capture is enabled, to
/// guarantee `frame_index` parity with the captured depth buffer.
pub struct GtaVHook {
    /// Monotonically increasing frame index emitted by the mock body.
    /// In the real implementation this is replaced with the recorder's
    /// global frame counter — same lifecycle as [`CyberpunkHook`].
    next_frame_index: u64,
    /// Wall-clock origin for `timestamp_ms`. Set on first call to
    /// `capture_frame`. Mock-only; the real impl reads from the
    /// recorder's clock.
    epoch: Option<std::time::Instant>,
}

impl GtaVHook {
    /// Construct a hook in the not-yet-attached state.
    ///
    /// In the real implementation this loads the ScriptHookV native
    /// table, verifies `NETWORK::NETWORK_IS_GAME_IN_PROGRESS()` returns
    /// `false` (Story Mode only — see anti-cheat note on the struct
    /// docs), and caches the FOV native hash. The mock implementation
    /// simply zeroes the counter.
    pub fn new() -> Self {
        Self {
            next_frame_index: 0,
            epoch: None,
        }
    }

    /// RAGE's metric scale. RAGE world units are meters (validated via
    /// the 100m walk test — see `docs/GTA_V_HOOK_RUNBOOK.md`). Hard-coded;
    /// do **not** derive at runtime.
    pub const METRIC_SCALE: f64 = 1.0;
}

impl Default for GtaVHook {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineHook for GtaVHook {
    /// Mock implementation. Emits a deterministic frame so the
    /// cross-platform tests can validate the JSON contract end-to-end
    /// without a running game. Replace the body with the ScriptHookV
    /// native invokes described in the struct-level docs above.
    ///
    /// Mock differs from [`CyberpunkHook`] in two intentional ways so
    /// the two hooks are distinguishable in regression tests:
    ///
    /// - Player walks along `+Y` (north) instead of `+X` (east); RAGE
    ///   convention is `X=east, Y=north, Z=up`, and "walking forward"
    ///   in GTA V's default avatar orientation moves `+Y`.
    /// - Default mock FOV is `50.0` (RAGE's gameplay default), not
    ///   `70.0`.
    fn capture_frame(&mut self) -> Result<EngineFrame, HookError> {
        // Establish epoch on first frame so timestamps are relative to
        // hook-install rather than process-start.
        let epoch = *self.epoch.get_or_insert_with(std::time::Instant::now);
        let timestamp_ms = epoch.elapsed().as_millis() as u64;

        let i = self.next_frame_index;
        self.next_frame_index = self.next_frame_index.wrapping_add(1);

        // Deterministic mock values: a slowly-advancing player walking
        // along +Y (RAGE convention: north). Fixed third-person follow
        // offset, identity rotation. Picked so a serde round-trip test
        // can assert the *exact* values without floating-point fuzz.
        let frame = EngineFrame {
            player_position: [0.0, i as f64 * 0.1, 0.0],
            player_rotation_quaternion: [0.0, 0.0, 0.0, 1.0],
            camera_position: [0.0, i as f64 * 0.1 - 3.0, 1.7],
            camera_rotation_quaternion: [0.0, 0.0, 0.0, 1.0],
            camera_follow_offset: [0.0, -3.0, 1.7],
            metric_scale: Self::METRIC_SCALE,
            fov_degrees: 50.0,
            frame_index: i,
            timestamp_ms,
        };

        // Sanity check the invariant "quaternion is roughly unit length".
        // Same guard as CyberpunkHook — the real ScriptHookV path will
        // hit this branch if the engine reports a degenerate camera
        // orientation (rare, but observed during cutscene transitions).
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

    // `cyberpunk_hook_metric_scale_is_one` and
    // `cyberpunk_hook_advances_frame_index` exercise the mock body's
    // deterministic semantics (frame_index starts at 0, each call
    // advances it, no engine attach required). They are conceptually
    // *mock-only* tests — on Windows, `CyberpunkHook::new()` returns a
    // hook backed by the production `Red4ExtDllRegistry` which yields
    // `NotAttached` until the operator wires up a specific RED4ext SDK
    // signature, so the same assertions wouldn't make sense.
    //
    // The Windows path has its own unit tests in
    // `crates/engine-telemetry/src/cyberpunk_windows.rs#tests` that
    // exercise `CyberpunkHook::with_registry(MockRegistry::happy())`
    // and cover the equivalent ground (frame_index advances, metric
    // scale = 1.0, etc.) against an injectable registry instead of
    // the real DLL.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn cyberpunk_hook_metric_scale_is_one() {
        let hook = CyberpunkHook::new();
        assert_eq!(hook.metric_scale(), 1.0);
        assert_eq!(CyberpunkHook::METRIC_SCALE, 1.0);
    }

    #[cfg(not(target_os = "windows"))]
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
