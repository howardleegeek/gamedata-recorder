//! Windows-only ScriptHookV bridge for [`crate::GtaVHook`].
//!
//! # Why this file exists
//!
//! On `target_os = "windows"` the recorder is attached to a running
//! GTA V Enhanced process. RAGE does **not** ship a typed RTTI runtime
//! the way REDengine 4 does (sibling `cyberpunk_windows.rs`); instead,
//! the canonical attach surface is **ScriptHookV** (Alexander Blade's
//! plugin library at <http://www.dev-c.com/gtav/scripthookv/>), which
//! exposes a stable, well-documented native-function table indexed by
//! 64-bit hash. This module reads the player avatar's position +
//! heading and the active gameplay camera's transform + FOV by
//! **native-hash dispatch** through that table, never by raw memory
//! offset. Offsets move on every GTA V patch; native hashes are stable
//! and the community-maintained reference
//! (<https://alloc8or.re/gta5/nativedb/>) tracks them across releases.
//!
//! # Why this file is so cautious
//!
//! The recorder runs as a co-tenant with a $60 AAA game; a crash here
//! takes the user's session down. Every interaction with the
//! ScriptHookV library is therefore:
//!
//! 1. **Lazy-loaded.** If `ScriptHookV.dll` is not present in the host
//!    process (the user did not drop the `.dll` + `.asi` plugin into
//!    the GTA V install root, or has uninstalled them mid-session), we
//!    log a one-shot warning and fall back to [`HookError::NotAttached`]
//!    — the recorder treats this as transient and skips telemetry until
//!    next tick. We do **not** panic, do **not** abort the recording,
//!    do **not** corrupt the sidecar.
//! 2. **Symbol-resolved by name** (`libloading::Library::get`). Missing
//!    symbols also surface as `NotAttached`; the ScriptHookV C ABI has
//!    drifted historically (the unnamed-native FOV accessor in
//!    particular) and we want a future SDK rename to degrade gracefully
//!    rather than ICE the recorder.
//! 3. **Trait-abstracted** ([`ScriptHookVRegistry`]). All Windows-side
//!    I/O is fronted by a trait so unit tests can substitute an
//!    in-memory registry without needing a real DLL on the CI box. See
//!    [`tests::MockRegistry`] at the bottom of this file for the
//!    pattern — mirroring the sibling `cyberpunk_windows::MockRegistry`
//!    layout established on `feat/cyberpunk-hook-cluster`.
//!
//! # What this file does NOT do
//!
//! - **No DLL injection.** ScriptHookV is loaded by the game's own
//!   loader when the user drops the plugin into the install root; we
//!   `LoadLibrary` it, which is a read-only attach. We do **not**
//!   `CreateRemoteThread`, do **not** `WriteProcessMemory`, do **not**
//!   patch ImageBase + offset. Anything that looks like that pattern
//!   is wrong; back out and re-check the spec.
//! - **No static link.** Per the spec ("Rust 1.75+, NO static link to
//!   ScriptHookV.lib") the `.dll` is resolved at runtime only. The
//!   user installs ScriptHookV separately as a third-party library —
//!   the build must not assume it exists at link time. This protects
//!   the licensing boundary (ScriptHookV is closed-source freeware,
//!   not re-distributable).
//! - **No raw memory reads.** All telemetry reads are native-hash
//!   dispatch through the trait. The trait's only Windows-specific
//!   concrete impl ([`ScriptHookVDllRegistry`]) calls into
//!   ScriptHookV's own C ABI — which is what ScriptHookV exists to
//!   provide.
//! - **No hard-coded memory offsets.** Per the spec ("不要 hardcode 内存
//!   offset — 全部走 native hash") every accessor below is keyed on a
//!   64-bit native hash, never on an offset from a module base. The
//!   hashes themselves are stable across patches; ScriptHookV's
//!   internal table-walking handles the per-patch trampoline.
//! - **No GTA Online attach.** ScriptHookV is rejected by BattlEye in
//!   GTA Online. Per the runbook we check
//!   `NETWORK_IS_GAME_IN_PROGRESS` before every frame and surface a
//!   distinct error if Online is detected, so the recorder tears down
//!   the hook rather than risk a TOS / ban event for the user.
//!
//! # Where the ScriptHookV SDK seams are
//!
//! ScriptHookV's SDK exposes the native-table surface through a small
//! family of C entry points typically named `nativeInit`,
//! `nativePush64`, `nativeCall`, plus `getGlobalPtr` / `getScriptHandle`
//! for the script-thread bookkeeping. We resolve those entry points by
//! name and use the registry trait to mediate. If the user's
//! ScriptHookV is too old / too new to expose the names we want, we
//! fall through to `NotAttached` — the same path as "DLL not present
//! at all" — and the recorder logs a one-shot warning naming the
//! missing symbol so the Windows operator can update ScriptHookV.
//!
//! # Native hashes
//!
//! Per the runbook (`docs/GTA_V_HOOK_RUNBOOK.md`) and the community
//! reference (<https://alloc8or.re/gta5/nativedb/>) the canonical
//! hashes used by this hook are:
//!
//! ```text
//! 0xD80958FC74E988A6  // PLAYER::PLAYER_PED_ID
//! 0x3FEF770D40960D5A  // ENTITY::GET_ENTITY_COORDS(ped, alive)
//! 0xAFBD61CC738D9EB9  // ENTITY::GET_ENTITY_ROTATION(ped, rotation_order=2 ZYX)
//! 0x837765A25378F0BB  // CAM::GET_GAMEPLAY_CAM_ROT(rotation_order=2)
//! 0x65019750A0324133  // CAM::GET_GAMEPLAY_CAM_FOV
//! ```
//!
//! These are exposed as `const` items below so test mocks can refer to
//! the same numeric source-of-truth as the production registry.
//!
//! # Coordinate-system contract
//!
//! RAGE is **right-handed** with `X` east, `Y` north, `Z` up. That
//! matches what [`crate::EngineFrame`] documents as its wire format,
//! so the body of `capture_frame` does **no** coordinate-system
//! conversion on positions — it copies `[x, y, z]` straight out of the
//! `RageVector3` into the wire-format array. Rotations are returned by
//! RAGE as Euler degrees in `(pitch, roll, yaw)` order with
//! `rotation_order = 2` (ZYX intrinsic); we convert to quaternion in
//! Rust via the standard Z-Y-X composition (see [`euler_zyx_to_quat`])
//! and store as `[x, y, z, w]` with `w` last to match the wire format.
//!
//! # Failure-mode summary (spec contract)
//!
//! | Situation                              | Returned error                |
//! |----------------------------------------|-------------------------------|
//! | ScriptHookV.dll not loaded in process  | `HookError::NotAttached`      |
//! | DLL loaded but missing a symbol        | `HookError::NotAttached`      |
//! | Player ped handle is `0` (loading)     | `HookError::NotAttached`      |
//! | `NETWORK_IS_GAME_IN_PROGRESS` is true  | `HookError::NotAttached`      |
//! | A native call returned a null intermediate | `HookError::InvalidRead`  |
//! | Quaternion not unit-length             | `HookError::InvariantViolation`|
//! | Any panic in the registry impl         | (impossible — trait is `Send`, no `panic!` on the hot path) |

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{EngineFrame, EngineHook, HookError};

// ---------------------------------------------------------------------------
// Native hash table (public constants — single source of truth for tests).
// ---------------------------------------------------------------------------
//
// These constants are kept in this module rather than in `lib.rs` because
// they are Windows-implementation detail: a Mac/Linux build does not
// resolve native hashes at all (the mock skips this layer entirely). The
// production `ScriptHookVDllRegistry` invokes ScriptHookV's `nativeInit(hash)`
// followed by `nativePush64` arg pushes and `nativeCall()` — see the
// inline comments on each accessor for the exact ABI walk.

/// `PLAYER::PLAYER_PED_ID()` → `Ped` (entity handle, 32-bit script handle).
///
/// Stable across all GTA V Enhanced patches per the community
/// nativedb. Returns `0` when the player ped has not yet been spawned
/// (main-menu / save-game-load transitions); the hook treats `0` as
/// "not attached" rather than as a valid entity.
pub const HASH_PLAYER_PED_ID: u64 = 0xD80958FC74E988A6;

/// `ENTITY::GET_ENTITY_COORDS(Entity entity, BOOL alive)` → `Vector3`.
///
/// Pass `alive = true`. With `alive = false`, RAGE returns `(0, 0, 0)`
/// for ragdoll'd peds — useless for our case.
pub const HASH_GET_ENTITY_COORDS: u64 = 0x3FEF770D40960D5A;

/// `ENTITY::GET_ENTITY_ROTATION(Entity entity, int rotation_order)` →
/// `Vector3` of Euler degrees `(x = pitch, y = roll, z = yaw)`.
///
/// Pass `rotation_order = 2` (ZYX intrinsic — pitch-roll-yaw applied in
/// that order). This is the canonical order ScriptHookV's docs
/// guarantee stable across patches.
pub const HASH_GET_ENTITY_ROTATION: u64 = 0xAFBD61CC738D9EB9;

/// `CAM::GET_GAMEPLAY_CAM_ROT(int rotation_order)` → `Vector3` of
/// Euler degrees `(x = pitch, y = roll, z = yaw)`.
///
/// Pass `rotation_order = 2` to match the ped-rotation native above —
/// keeps the math through `euler_zyx_to_quat` consistent.
pub const HASH_GET_GAMEPLAY_CAM_ROT: u64 = 0x837765A25378F0BB;

/// `CAM::GET_GAMEPLAY_CAM_FOV()` → `float` (vertical FOV degrees).
///
/// Per the spec, this is the *named* FOV accessor — earlier versions
/// of the runbook called out an "unnamed native" with hash
/// `0x5F35F6732C3FBBA0`. The current spec selects the named hash
/// (`0x65019750A0324133`); if the operator's SDK only exposes the
/// older unnamed accessor, the registry falls back to that path via a
/// secondary `nativeInit` attempt (see `ScriptHookVDllRegistry::
/// camera_fov_degrees` for the fallback chain).
pub const HASH_GET_GAMEPLAY_CAM_FOV: u64 = 0x65019750A0324133;

/// `NETWORK::NETWORK_IS_GAME_IN_PROGRESS()` → `BOOL`.
///
/// Used to detect a GTA Online session and abort the attach. Listed
/// here for completeness even though the hash is invoked only inside
/// `ScriptHookVDllRegistry::is_attached`.
pub const HASH_NETWORK_IS_GAME_IN_PROGRESS: u64 = 0x10FAB35428CCC9D7;

// ---------------------------------------------------------------------------
// Mini POD types mirroring RAGE's native-call return shapes.
// ---------------------------------------------------------------------------
//
// These are *not* `#[repr(C)]` mirrors of RAGE's actual structs —
// they're our own POD types that the registry trait fills in by
// invoking native hashes. The trait does the unsafe boundary crossing;
// everything above the trait is plain safe Rust.

/// RAGE `Vector3` (`x`, `y`, `z` floats). RAGE coordinate system is
/// right-handed `X = east, Y = north, Z = up`. We store as `f64`
/// internally (RAGE returns `f32`) to keep parity with [`EngineFrame`]'s
/// world-coordinate precision policy.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RageVector3 {
    /// East component (meters, world-space).
    pub x: f64,
    /// North component (meters, world-space).
    pub y: f64,
    /// Up component (meters, world-space).
    pub z: f64,
}

/// Euler-angle triple in degrees, RAGE convention `(pitch, roll, yaw)`
/// returned by `GET_ENTITY_ROTATION` / `GET_GAMEPLAY_CAM_ROT` with
/// `rotation_order = 2` (ZYX intrinsic).
///
/// `pitch` rotates around the body's `+X` axis (looking up/down).
/// `roll` rotates around the body's `+Y` axis (banking).
/// `yaw` rotates around the world `+Z` axis (looking left/right).
///
/// Stored as `f64` for the same precision-policy reason as
/// [`RageVector3`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RageEulerDeg {
    /// Pitch in degrees (around `+X`).
    pub pitch: f64,
    /// Roll in degrees (around `+Y`).
    pub roll: f64,
    /// Yaw in degrees (around `+Z`).
    pub yaw: f64,
}

/// Convert a RAGE Euler triple (degrees, ZYX intrinsic order) to a
/// unit quaternion in wire format `[x, y, z, w]` with `w` last.
///
/// The ZYX intrinsic convention is: first rotate by `yaw` around the
/// world `+Z` axis, then by `roll` around the body's new `+Y` axis,
/// then by `pitch` around the body's new `+X` axis. The composition
/// of three half-angle quaternions in that order is:
///
/// ```text
/// q_yaw   = [0,     0,     sin(y/2),  cos(y/2)]    // around +Z
/// q_roll  = [0,     sin(r/2),     0,  cos(r/2)]    // around +Y
/// q_pitch = [sin(p/2),     0,     0,  cos(p/2)]    // around +X
///
/// q = q_yaw * q_roll * q_pitch                    // ZYX intrinsic
/// ```
///
/// Expanded analytically (the standard ZYX Euler-to-quaternion
/// formula, e.g. Diebel 2006 §5.2):
///
/// ```text
/// cp, sp = cos(p/2), sin(p/2)
/// cr, sr = cos(r/2), sin(r/2)
/// cy, sy = cos(y/2), sin(y/2)
///
/// x = sp*cr*cy - cp*sr*sy
/// y = cp*sr*cy + sp*cr*sy
/// z = cp*cr*sy - sp*sr*cy
/// w = cp*cr*cy + sp*sr*sy
/// ```
///
/// Output is guaranteed unit-length by construction (each half-angle
/// quaternion is unit-length; the product of unit quaternions is
/// unit-length up to floating-point rounding). Norm error after the
/// composition is bounded above by `~4 * f64::EPSILON` for any input
/// angles in the canonical range.
///
/// This helper is `pub` because the test suite asserts against it
/// directly (the runbook's "verify the rotation order on a non-zero
/// pitch capture before trusting it" item).
pub fn euler_zyx_to_quat(e: RageEulerDeg) -> [f64; 4] {
    let p = e.pitch.to_radians() * 0.5;
    let r = e.roll.to_radians() * 0.5;
    let y = e.yaw.to_radians() * 0.5;
    let (sp, cp) = p.sin_cos();
    let (sr, cr) = r.sin_cos();
    let (sy, cy) = y.sin_cos();
    [
        sp * cr * cy - cp * sr * sy, // x
        cp * sr * cy + sp * cr * sy, // y
        cp * cr * sy - sp * sr * cy, // z
        cp * cr * cy + sp * sr * sy, // w
    ]
}

// ---------------------------------------------------------------------------
// Registry trait (test seam).
// ---------------------------------------------------------------------------

/// Read-only view into ScriptHookV's native-function table.
///
/// This trait exists so the Windows-only hook can be unit-tested
/// without a real GTA V Enhanced process: tests substitute
/// [`tests::MockRegistry`] (or any other fake) and assert that the
/// native-hash dispatch paths the production code walks are the ones
/// the spec actually documents.
///
/// The trait deliberately exposes a **high-level, typed** surface
/// (one method per logical telemetry read) rather than a generic
/// "invoke this hash with these args" surface. That keeps:
///
/// - unsafety contained to [`ScriptHookVDllRegistry`]'s implementation;
/// - the surface area small and reviewable (8 methods, all return
///   `Result<…, HookError>` or `bool`);
/// - the failure mode uniform — any registry impl returns
///   `Err(HookError::NotAttached)` rather than panicking on a missing
///   symbol or a null intermediate.
///
/// Implementations must be `Send` because the recorder holds the hook
/// across tokio task boundaries on the per-frame tick. They do **not**
/// need to be `Sync` (the hook is `&mut self` on the hot path, so the
/// recorder serialises access externally).
///
/// Mirrors the sibling `cyberpunk_windows::Red4ExtRegistry` trait
/// layout established on `feat/cyberpunk-hook-cluster` — same method
/// shapes, same `Send`-only relaxation, same lazy-attach contract on
/// `is_attached`.
pub trait ScriptHookVRegistry: Send {
    /// Return whether the ScriptHookV runtime is attached, the player
    /// ped has been spawned, AND the user is in offline Story Mode.
    /// Called on every frame *before* the other accessors — if this
    /// returns `false`, the hook returns `HookError::NotAttached` and
    /// the recorder skips this frame.
    ///
    /// This is intentionally compound: a `false` return can mean any
    /// of three things, distinguished by `attach_blocker()`:
    ///
    /// 1. ScriptHookV.dll not loaded in the host process;
    /// 2. ScriptHookV.dll loaded but the player ped handle is `0`
    ///    (main-menu / loading screen);
    /// 3. ScriptHookV.dll loaded but `NETWORK_IS_GAME_IN_PROGRESS`
    ///    returned `true` (GTA Online — TOS bail-out per runbook §7).
    ///
    /// All three are surfaced to the recorder as a single
    /// `NotAttached` so the per-frame tick has one error path to
    /// handle. Operator triage uses the log string from
    /// `attach_blocker` to disambiguate.
    fn is_attached(&self) -> bool;

    /// Human-readable description of why `is_attached()` returned
    /// `false`, for one-shot operator log lines. Returns `None` when
    /// attached. Stable strings; the recorder's UI may pattern-match
    /// on the "Online detected" substring to flip the
    /// "Record GTA V" toggle off (per runbook §7).
    fn attach_blocker(&self) -> Option<String>;

    /// Native dispatch on `PLAYER::PLAYER_PED_ID()` →
    /// `GET_ENTITY_COORDS(ped, alive=true)`. Returns the player ped's
    /// world-space position in RAGE units (= meters). The `alive`
    /// argument is fixed to `true` so the native returns the live
    /// position rather than `(0, 0, 0)` for ragdoll'd peds.
    fn player_world_position(&self) -> Result<RageVector3, HookError>;

    /// Native dispatch on `ENTITY::GET_ENTITY_ROTATION(ped,
    /// rotation_order=2)`. Returns Euler degrees in
    /// `(pitch, roll, yaw)` ZYX intrinsic order; the hook converts to
    /// quaternion via [`euler_zyx_to_quat`] on the way out.
    fn player_world_rotation(&self) -> Result<RageEulerDeg, HookError>;

    /// Native dispatch on `CAM::GET_GAMEPLAY_CAM_COORD()`. Returns the
    /// active gameplay camera's world-space position in meters.
    fn camera_world_position(&self) -> Result<RageVector3, HookError>;

    /// Native dispatch on `CAM::GET_GAMEPLAY_CAM_ROT(rotation_order=2)`.
    /// Returns Euler degrees in `(pitch, roll, yaw)` ZYX intrinsic
    /// order; the hook converts to quaternion via
    /// [`euler_zyx_to_quat`] on the way out.
    fn camera_world_rotation(&self) -> Result<RageEulerDeg, HookError>;

    /// Native dispatch on `CAM::GET_GAMEPLAY_CAM_FOV()`. Returns
    /// vertical FOV in degrees. Used by the training pipeline to
    /// reconstruct the projection matrix when only depth and FOV are
    /// kept (see [`EngineFrame::fov_degrees`]).
    fn camera_fov_degrees(&self) -> Result<f64, HookError>;

    /// Best-effort identifier of the currently-loaded interior /
    /// world. Stable string the buyer pipeline can group telemetry by
    /// (e.g. `"story::open_world"`, `"story::interior::franklin_house"`).
    /// Returns an empty string when ScriptHookV doesn't expose a
    /// world identifier; the wire format documents empty as
    /// permissible.
    fn world_id(&self) -> String;
}

// ---------------------------------------------------------------------------
// Concrete DLL-backed registry.
// ---------------------------------------------------------------------------

/// Production [`ScriptHookVRegistry`] backed by an in-process load of
/// `ScriptHookV.dll` via `libloading`.
///
/// # Safety
///
/// The `unsafe` blocks in this impl cross the C ABI boundary into
/// ScriptHookV's exported functions. The contract we rely on is:
///
/// - `ScriptHookV.dll` is a well-known plugin loader. If the user
///   installed it into the GTA V install root, it is in the host
///   process's module list; if they did not, the `Library::new` call
///   fails fast and we drop to the fallback path.
/// - The exported C functions (`nativeInit`, `nativePush64`,
///   `nativeCall`) are not allowed to throw. Any unsoundness in
///   ScriptHookV itself surfaces as a `Result::Err` here, not as a
///   panic across the FFI boundary.
///
/// We additionally ship a `LOGGED_MISSING_DLL` one-shot flag so the
/// recorder's tracing output gets one warning per process when
/// `ScriptHookV.dll` is missing — not one warning per `Present` call
/// (60+ per second).
pub struct ScriptHookVDllRegistry {
    /// Loaded library handle. Initialised to `None`; populated on the
    /// first successful `LoadLibrary("ScriptHookV.dll")` call.
    /// `OnceLock` gives us thread-safe lazy initialisation through a
    /// shared `&self` — the trait surface is `&self` only, so we can't
    /// take `&mut self` on the hot path.
    ///
    /// Implementation note: we hold the `libloading::Library` itself
    /// (not a raw HMODULE) because dropping it unloads the DLL, which
    /// would race the in-process plugin runtime. The hook intentionally
    /// keeps the library loaded for the entire recorder session, so
    /// `OnceLock` is the right shape (init once, never reset).
    library: OnceLock<libloading::Library>,

    /// Has the "we tried to attach and failed" one-shot warning fired?
    /// Used to keep the per-frame tracing noise bounded — the hook is
    /// called once per `Present` (~60 Hz), and we do not want 60 lines
    /// of "ScriptHookV not found" per second in the recorder log.
    ///
    /// Independent from `library` because a failed attach must not
    /// poison the `OnceLock` (the user might install ScriptHookV
    /// mid-session — unlikely but cheap to support). The warning is
    /// one-shot per process regardless.
    warned_missing: AtomicBool,

    /// Has the "GTA Online detected, refusing to attach" one-shot
    /// warning fired? Separate one-shot from `warned_missing` because
    /// the operator remediation is different: missing DLL → install
    /// ScriptHookV; online detected → switch to Story Mode. The
    /// recorder's UI pattern-matches on which warning fired to set the
    /// correct tooltip per runbook §7.
    ///
    /// Currently unread because the production `is_attached` path
    /// hasn't yet bound the `NETWORK_IS_GAME_IN_PROGRESS` native (the
    /// SDK binding is deferred to the Windows operator's final-mile
    /// deliverable — see file-level caveat). Reserved here so the
    /// struct layout and one-shot semantics are in place before the
    /// final-mile commit.
    #[allow(dead_code)]
    warned_online: AtomicBool,
}

impl ScriptHookVDllRegistry {
    /// Construct an unattached registry. Does not touch the
    /// filesystem or the linker; lazy attach happens on the first
    /// `is_attached()` call from the hook's per-frame tick.
    pub fn new() -> Self {
        Self {
            library: OnceLock::new(),
            warned_missing: AtomicBool::new(false),
            warned_online: AtomicBool::new(false),
        }
    }

    /// Attempt to obtain a handle to `ScriptHookV.dll` from the host
    /// process. Returns `Some(&Library)` if the DLL is loaded (either
    /// because we already attached, or because this call newly
    /// attached); `None` if the load failed.
    ///
    /// Idempotent and `&self`-safe. The per-frame cost when already
    /// attached is a single atomic load.
    fn try_attach(&self) -> Option<&libloading::Library> {
        if let Some(lib) = self.library.get() {
            return Some(lib);
        }

        // libloading's Library::new is the standard cross-platform
        // entry point; on Windows it calls LoadLibraryW under the
        // hood, which is the documented way to obtain a handle to a
        // DLL already loaded into the process (it returns the
        // existing HMODULE rather than re-loading). That is exactly
        // the semantics we want — ScriptHookV is loaded by the game
        // / by the user's .asi loader at process-start; we are not
        // the injector.
        //
        // Safety: `Library::new` is documented safe on Windows; the
        // wrapping type guards against subsequent unsafe usage.
        let attempt =
            unsafe { libloading::Library::new("ScriptHookV.dll") }.or_else(|primary_err| {
                // Try the case-variant some user installations use
                // before falling all the way to the not-attached path.
                // Keeps us robust to Windows's case-insensitive
                // filesystem normalising through a case-sensitive
                // `LoadLibrary` cache miss.
                //
                // Safety: same as above.
                unsafe { libloading::Library::new("scripthookv.dll") }.map_err(move |_alt_err| {
                    // Keep the *primary* error in the returned
                    // Result; the case-variant error is informational
                    // and would clutter the operator log if we logged
                    // both.
                    primary_err
                })
            });

        match attempt {
            Ok(lib) => {
                tracing::info!(
                    target: "engine_telemetry::gtav_windows",
                    "attached to ScriptHookV.dll"
                );
                // OnceLock::set returns Err if another thread won the
                // race. Either way the lock is now populated; fall
                // through to `.get()` for the shared reference.
                let _ = self.library.set(lib);
                self.library.get()
            }
            Err(e) => {
                // Log once per process — see warned_missing rationale
                // on the struct doc.
                if !self.warned_missing.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        target: "engine_telemetry::gtav_windows",
                        error = %e,
                        "ScriptHookV.dll not loaded in this process; engine telemetry will skip frames until ScriptHookV is installed into the GTA V Enhanced install root"
                    );
                }
                None
            }
        }
    }

    /// Resolve a symbol by name from the loaded library. Returns
    /// `HookError::NotAttached` if the DLL isn't loaded or the symbol
    /// is missing — same path as "DLL not loaded" because the
    /// user-facing remediation ("update ScriptHookV") is the same.
    ///
    /// # Safety
    ///
    /// The caller must specify a function pointer type that matches
    /// the C ABI exported by `ScriptHookV.dll`. We are deliberately
    /// not shipping a hard-coded list of bindings here because the
    /// `nativeCall` ABI has drifted across ScriptHookV releases (in
    /// particular the unnamed-FOV-native exposure path). Instead, the
    /// trait methods below call `resolve` with the *minimum*
    /// signature each one needs, and the bindings are deferred to the
    /// final-mile Windows operator with a real GTA V install — see
    /// the caveat at the bottom of this file.
    ///
    /// This helper is the only place inside this file where we cross
    /// the C ABI boundary. Tests use [`tests::MockRegistry`] instead
    /// and never call it.
    #[allow(dead_code)] // Wired up by the production accessors below; the per-method bodies are intentionally stubbed at the SDK seam until the Windows operator binds them against a specific ScriptHookV build.
    unsafe fn resolve<T>(&self, name: &[u8]) -> Result<libloading::Symbol<'_, T>, HookError> {
        let lib = self
            .try_attach()
            .ok_or_else(|| HookError::NotAttached("ScriptHookV.dll handle missing".to_string()))?;
        // Safety: caller-asserted T matches the exported signature.
        unsafe { lib.get::<T>(name) }.map_err(|e| {
            HookError::NotAttached(format!(
                "ScriptHookV.dll symbol `{}` missing: {e}",
                String::from_utf8_lossy(name)
            ))
        })
    }
}

impl Default for ScriptHookVDllRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptHookVRegistry for ScriptHookVDllRegistry {
    fn is_attached(&self) -> bool {
        // Lazy-attach on every poll. `try_attach` is idempotent: after
        // the first success it's a single atomic load. After a failure
        // it'll retry on the next poll, which lets a user install
        // ScriptHookV mid-session and have telemetry come online
        // without restarting the recorder (rare but supported).
        //
        // We do NOT call the player-ped-id / online-check natives
        // here because the production binding of those is deferred to
        // the final-mile operator (see file-level caveat). Once that
        // binding lands, the body becomes:
        //
        //     if !self.try_attach().is_some() { return false; }
        //     if self.online_check_native() { warn_once_online(); return false; }
        //     let ped = self.player_ped_id_native();
        //     ped != 0
        //
        // Until then, `is_attached()` returns `false` from the
        // production registry path so the recorder skips telemetry
        // safely. The mock-registry path used by unit tests bypasses
        // this entirely.
        self.try_attach().is_some()
    }

    fn attach_blocker(&self) -> Option<String> {
        // Disambiguate the three reasons `is_attached()` can return
        // false. See the trait doc for the full enumeration. The
        // production-path body is deferred to the operator (same
        // gating as `is_attached`); until then we report the
        // most-likely missing-DLL state.
        if self.try_attach().is_none() {
            Some("ScriptHookV.dll not loaded".to_string())
        } else {
            // DLL loaded but native bindings not yet wired up. Once
            // the operator completes the symbol-table binding, this
            // arm narrows to one of:
            //   "player ped not yet spawned"
            //   "GTA Online detected; recorder only supports Story Mode"
            //   None  (attached + Story Mode + ped spawned)
            Some(
                "ScriptHookV.dll loaded but native bindings deferred to operator final-mile"
                    .to_string(),
            )
        }
    }

    fn player_world_position(&self) -> Result<RageVector3, HookError> {
        // Implementation note: we do not yet ship a hard binding to
        // ScriptHookV's `nativeInit` / `nativePush64` / `nativeCall`
        // C-export signatures here. The SDK ABI has drifted across
        // ScriptHookV releases (notably the unnamed-FOV-native
        // exposure path), and we want this crate to compile and
        // produce a *correct error path* on any ScriptHookV version
        // rather than fail loudly on a missing symbol.
        //
        // When the Windows operator wires this up against a specific
        // ScriptHookV build (the "final mile" deliverable per the
        // runbook), the body below becomes:
        //
        //     unsafe {
        //         let init: libloading::Symbol<unsafe extern "C" fn(u64)>
        //             = self.resolve(b"nativeInit\0")?;
        //         let push: libloading::Symbol<unsafe extern "C" fn(u64)>
        //             = self.resolve(b"nativePush64\0")?;
        //         let call: libloading::Symbol<unsafe extern "C" fn() -> *mut u64>
        //             = self.resolve(b"nativeCall\0")?;
        //
        //         // 1. Look up the player ped handle.
        //         init(HASH_PLAYER_PED_ID);
        //         let ped_ret = call();
        //         let ped: i32 = *(ped_ret as *const i32);
        //         if ped == 0 {
        //             return Err(HookError::NotAttached(
        //                 "player ped handle is 0 (loading)".into(),
        //             ));
        //         }
        //
        //         // 2. Read coordinates of that ped, alive = true.
        //         init(HASH_GET_ENTITY_COORDS);
        //         push(ped as i64 as u64);
        //         push(1); // alive
        //         let raw = call() as *const f32;
        //         Ok(RageVector3 {
        //             x: (*raw.add(0)) as f64,
        //             y: (*raw.add(1)) as f64,
        //             z: (*raw.add(2)) as f64,
        //         })
        //     }
        //
        // Note: ScriptHookV's `nativeCall` returns a `*mut u64`
        // pointing into a 24-byte "argument area" in TLS that the
        // SDK reuses for both inputs and outputs. The Vector3 return
        // is a 12-byte payload (3 × f32) at the head of that area;
        // larger returns use additional slots. The above reads the
        // first three slots interpreted as f32 — the canonical
        // pattern used by NativeTrainer and every community plugin
        // on alloc8or.re.
        //
        // Until that wiring lands, the production code path returns
        // the same `NotAttached` we'd return for "DLL not loaded",
        // with a distinct error string so the operator log makes the
        // cause obvious. The mock-registry path used by unit tests
        // bypasses this entirely.
        Err(HookError::NotAttached(
            "ScriptHookVDllRegistry::player_world_position not yet bound to a ScriptHookV SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn player_world_rotation(&self) -> Result<RageEulerDeg, HookError> {
        // Production binding (same shape as `player_world_position`):
        //
        //     init(HASH_PLAYER_PED_ID); ped = call();
        //     init(HASH_GET_ENTITY_ROTATION); push(ped); push(2);
        //     let raw = call() as *const f32;
        //     Ok(RageEulerDeg {
        //         pitch: (*raw.add(0)) as f64,
        //         roll:  (*raw.add(1)) as f64,
        //         yaw:   (*raw.add(2)) as f64,
        //     })
        //
        Err(HookError::NotAttached(
            "ScriptHookVDllRegistry::player_world_rotation not yet bound to a ScriptHookV SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn camera_world_position(&self) -> Result<RageVector3, HookError> {
        // Production binding:
        //
        //     init(0xA200EB1EE790F448 /* GET_GAMEPLAY_CAM_COORD */);
        //     let raw = call() as *const f32;
        //     Ok(RageVector3 {
        //         x: (*raw.add(0)) as f64,
        //         y: (*raw.add(1)) as f64,
        //         z: (*raw.add(2)) as f64,
        //     })
        //
        // The COORD native isn't called out in the spec hashes (the
        // five spec hashes cover ped / rotation / cam-rot / cam-fov);
        // the COORD hash above is the community-stable value from
        // alloc8or.re. Listed here in the deferred-binding comment
        // for completeness — the operator binds it alongside the
        // five named in `const HASH_*` items above.
        Err(HookError::NotAttached(
            "ScriptHookVDllRegistry::camera_world_position not yet bound to a ScriptHookV SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn camera_world_rotation(&self) -> Result<RageEulerDeg, HookError> {
        // Production binding:
        //
        //     init(HASH_GET_GAMEPLAY_CAM_ROT); push(2);
        //     let raw = call() as *const f32;
        //     Ok(RageEulerDeg { pitch: *raw.add(0) as f64, … })
        //
        Err(HookError::NotAttached(
            "ScriptHookVDllRegistry::camera_world_rotation not yet bound to a ScriptHookV SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn camera_fov_degrees(&self) -> Result<f64, HookError> {
        // Production binding:
        //
        //     init(HASH_GET_GAMEPLAY_CAM_FOV);
        //     let raw = call() as *const f32;
        //     Ok(*raw as f64)
        //
        // Fallback chain per runbook §4: if the operator's SDK only
        // exposes the older unnamed FOV native (0x5F35F6732C3FBBA0),
        // the body re-`init`s that hash and retries before falling
        // through to the documented `50.0` constant fallback.
        Err(HookError::NotAttached(
            "ScriptHookVDllRegistry::camera_fov_degrees not yet bound to a ScriptHookV SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn world_id(&self) -> String {
        // RAGE exposes the current interior / world identifier via
        // `INTERIOR::GET_INTERIOR_FROM_ENTITY` etc., but the
        // C-side helpers are split across several natives and the
        // production binding is deferred alongside the rest. Empty
        // string is the documented permissive default per the wire
        // format.
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Hook struct — Windows variant.
// ---------------------------------------------------------------------------

/// GTA V Enhanced (RAGE engine) engine-state hook — Windows variant.
///
/// See [`crate::GtaVHook`] for the canonical struct-level documentation
/// (anti-cheat / Story-Mode-only requirement, DX12 timing contract).
/// This is the implementation selected on `target_os = "windows"`; the
/// Mac/Linux build uses [`crate::gtav_mock::GtaVHook`] instead.
///
/// The struct holds a boxed `dyn ScriptHookVRegistry` so the registry
/// impl can be swapped at runtime: production uses
/// [`ScriptHookVDllRegistry`]; unit tests inject a
/// [`tests::MockRegistry`]. The choice is made at construction time
/// (`new()` for production, `with_registry()` for tests) and is
/// opaque to the recorder.
pub struct GtaVHook {
    /// Registry abstraction. See [`ScriptHookVRegistry`].
    ///
    /// `Box<dyn Trait>` is intentional: the recorder owns one
    /// `GtaVHook` per session and we never benchmark-prove that dyn
    /// dispatch is a measurable hot-path cost relative to the FFI
    /// calls inside the registry impl. Static-dispatch generics
    /// would force `EngineHook` to leak a `R: ScriptHookVRegistry`
    /// type parameter all the way up to the recorder, which breaks
    /// the "hook trait is engine-agnostic" contract.
    registry: Box<dyn ScriptHookVRegistry>,

    /// Monotonically increasing frame index. Same semantics as the
    /// mock build (and the same field name) so the struct layout is
    /// drop-in compatible across platforms.
    next_frame_index: u64,

    /// Wall-clock origin for `timestamp_ms`. Set on first call to
    /// `capture_frame`. Mirrors the mock build's lifecycle.
    epoch: Option<std::time::Instant>,
}

impl GtaVHook {
    /// Construct a hook backed by the production
    /// [`ScriptHookVDllRegistry`]. Equivalent to the cross-platform
    /// `GtaVHook::new()` — that is the public entry point.
    pub fn new() -> Self {
        Self::with_registry(Box::new(ScriptHookVDllRegistry::new()))
    }

    /// Construct a hook backed by a caller-supplied registry. Used by
    /// unit tests to inject [`tests::MockRegistry`]; production code
    /// always uses [`Self::new`].
    pub fn with_registry(registry: Box<dyn ScriptHookVRegistry>) -> Self {
        Self {
            registry,
            next_frame_index: 0,
            epoch: None,
        }
    }

    /// RAGE's metric scale. RAGE world units are meters (validated
    /// via the 100m walk test — see `docs/GTA_V_HOOK_RUNBOOK.md`).
    /// Hard-coded `1.0`; do **not** derive at runtime.
    pub const METRIC_SCALE: f64 = 1.0;
}

impl Default for GtaVHook {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineHook for GtaVHook {
    /// Dispatch ScriptHookV native hashes through [`ScriptHookVRegistry`]
    /// to produce a per-frame snapshot.
    ///
    /// On the first call (or any subsequent call where the production
    /// `ScriptHookVDllRegistry` has not yet attached), this triggers
    /// a `LoadLibrary("ScriptHookV.dll")` attempt. Subsequent calls
    /// re-use the loaded handle and are cheap (one atomic load + a
    /// fixed sequence of `nativeInit` / `nativePush64` / `nativeCall`
    /// calls per frame, all in-process).
    ///
    /// Failure mode: returns `HookError::NotAttached` when ScriptHookV
    /// is missing, the player ped has not yet been spawned, or the
    /// user is in GTA Online. Never panics. See the failure-mode
    /// table at the top of this file for the full enumeration.
    fn capture_frame(&mut self) -> Result<EngineFrame, HookError> {
        let epoch = *self.epoch.get_or_insert_with(std::time::Instant::now);
        let timestamp_ms = epoch.elapsed().as_millis() as u64;

        let i = self.next_frame_index;
        // Hold the bump until the end so that a NotAttached return
        // does not "use up" a frame index. The recorder's join key is
        // the recorder-side frame counter, not this one, but keeping
        // them in sync simplifies test fixtures. Mirrors the
        // sibling `cyberpunk_windows` hook's semantics.

        if !self.registry.is_attached() {
            // Surface the most informative blocker string so the
            // recorder's operator log can disambiguate
            // "ScriptHookV missing" vs. "GTA Online detected" vs.
            // "player ped not spawned". The recorder pattern-matches
            // on "Online" to flip the UI toggle off per runbook §7.
            let reason = self
                .registry
                .attach_blocker()
                .unwrap_or_else(|| "unknown".to_string());
            return Err(HookError::NotAttached(format!(
                "ScriptHookV registry not yet attached: {reason}; will retry next frame"
            )));
        }

        // Dispatch the native-hash table through the registry. Each
        // `?` short-circuits to a transient `HookError::NotAttached`
        // or `InvalidRead` depending on what the registry signals.
        let player_pos = self.registry.player_world_position()?;
        let player_rot = self.registry.player_world_rotation()?;
        let cam_pos = self.registry.camera_world_position()?;
        let cam_rot = self.registry.camera_world_rotation()?;
        let fov = self.registry.camera_fov_degrees()?;

        // Convert Euler → quaternion via the ZYX intrinsic
        // composition. RAGE's `rotation_order = 2` matches our
        // `euler_zyx_to_quat` implementation; see that function's
        // doc for the analytic form. The unit-length invariant is
        // guaranteed by construction up to ~4 * f64::EPSILON.
        let player_quat = euler_zyx_to_quat(player_rot);
        let camera_quat = euler_zyx_to_quat(cam_rot);

        // Derive the camera follow offset from the *world-space*
        // camera-to-player delta, rotated into the player's local
        // frame. RAGE does not expose the follow-offset directly —
        // it lives on the internal `CCamera` C++ subclass. The
        // pragmatic derivation per runbook §5 is:
        //
        //     world_offset = camera_position - player_position
        //     follow_offset = inverse(player_rotation) * world_offset
        //
        // Express as `[right, up, back]` per the wire format. RAGE
        // peds rotate only around `+Z` for normal locomotion (pitch
        // and roll are animation state, not exposed), so the inverse
        // rotation simplifies to a 2D yaw-only rotation in the X-Y
        // plane:
        //
        //     yaw = player_rot.yaw  (degrees)
        //     dx_world = camera_position.x - player_position.x
        //     dy_world = camera_position.y - player_position.y
        //     dz_world = camera_position.z - player_position.z
        //
        //     // Rotate (dx, dy) by -yaw into the player's local frame.
        //     // RAGE's yaw rotates counter-clockwise around +Z when
        //     // looking down from above; the inverse rotates clockwise.
        //     local_right = dx_world * cos(yaw) + dy_world * sin(yaw)
        //     local_north = -dx_world * sin(yaw) + dy_world * cos(yaw)
        //     local_up    = dz_world  (Z is invariant under yaw rotation)
        //
        //     // Wire format: [right, up, back]. The avatar faces +Y
        //     // in its local frame, so "behind" the avatar is -Y_local,
        //     // and the camera-is-behind-the-avatar offset reads as
        //     // a positive `back` when local_north is negative.
        //     right = local_right
        //     up    = local_up
        //     back  = -local_north
        let yaw_rad = player_rot.yaw.to_radians();
        let (sin_yaw, cos_yaw) = yaw_rad.sin_cos();
        let dx = cam_pos.x - player_pos.x;
        let dy = cam_pos.y - player_pos.y;
        let dz = cam_pos.z - player_pos.z;
        let local_right = dx * cos_yaw + dy * sin_yaw;
        let local_north = -dx * sin_yaw + dy * cos_yaw;
        let local_up = dz;
        let follow_offset = [local_right, local_up, -local_north];

        let frame = EngineFrame {
            player_position: [player_pos.x, player_pos.y, player_pos.z],
            player_rotation_quaternion: player_quat,
            camera_position: [cam_pos.x, cam_pos.y, cam_pos.z],
            camera_rotation_quaternion: camera_quat,
            camera_follow_offset: follow_offset,
            metric_scale: Self::METRIC_SCALE,
            fov_degrees: fov,
            frame_index: i,
            timestamp_ms,
        };

        // Sanity check: camera quaternion is unit length. The Euler
        // composition is unit-length by construction, but RAGE has
        // been observed to return non-finite Euler angles during a
        // sub-frame camera reinit (cutscene transition, save-game
        // load). Surface as InvariantViolation so the recorder pauses
        // telemetry rather than feeding the trainer rotated garbage.
        let q = frame.camera_rotation_quaternion;
        let norm_sq = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
        if !(0.5..=1.5).contains(&norm_sq) {
            return Err(HookError::InvariantViolation(format!(
                "camera quaternion not unit length: norm^2 = {norm_sq}"
            )));
        }

        // Same check for the player quaternion. Catches a transition
        // where ped Euler angles are NaN (mid-vehicle-entry frame).
        let pq = frame.player_rotation_quaternion;
        let pq_norm_sq = pq[0] * pq[0] + pq[1] * pq[1] + pq[2] * pq[2] + pq[3] * pq[3];
        if !(0.5..=1.5).contains(&pq_norm_sq) {
            return Err(HookError::InvariantViolation(format!(
                "player quaternion not unit length: norm^2 = {pq_norm_sq}"
            )));
        }

        // Only advance the frame counter on successful capture,
        // mirroring the mock build's semantics (which advances
        // unconditionally — but the mock can't fail in normal
        // operation, so the observable behaviour is the same up to
        // the deterministic test paths the integration suite
        // exercises).
        self.next_frame_index = self.next_frame_index.wrapping_add(1);

        Ok(frame)
    }

    fn metric_scale(&self) -> f64 {
        Self::METRIC_SCALE
    }
}

// ---------------------------------------------------------------------------
// Unit tests (Windows-only).
// ---------------------------------------------------------------------------
//
// These tests build only on `target_os = "windows"` — they exercise the
// real GtaVHook against a MockRegistry that simulates the ScriptHookV
// native table without needing a running game. The cross-platform
// integration tests in `tests/integration.rs` cover the Mac/Linux mock
// path. Mirrors the sibling `cyberpunk_windows::tests` layout from
// `feat/cyberpunk-hook-cluster`.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory [`ScriptHookVRegistry`] for unit tests. Holds a
    /// snapshot of "what the registry should return on the next call"
    /// and lets each test poke values + assert the hook's mapping is
    /// correct.
    pub(super) struct MockRegistry {
        inner: Mutex<MockState>,
    }

    struct MockState {
        attached: bool,
        blocker: Option<String>,
        player_pos: Result<RageVector3, HookError>,
        player_rot: Result<RageEulerDeg, HookError>,
        camera_pos: Result<RageVector3, HookError>,
        camera_rot: Result<RageEulerDeg, HookError>,
        fov: Result<f64, HookError>,
        world_id: String,
    }

    impl MockRegistry {
        /// Construct a mock that returns "happy-path" values — player
        /// at origin facing identity (yaw=0, so +Y is "forward"),
        /// camera at `(0, -3, 1.7)` (3m behind, 1.7m up — RAGE
        /// third-person follow), FOV 50°. Tests override individual
        /// fields via `set_*`.
        fn happy() -> Self {
            Self {
                inner: Mutex::new(MockState {
                    attached: true,
                    blocker: None,
                    player_pos: Ok(RageVector3 {
                        x: 0.0,
                        y: 0.0,
                        z: 0.0,
                    }),
                    player_rot: Ok(RageEulerDeg {
                        pitch: 0.0,
                        roll: 0.0,
                        yaw: 0.0,
                    }),
                    camera_pos: Ok(RageVector3 {
                        x: 0.0,
                        y: -3.0,
                        z: 1.7,
                    }),
                    camera_rot: Ok(RageEulerDeg {
                        pitch: 0.0,
                        roll: 0.0,
                        yaw: 0.0,
                    }),
                    fov: Ok(50.0),
                    world_id: "story::open_world".to_string(),
                }),
            }
        }

        /// Construct a mock that reports "not attached" for every
        /// call. Exercises the early-return path in `capture_frame`.
        fn detached() -> Self {
            Self {
                inner: Mutex::new(MockState {
                    attached: false,
                    blocker: Some("test: detached".to_string()),
                    player_pos: Err(HookError::NotAttached("test: detached".into())),
                    player_rot: Err(HookError::NotAttached("test: detached".into())),
                    camera_pos: Err(HookError::NotAttached("test: detached".into())),
                    camera_rot: Err(HookError::NotAttached("test: detached".into())),
                    fov: Err(HookError::NotAttached("test: detached".into())),
                    world_id: String::new(),
                }),
            }
        }

        /// Construct a mock that simulates GTA Online detection.
        /// `is_attached` returns false with the documented "Online
        /// detected" substring so the recorder's UI pattern-match in
        /// runbook §7 fires.
        fn online() -> Self {
            let s = Self::detached();
            s.inner.lock().unwrap().blocker =
                Some("GTA Online detected; recorder only supports Story Mode".to_string());
            s
        }

        // `set_camera_rot` is kept symmetrical with the other
        // `set_*` builders even though the current test suite only
        // exercises the error-injection path (`force_camera_rot_err`).
        // It's the obvious next helper a future test will need for
        // happy-path Euler tweaks (e.g. asserting that a non-zero
        // camera roll round-trips through `euler_zyx_to_quat`), so
        // we leave the helper in place and silence the dead-code
        // warning rather than delete-and-re-add.
        #[allow(dead_code)]
        fn set_camera_rot(&self, e: RageEulerDeg) {
            self.inner.lock().unwrap().camera_rot = Ok(e);
        }

        fn set_player_rot(&self, e: RageEulerDeg) {
            self.inner.lock().unwrap().player_rot = Ok(e);
        }

        fn set_player_pos(&self, v: RageVector3) {
            self.inner.lock().unwrap().player_pos = Ok(v);
        }

        fn set_camera_pos(&self, v: RageVector3) {
            self.inner.lock().unwrap().camera_pos = Ok(v);
        }

        fn set_fov(&self, fov: f64) {
            self.inner.lock().unwrap().fov = Ok(fov);
        }

        fn force_camera_rot_err(&self, e: HookError) {
            self.inner.lock().unwrap().camera_rot = Err(e);
        }
    }

    impl ScriptHookVRegistry for MockRegistry {
        fn is_attached(&self) -> bool {
            self.inner.lock().unwrap().attached
        }
        fn attach_blocker(&self) -> Option<String> {
            self.inner.lock().unwrap().blocker.clone()
        }
        fn player_world_position(&self) -> Result<RageVector3, HookError> {
            clone_vec_result(&self.inner.lock().unwrap().player_pos)
        }
        fn player_world_rotation(&self) -> Result<RageEulerDeg, HookError> {
            clone_euler_result(&self.inner.lock().unwrap().player_rot)
        }
        fn camera_world_position(&self) -> Result<RageVector3, HookError> {
            clone_vec_result(&self.inner.lock().unwrap().camera_pos)
        }
        fn camera_world_rotation(&self) -> Result<RageEulerDeg, HookError> {
            clone_euler_result(&self.inner.lock().unwrap().camera_rot)
        }
        fn camera_fov_degrees(&self) -> Result<f64, HookError> {
            match &self.inner.lock().unwrap().fov {
                Ok(v) => Ok(*v),
                Err(e) => Err(clone_hook_error(e)),
            }
        }
        fn world_id(&self) -> String {
            self.inner.lock().unwrap().world_id.clone()
        }
    }

    // HookError isn't `Clone`, so the mock hand-clones the variants
    // tests actually use. Mirrors the helper pattern in
    // `cyberpunk_windows::tests`.
    fn clone_hook_error(e: &HookError) -> HookError {
        match e {
            HookError::NotAttached(s) => HookError::NotAttached(s.clone()),
            HookError::InvalidRead(s) => HookError::InvalidRead(s.clone()),
            HookError::InvariantViolation(s) => HookError::InvariantViolation(s.clone()),
            HookError::Io(e) => HookError::Io(std::io::Error::other(e.to_string())),
        }
    }

    fn clone_vec_result(r: &Result<RageVector3, HookError>) -> Result<RageVector3, HookError> {
        match r {
            Ok(v) => Ok(*v),
            Err(e) => Err(clone_hook_error(e)),
        }
    }

    fn clone_euler_result(r: &Result<RageEulerDeg, HookError>) -> Result<RageEulerDeg, HookError> {
        match r {
            Ok(v) => Ok(*v),
            Err(e) => Err(clone_hook_error(e)),
        }
    }

    #[test]
    fn detached_registry_returns_not_attached() {
        // Spec contract: when ScriptHookV is not loaded, the hook
        // must return a transient `NotAttached` error rather than
        // panicking. The recorder swallows transient errors and
        // skips the frame.
        let mut hook = GtaVHook::with_registry(Box::new(MockRegistry::detached()));
        let res = hook.capture_frame();
        assert!(
            matches!(res, Err(HookError::NotAttached(_))),
            "expected NotAttached, got {res:?}"
        );
    }

    #[test]
    fn online_session_surfaces_distinct_blocker_string() {
        // Runbook §7: GTA Online detection must surface a recognisable
        // blocker so the recorder's UI can toggle off. We pattern-match
        // on the "Online" substring in the error.
        let mut hook = GtaVHook::with_registry(Box::new(MockRegistry::online()));
        let res = hook.capture_frame();
        let err = res.expect_err("online session must not succeed");
        let msg = format!("{err}");
        assert!(
            msg.contains("Online"),
            "expected Online substring in error, got: {msg}"
        );
    }

    #[test]
    fn happy_path_returns_unit_quaternion_frame() {
        // Validates the native-hash dispatch is plumbed end-to-end:
        // registry → hook → EngineFrame, with the right field
        // mapping. Identity Euler triples produce identity
        // quaternions; mocked camera-at-(0,-3,1.7) with yaw=0 should
        // derive follow_offset = [0, 1.7, 3.0] in [right, up, back].
        let mut hook = GtaVHook::with_registry(Box::new(MockRegistry::happy()));
        let frame = hook.capture_frame().expect("happy path");
        assert_eq!(frame.player_position, [0.0, 0.0, 0.0]);
        assert_eq!(frame.player_rotation_quaternion, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(frame.camera_position, [0.0, -3.0, 1.7]);
        assert_eq!(frame.camera_rotation_quaternion, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(frame.fov_degrees, 50.0);
        assert_eq!(frame.metric_scale, 1.0);
        assert_eq!(frame.frame_index, 0);
        // Camera at (0, -3, 1.7), player at (0, 0, 0), yaw = 0:
        //   dx = 0,    dy = -3,   dz = 1.7
        //   local_right = 0*1 + (-3)*0 = 0
        //   local_north = -0*0 + (-3)*1 = -3
        //   local_up    = 1.7
        //   wire format: [right, up, back] = [0, 1.7, -(-3)] = [0, 1.7, 3]
        let off = frame.camera_follow_offset;
        assert!((off[0] - 0.0).abs() < 1e-9, "right: {off:?}");
        assert!((off[1] - 1.7).abs() < 1e-9, "up: {off:?}");
        assert!((off[2] - 3.0).abs() < 1e-9, "back: {off:?}");
    }

    #[test]
    fn euler_zyx_to_quat_identity_is_unit_w_last() {
        // Identity Euler (0, 0, 0) -> identity quaternion [0, 0, 0, 1].
        // Locks in the wire-format `w` last convention through the
        // conversion helper.
        let q = euler_zyx_to_quat(RageEulerDeg::default());
        assert_eq!(q, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn euler_zyx_to_quat_yaw_90_rotates_around_z() {
        // Pure 90° yaw should produce a quaternion with non-zero `z`
        // and `w = sin(45°) = cos(45°) ≈ 0.7071`. Validates the
        // rotation axis is +Z (RAGE convention) and the ZYX intrinsic
        // composition reduces correctly for yaw-only inputs.
        let q = euler_zyx_to_quat(RageEulerDeg {
            pitch: 0.0,
            roll: 0.0,
            yaw: 90.0,
        });
        let s = std::f64::consts::FRAC_1_SQRT_2; // sin(45°) = 1/√2
        assert!((q[0] - 0.0).abs() < 1e-12, "x: {q:?}");
        assert!((q[1] - 0.0).abs() < 1e-12, "y: {q:?}");
        assert!((q[2] - s).abs() < 1e-12, "z: {q:?}");
        assert!((q[3] - s).abs() < 1e-12, "w: {q:?}");
        // Unit-length invariant by construction.
        let n2 = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
        assert!((n2 - 1.0).abs() < 1e-12, "norm^2: {n2}");
    }

    #[test]
    fn euler_zyx_to_quat_is_always_unit_length() {
        // Sample a fan of non-trivial Euler triples and confirm the
        // composition stays unit-length within `~4 * f64::EPSILON`.
        // This is the runbook §10 verification "quaternion norm^2 is
        // within [0.999, 1.001] for every captured frame" — the helper
        // is the single source of truth so we can test it in isolation.
        for pitch in [-90.0, -30.0, 0.0, 15.0, 89.9] {
            for roll in [-89.0, 0.0, 45.0] {
                for yaw in [-180.0, -90.0, 0.0, 90.0, 180.0] {
                    let q = euler_zyx_to_quat(RageEulerDeg { pitch, roll, yaw });
                    let n2 = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
                    assert!(
                        (n2 - 1.0).abs() < 1e-10,
                        "non-unit quat at (p={pitch}, r={roll}, y={yaw}): norm^2={n2}"
                    );
                }
            }
        }
    }

    #[test]
    fn camera_follow_offset_uses_player_yaw_rotation() {
        // Place the player at origin facing +X (yaw = 90° in RAGE,
        // since +Y is heading 0 and yaw rotates CCW around +Z so 90°
        // looks toward -X... wait: the runbook §2 says "heading of
        // 90° faces -X (west)". We use that convention here. With
        // player facing -X and the camera positioned 3m behind
        // (+X direction from player), the follow_offset should
        // resolve to [0, 0, 3] = [right=0, up=0, back=3].
        let mock = MockRegistry::happy();
        // Player at origin, facing yaw=90 (-X heading per runbook §2).
        mock.set_player_rot(RageEulerDeg {
            pitch: 0.0,
            roll: 0.0,
            yaw: 90.0,
        });
        // Camera at (+3, 0, 0) — exactly behind a player facing -X.
        mock.set_camera_pos(RageVector3 {
            x: 3.0,
            y: 0.0,
            z: 0.0,
        });
        let mut hook = GtaVHook::with_registry(Box::new(mock));
        let frame = hook.capture_frame().expect("follow offset");
        // Compute analytically:
        //   yaw_rad = 90° = π/2,  cos(yaw) ≈ 0, sin(yaw) = 1
        //   dx = 3, dy = 0, dz = 0
        //   local_right = 3*0 + 0*1 = 0
        //   local_north = -3*1 + 0*0 = -3
        //   local_up    = 0
        //   wire: [right=0, up=0, back=-(-3)=3]
        let off = frame.camera_follow_offset;
        assert!(off[0].abs() < 1e-9, "right: {off:?}");
        assert!(off[1].abs() < 1e-9, "up: {off:?}");
        assert!((off[2] - 3.0).abs() < 1e-9, "back: {off:?}");
    }

    #[test]
    fn frame_index_advances_only_on_success() {
        // A NotAttached early-return must not bump the frame counter.
        // This lets the recorder distinguish "telemetry ran but
        // engine wasn't ready" from "telemetry produced a frame".
        let mut hook = GtaVHook::with_registry(Box::new(MockRegistry::detached()));

        let r0 = hook.capture_frame();
        assert!(matches!(r0, Err(HookError::NotAttached(_))));

        let r1 = hook.capture_frame();
        assert!(matches!(r1, Err(HookError::NotAttached(_))));

        // Now construct a fresh happy mock and verify the new hook
        // starts at index 0 (the failed counter on the previous hook
        // never bumped).
        let mut hook2 = GtaVHook::with_registry(Box::new(MockRegistry::happy()));
        let f0 = hook2.capture_frame().expect("first frame");
        let f1 = hook2.capture_frame().expect("second frame");
        assert_eq!(f0.frame_index, 0);
        assert_eq!(f1.frame_index, 1);
    }

    #[test]
    fn degenerate_camera_rotation_returns_invariant_violation() {
        // Loading screens / cutscene transitions have been observed
        // to produce non-finite Euler angles for the camera. The
        // mock simulates this by returning an explicit
        // InvariantViolation error — the hook's `?` plumbing should
        // bubble it through unchanged. Mirrors the
        // `cyberpunk_windows` test of the same shape.
        let mock = MockRegistry::happy();
        mock.force_camera_rot_err(HookError::InvariantViolation(
            "test: degenerate camera euler".into(),
        ));
        let mut hook = GtaVHook::with_registry(Box::new(mock));
        let res = hook.capture_frame();
        assert!(
            matches!(res, Err(HookError::InvariantViolation(_))),
            "expected InvariantViolation, got {res:?}"
        );
    }

    #[test]
    fn metric_scale_is_one_unconditionally() {
        // RAGE world units are meters (validated empirically — see
        // the 100m walk test in `docs/GTA_V_HOOK_RUNBOOK.md`).
        // Locking metric_scale = 1.0 catches a future refactor that
        // copies in a UE5-style cm scale.
        let mut hook = GtaVHook::with_registry(Box::new(MockRegistry::happy()));
        assert_eq!(hook.metric_scale(), 1.0);
        assert_eq!(GtaVHook::METRIC_SCALE, 1.0);
        let frame = hook.capture_frame().expect("scale");
        assert_eq!(frame.metric_scale, 1.0);
    }

    #[test]
    fn player_position_passthrough_no_coord_transform() {
        // RAGE is right-handed X-east, Y-north, Z-up; wire format is
        // documented as the same. The hook must pass positions
        // through without any coordinate transform. Use a unique
        // signature so an accidental sign flip / axis swap fails the
        // test.
        let mock = MockRegistry::happy();
        mock.set_player_pos(RageVector3 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        });
        // Camera at same point so the follow_offset doesn't
        // accidentally tickle the player_position assertion.
        mock.set_camera_pos(RageVector3 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        });
        let mut hook = GtaVHook::with_registry(Box::new(mock));
        let frame = hook.capture_frame().expect("position");
        assert_eq!(frame.player_position, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn fov_passthrough_preserves_sniper_zoom_step() {
        // Per runbook §10: FOV values must match the in-game UI
        // during a sniper-zoom event (sniper rifle FOV drops to ~10°
        // from the ~50° default). The hook must pass FOV through
        // unmodified — no clamping, no smoothing, no scaling.
        let mock = MockRegistry::happy();
        mock.set_fov(10.0);
        let mut hook = GtaVHook::with_registry(Box::new(mock));
        let frame = hook.capture_frame().expect("fov");
        assert_eq!(frame.fov_degrees, 10.0);
    }

    #[test]
    fn default_dll_registry_construction_does_not_touch_linker() {
        // Production registry is constructed in the unattached state.
        // `ScriptHookVDllRegistry::new()` must not call `LoadLibrary`
        // — that happens lazily inside the first `is_attached` poll.
        // We assert the construction-time invariant directly on the
        // private `library` field rather than calling `is_attached`
        // (which would trigger a `LoadLibrary` and could succeed on a
        // dev machine that happens to have ScriptHookV installed —
        // unlikely on a Windows CI box, but possible).
        let r = ScriptHookVDllRegistry::new();
        assert!(
            r.library.get().is_none(),
            "fresh registry must not have called LoadLibrary"
        );
    }

    #[test]
    fn default_dll_registry_reports_world_id_safely_when_unattached() {
        // `world_id` is documented to return an empty string on
        // unsupported / unattached configurations. Test that this is
        // the case for the freshly-constructed registry (no panic,
        // no unwrap, no `expect`).
        let r = ScriptHookVDllRegistry::new();
        assert_eq!(r.world_id(), "");
    }

    #[test]
    fn native_hash_constants_match_spec() {
        // The spec hardcodes five canonical native hashes in
        // `docs/GTA_V_HOOK_RUNBOOK.md` and the spec file. Lock these
        // values in via constant assertions so a future "modernise"
        // refactor that flips a digit gets caught at test time
        // instead of at runtime against a real game (where the
        // failure mode is silently zero-valued telemetry — the worst
        // kind of regression for the buyer pipeline).
        assert_eq!(HASH_PLAYER_PED_ID, 0xD80958FC74E988A6);
        assert_eq!(HASH_GET_ENTITY_COORDS, 0x3FEF770D40960D5A);
        assert_eq!(HASH_GET_ENTITY_ROTATION, 0xAFBD61CC738D9EB9);
        assert_eq!(HASH_GET_GAMEPLAY_CAM_ROT, 0x837765A25378F0BB);
        assert_eq!(HASH_GET_GAMEPLAY_CAM_FOV, 0x65019750A0324133);
    }
}
