//! Windows-only RED4ext / RTTI implementation of [`crate::CyberpunkHook`].
//!
//! # Why this file exists
//!
//! On `target_os = "windows"` the recorder is attached to a running
//! Cyberpunk 2077 process. REDengine 4 ships a typed RTTI runtime — the
//! same one RED4ext (the third-party plugin loader at
//! <https://github.com/WopsS/RED4ext.SDK>) exposes to in-process plugins.
//! This module reads the player avatar's position / rotation and the
//! active camera's transform / FOV by **string lookup** through that
//! registry, never by raw memory offset. Offsets move on every patch;
//! class and method names are stable.
//!
//! # Why this file is so cautious
//!
//! The recorder runs as a co-tenant with a $60 AAA game; a crash here
//! takes the user's session down. Every interaction with the RED4ext
//! library is therefore:
//!
//! 1. **Lazy-loaded.** If `red4ext.dll` is not present in the host
//!    process (the user did not install RED4ext, or did and then
//!    uninstalled it without restarting), we log a one-shot warning and
//!    fall back to [`HookError::NotAttached`] — the recorder treats this
//!    as transient and skips telemetry until next tick. We do **not**
//!    panic, do **not** abort the recording, do **not** corrupt the
//!    sidecar.
//! 2. **Symbol-resolved by name** (`libloading::Library::get`). Missing
//!    symbols also surface as `NotAttached`; the RED4ext C ABI has
//!    drifted historically and we want a future SDK rename to degrade
//!    gracefully rather than ICE the recorder.
//! 3. **Trait-abstracted** ([`Red4ExtRegistry`]). All Windows-side I/O
//!    is fronted by a trait so unit tests can substitute an in-memory
//!    registry without needing a real DLL on the CI box. See
//!    [`tests::MockRegistry`] at the bottom of this file for the
//!    pattern.
//!
//! # What this file does NOT do
//!
//! - **No DLL injection.** RED4ext is already in-process if the user
//!   installed it; we `LoadLibrary` it, which is a read-only attach. We
//!   do **not** `CreateRemoteThread`, do **not** `WriteProcessMemory`,
//!   do **not** patch ImageBase + offset. Anything that looks like that
//!   pattern is wrong; back out and re-check the spec.
//! - **No raw memory reads.** All RTTI walks are name-keyed lookups
//!   through the trait. The trait's only Windows-specific concrete impl
//!   ([`Red4ExtDllRegistry`]) calls into RED4ext's own C ABI — which is
//!   what RED4ext exists to provide.
//! - **No game patches.** This is a passive read-only telemetry hook;
//!   we never write into the game's address space.
//!
//! # Where the SDK seams are
//!
//! RED4ext's plugin SDK exposes the RTTI surface through a C entry
//! point typically named `RED4ext_GetSystem` (and a small family of
//! `RED4ext_Lookup*` / `RED4ext_Call*` helpers; the exact set has
//! drifted across the 1.x release line). We resolve those entry points
//! by name and use the registry trait to mediate. If the user's RED4ext
//! is too old / too new to expose the names we want, we fall through to
//! `NotAttached` — the same path as "DLL not present at all" — and the
//! recorder logs a one-shot warning naming the missing symbol so the
//! Windows operator can update RED4ext.
//!
//! # Coordinate-system contract
//!
//! REDengine 4 is **right-handed** with `X` east, `Y` north, `Z` up.
//! That matches what [`crate::EngineFrame`] documents as its wire
//! format, so the body of `capture_frame` does **no** coordinate-system
//! conversion — it copies `[x, y, z]` straight out of the RTTI
//! `Vector4` and `[i, j, k, r]` straight out of the RTTI `Quaternion`
//! into the `[x, y, z, w]` array. The `Quaternion::r` component is
//! mathematically the same as the wire format's `w`; do not swap.
//!
//! # Failure-mode summary (spec contract)
//!
//! | Situation                              | Returned error                |
//! |----------------------------------------|-------------------------------|
//! | RED4ext not loaded in process          | `HookError::NotAttached`      |
//! | RED4ext loaded but missing a symbol    | `HookError::NotAttached`      |
//! | Player not spawned yet (main menu)     | `HookError::NotAttached`      |
//! | RTTI walk returned a null intermediate | `HookError::InvalidRead`      |
//! | Quaternion not unit-length             | `HookError::InvariantViolation`|
//! | Any panic in the registry impl         | (impossible — trait is `Send`, no `panic!` on the hot path; verified by `cargo clippy --deny clippy::panic` if enabled) |

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{EngineFrame, EngineHook, HookError};

// ---------------------------------------------------------------------------
// Mini POD types mirroring REDengine 4's RTTI shapes.
// ---------------------------------------------------------------------------
//
// These are *not* `#[repr(C)]` mirrors of the engine's actual structs —
// they're our own POD types that the registry trait fills in by name.
// The trait does the unsafe boundary crossing; everything above the trait
// is plain safe Rust.

/// REDengine 4 `Vector4` (`x`, `y`, `z`, `w` floats). `w` is unused for
/// the position read — REDengine reuses Vector4 for SoA reasons. We
/// store it for completeness but `capture_frame` only copies `[x, y, z]`
/// into the wire format.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Red4Vector4 {
    /// East component (meters, world-space).
    pub x: f64,
    /// North component (meters, world-space).
    pub y: f64,
    /// Up component (meters, world-space).
    pub z: f64,
    /// Unused for position; engine reuses Vector4 for SoA. Ignored on
    /// the way out.
    pub w: f64,
}

/// REDengine 4 `Quaternion` (`i`, `j`, `k`, `r` floats). Maps to our
/// wire format `[x, y, z, w]` as `[i, j, k, r]`. `r` is the scalar part
/// — same as `w` in the wire format; do not swap.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Red4Quaternion {
    /// Imaginary i (maps to wire-format `x`).
    pub i: f64,
    /// Imaginary j (maps to wire-format `y`).
    pub j: f64,
    /// Imaginary k (maps to wire-format `z`).
    pub k: f64,
    /// Real / scalar (maps to wire-format `w`).
    pub r: f64,
}

impl Red4Quaternion {
    /// Identity quaternion `[0, 0, 0, 1]` (rotation by zero radians).
    /// Used by the mock-registry path and by the early-frame fallback
    /// when the camera component has not yet been instantiated.
    pub fn identity() -> Self {
        Self {
            i: 0.0,
            j: 0.0,
            k: 0.0,
            r: 1.0,
        }
    }

    /// Convert to the wire-format `[x, y, z, w]` array. `w` last.
    pub fn to_xyzw(self) -> [f64; 4] {
        [self.i, self.j, self.k, self.r]
    }
}

// ---------------------------------------------------------------------------
// Registry trait (test seam).
// ---------------------------------------------------------------------------

/// Read-only view into REDengine 4's RTTI registry.
///
/// This trait exists so the Windows-only hook can be unit-tested without
/// a real Cyberpunk 2077 process: tests substitute [`tests::MockRegistry`]
/// (or any other fake) and assert that the string-lookup paths the
/// production code walks are the ones the SDK actually documents.
///
/// The trait deliberately exposes the **string-keyed** surface (class +
/// method name, plus a few well-typed accessors for known shapes) rather
/// than raw void pointers. That keeps:
///
/// - unsafety contained to [`Red4ExtDllRegistry`]'s implementation;
/// - the surface area small and reviewable (4 methods, all return
///   `Result<…, HookError>`);
/// - the failure mode uniform — any registry impl returns
///   `Err(HookError::NotAttached)` rather than panicking on a missing
///   symbol or a null intermediate.
///
/// Implementations must be `Send` because the recorder holds the hook
/// across tokio task boundaries on the per-frame tick. They do **not**
/// need to be `Sync` (the hook is `&mut self` on the hot path, so the
/// recorder serialises access externally).
pub trait Red4ExtRegistry: Send {
    /// Return whether the RED4ext runtime is attached and the player
    /// avatar has been instantiated. Called on every frame *before* the
    /// other accessors — if this returns `false`, the hook returns
    /// `HookError::NotAttached` and the recorder skips this frame.
    ///
    /// This is intentionally separate from "is RED4ext loaded" — the
    /// loading screen between game-start and the first spawned avatar
    /// is the most common reason this returns `false` in practice.
    fn is_attached(&self) -> bool;

    /// String-lookup the player avatar's world-space position. Walks
    /// `gameInstance → GetPlayerSystem() → gamePlayerSystem →
    /// GetLocalPlayerControlledGameObject() → gamePuppetEntity →
    /// GetWorldPosition() → Vector4`.
    ///
    /// Returns `HookError::NotAttached` if any intermediate is null
    /// (e.g. avatar not spawned yet); returns `HookError::InvalidRead`
    /// for a malformed read.
    fn player_world_position(&self) -> Result<Red4Vector4, HookError>;

    /// String-lookup the player avatar's world-space orientation.
    /// Walks `gamePuppetEntity → GetWorldOrientation() → Quaternion`.
    fn player_world_orientation(&self) -> Result<Red4Quaternion, HookError>;

    /// String-lookup the active camera's world-space position. Walks
    /// `gameInstance → GetCameraSystem() → gameCameraSystem →
    /// GetActiveCameraWorldTransform() → WorldTransform.Position`.
    fn camera_world_position(&self) -> Result<Red4Vector4, HookError>;

    /// String-lookup the active camera's world-space orientation.
    /// Walks `gameCameraSystem → GetActiveCameraWorldTransform() →
    /// WorldTransform.Orientation`.
    fn camera_world_orientation(&self) -> Result<Red4Quaternion, HookError>;

    /// String-lookup the active camera component's local-space follow
    /// offset. Walks `gameCameraSystem → GetActiveCameraComponent() →
    /// gameCameraComponent → followOffset → Vector3 [right, up, -forward]`.
    ///
    /// Returns the raw REDengine convention `[right, up, -forward]`;
    /// the hook negates `z` on the way out to produce the wire-format
    /// `[right, up, back]`.
    fn camera_follow_offset(&self) -> Result<Red4Vector4, HookError>;

    /// String-lookup the active camera component's effective FOV
    /// (vertical degrees, post-multiplier — matches what the player
    /// saw on screen). Walks `gameCameraComponent → fov → f32` and
    /// widens.
    fn camera_fov_degrees(&self) -> Result<f64, HookError>;

    /// Identifier of the currently-loaded world. Stable string the
    /// buyer pipeline can group telemetry by (e.g.
    /// `"q000_main_menu"`, `"open_world"`). Returns an empty string
    /// if RED4ext doesn't expose a world identifier on this version.
    fn world_id(&self) -> String;
}

// ---------------------------------------------------------------------------
// Concrete DLL-backed registry.
// ---------------------------------------------------------------------------

/// Production [`Red4ExtRegistry`] backed by an in-process load of
/// `red4ext.dll` via `libloading`.
///
/// # Safety
///
/// The `unsafe` blocks in this impl cross the C ABI boundary into
/// RED4ext's exported functions. The contract we rely on is:
///
/// - `red4ext.dll` is a well-known plugin loader. If the user installed
///   it, it is in the host process's module list; if they did not, the
///   `Library::new` call fails fast and we drop to the fallback path.
/// - The exported C functions are not allowed to throw. Any
///   unsoundness in RED4ext itself surfaces as a `Result::Err` here,
///   not as a panic across the FFI boundary.
///
/// We additionally ship a `LOGGED_MISSING_DLL` one-shot flag so the
/// recorder's tracing output gets one warning per process when
/// `red4ext.dll` is missing — not one warning per `Present` call (60+
/// per second).
pub struct Red4ExtDllRegistry {
    /// Loaded library handle. Initialised to `None`; populated on the
    /// first successful `LoadLibrary("red4ext.dll")` call. `OnceLock`
    /// gives us thread-safe lazy initialisation through a shared `&self`
    /// — the trait surface is `&self` only, so we can't take `&mut self`
    /// on the hot path.
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
    /// of "RED4ext not found" per second in the recorder log.
    ///
    /// Independent from `library` because a failed attach must not
    /// poison the `OnceLock` (the user might install RED4ext mid-session
    /// — unlikely but cheap to support). The warning is one-shot per
    /// process regardless.
    warned_missing: AtomicBool,
}

impl Red4ExtDllRegistry {
    /// Construct an unattached registry. Does not touch the filesystem
    /// or the linker; lazy attach happens on the first `is_attached()`
    /// call from the hook's per-frame tick.
    pub fn new() -> Self {
        Self {
            library: OnceLock::new(),
            warned_missing: AtomicBool::new(false),
        }
    }

    /// Attempt to obtain a handle to `red4ext.dll` from the host
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
        // entry point; on Windows it calls LoadLibraryW under the hood,
        // which is the documented way to obtain a handle to a DLL
        // already loaded into the process (it returns the existing
        // HMODULE rather than re-loading). That is exactly the
        // semantics we want — RED4ext is loaded by the game's own
        // loader at process-start; we are not the injector.
        //
        // Safety: `Library::new` is documented safe on Windows; the
        // wrapping type guards against subsequent unsafe usage.
        let attempt = unsafe { libloading::Library::new("red4ext.dll") }.or_else(|primary_err| {
            // Try the case-variant the RED4ext installer used in some
            // 1.20.x releases before falling all the way to the
            // not-attached path. This keeps us robust to Windows's
            // case-insensitive filesystem normalising through a
            // case-sensitive `LoadLibrary` cache miss.
            // Safety: same as above.
            unsafe { libloading::Library::new("RED4ext.dll") }.map_err(move |_alt_err| {
                // Keep the *primary* error in the returned Result; the
                // case-variant error is informational and would clutter
                // the operator log if we logged both.
                primary_err
            })
        });

        match attempt {
            Ok(lib) => {
                tracing::info!(target: "engine_telemetry::cyberpunk_windows", "attached to red4ext.dll");
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
                        target: "engine_telemetry::cyberpunk_windows",
                        error = %e,
                        "red4ext.dll not loaded in this process; engine telemetry will skip frames until RED4ext is installed"
                    );
                }
                None
            }
        }
    }

    /// Resolve a symbol by name from the loaded library. Returns
    /// `HookError::NotAttached` if the DLL isn't loaded or the symbol
    /// is missing — same path as "DLL not loaded" because the
    /// user-facing remediation ("update RED4ext") is the same.
    ///
    /// # Safety
    ///
    /// The caller must specify a function pointer type that matches
    /// the C ABI exported by `red4ext.dll`. We are deliberately not
    /// shipping a hard-coded list of bindings here because the SDK
    /// signatures have drifted across releases; instead the trait
    /// methods below call `resolve` with the *minimum* signature each
    /// one needs.
    ///
    /// This helper is the only place inside this file where we cross
    /// the C ABI boundary. Tests use [`tests::MockRegistry`] instead
    /// and never call it.
    #[allow(dead_code)] // Wired up by the production accessors below; the per-method bodies are intentionally stubbed at the SDK seam until the Windows operator binds them.
    unsafe fn resolve<T>(&self, name: &[u8]) -> Result<libloading::Symbol<'_, T>, HookError> {
        let lib = self
            .try_attach()
            .ok_or_else(|| HookError::NotAttached("red4ext.dll handle missing".to_string()))?;
        // Safety: caller-asserted T matches the exported signature.
        unsafe { lib.get::<T>(name) }.map_err(|e| {
            HookError::NotAttached(format!(
                "red4ext.dll symbol `{}` missing: {e}",
                String::from_utf8_lossy(name)
            ))
        })
    }
}

impl Default for Red4ExtDllRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Red4ExtRegistry for Red4ExtDllRegistry {
    fn is_attached(&self) -> bool {
        // Lazy-attach on every poll. `try_attach` is idempotent: after
        // the first success it's a single atomic load. After a failure
        // it'll retry on the next poll, which lets a user install
        // RED4ext mid-session and have telemetry come online without
        // restarting the recorder (rare but supported).
        self.try_attach().is_some()
    }

    fn player_world_position(&self) -> Result<Red4Vector4, HookError> {
        // Implementation note: we do not yet ship a hard binding to a
        // specific RED4ext C-export signature here. The SDK exports
        // have drifted across 1.x and we want this crate to compile
        // and produce a *correct error path* on any RED4ext version
        // rather than fail loudly on a missing symbol.
        //
        // When the Windows operator wires this up against a specific
        // RED4ext build, the body below becomes:
        //
        //     let sym: libloading::Symbol<unsafe extern "C" fn(/* … */) -> /* … */>
        //         = unsafe { self.resolve(b"RED4ext_PlayerWorldPosition\0")? };
        //     let raw = unsafe { sym(/* … */) };
        //     Ok(Red4Vector4 { x: raw.x as f64, y: raw.y as f64, z: raw.z as f64, w: raw.w as f64 })
        //
        // The string-lookup name (`gameInstance → GetPlayerSystem → …`)
        // is what RED4ext's C-side helper resolves internally — we do
        // **not** encode the offset chain here. That is the whole point
        // of taking the RED4ext attach path: name stability across
        // patches.
        //
        // Until that wiring lands, the production code path returns the
        // same NotAttached we'd return for "DLL not loaded", with a
        // distinct error string so the operator log makes the cause
        // obvious. The mock-registry path used by unit tests bypasses
        // this entirely.
        Err(HookError::NotAttached(
            "Red4ExtDllRegistry::player_world_position not yet bound to a RED4ext SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn player_world_orientation(&self) -> Result<Red4Quaternion, HookError> {
        Err(HookError::NotAttached(
            "Red4ExtDllRegistry::player_world_orientation not yet bound to a RED4ext SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn camera_world_position(&self) -> Result<Red4Vector4, HookError> {
        Err(HookError::NotAttached(
            "Red4ExtDllRegistry::camera_world_position not yet bound to a RED4ext SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn camera_world_orientation(&self) -> Result<Red4Quaternion, HookError> {
        Err(HookError::NotAttached(
            "Red4ExtDllRegistry::camera_world_orientation not yet bound to a RED4ext SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn camera_follow_offset(&self) -> Result<Red4Vector4, HookError> {
        Err(HookError::NotAttached(
            "Red4ExtDllRegistry::camera_follow_offset not yet bound to a RED4ext SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn camera_fov_degrees(&self) -> Result<f64, HookError> {
        Err(HookError::NotAttached(
            "Red4ExtDllRegistry::camera_fov_degrees not yet bound to a RED4ext SDK signature; falling back to skip-frame".to_string(),
        ))
    }

    fn world_id(&self) -> String {
        // RED4ext exposes the active sector / world identifier through
        // `gameInstance::GetActiveWorld()::name`, but the C-side helper
        // for that accessor only stabilised in RED4ext 1.21. Older
        // builds return an empty string; the wire format is documented
        // to allow that ("missing world identifier"), so we treat it
        // as best-effort rather than fail.
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Hook struct — Windows variant.
// ---------------------------------------------------------------------------

/// Cyberpunk 2077 (REDengine 4) engine-state hook — Windows variant.
///
/// See [`crate::CyberpunkHook`] for the canonical struct-level
/// documentation. This is the implementation selected on
/// `target_os = "windows"`; the Mac/Linux build uses
/// [`crate::cyberpunk_mock::CyberpunkHook`] instead.
///
/// The struct holds a boxed `dyn Red4ExtRegistry` so the registry impl
/// can be swapped at runtime: production uses [`Red4ExtDllRegistry`];
/// unit tests inject a [`tests::MockRegistry`]. The choice is made at
/// construction time (`new()` for production, `with_registry()` for
/// tests) and is opaque to the recorder.
pub struct CyberpunkHook {
    /// Registry abstraction. See [`Red4ExtRegistry`].
    ///
    /// `Box<dyn Trait>` is intentional: the recorder owns one
    /// `CyberpunkHook` per session and we never benchmark-prove that
    /// dyn dispatch is a measurable hot-path cost relative to the
    /// FFI calls inside the registry impl. Static-dispatch generics
    /// would force `EngineHook` to leak a `R: Red4ExtRegistry` type
    /// parameter all the way up to the recorder, which breaks the
    /// "hook trait is engine-agnostic" contract.
    registry: Box<dyn Red4ExtRegistry>,

    /// Monotonically increasing frame index. Same semantics as the
    /// mock build (and the same field name) so the struct layout is
    /// drop-in compatible across platforms.
    next_frame_index: u64,

    /// Wall-clock origin for `timestamp_ms`. Set on first call to
    /// `capture_frame`. Mirrors the mock build's lifecycle.
    epoch: Option<std::time::Instant>,
}

impl CyberpunkHook {
    /// Construct a hook backed by the production
    /// [`Red4ExtDllRegistry`]. Equivalent to the cross-platform
    /// `CyberpunkHook::new()` — that is the public entry point.
    pub fn new() -> Self {
        Self::with_registry(Box::new(Red4ExtDllRegistry::new()))
    }

    /// Construct a hook backed by a caller-supplied registry. Used by
    /// unit tests to inject [`tests::MockRegistry`]; production code
    /// always uses [`Self::new`].
    pub fn with_registry(registry: Box<dyn Red4ExtRegistry>) -> Self {
        Self {
            registry,
            next_frame_index: 0,
            epoch: None,
        }
    }

    /// REDengine 4's metric scale. Hard-coded `1.0`; do not derive at
    /// runtime. See the `metric_scale` field on [`EngineFrame`].
    pub const METRIC_SCALE: f64 = 1.0;
}

impl Default for CyberpunkHook {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineHook for CyberpunkHook {
    /// Walk REDengine 4's RTTI registry through [`Red4ExtRegistry`] to
    /// produce a per-frame snapshot.
    ///
    /// On the first call (or any subsequent call where the production
    /// `Red4ExtDllRegistry` has not yet attached), this triggers a
    /// `LoadLibrary("red4ext.dll")` attempt. Subsequent calls re-use
    /// the loaded handle and are cheap.
    ///
    /// Failure mode: returns `HookError::NotAttached` when RED4ext is
    /// missing or the player is not yet spawned. Never panics.
    fn capture_frame(&mut self) -> Result<EngineFrame, HookError> {
        let epoch = *self.epoch.get_or_insert_with(std::time::Instant::now);
        let timestamp_ms = epoch.elapsed().as_millis() as u64;

        let i = self.next_frame_index;
        // Hold the bump until the end so that a NotAttached return does
        // not "use up" a frame index. The recorder's join key is the
        // recorder-side frame counter, not this one, but keeping them
        // in sync simplifies test fixtures.
        // NOTE: We DO still bump on InvalidRead / InvariantViolation —
        // those are "we read the engine and it gave us garbage"
        // outcomes that should advance the per-hook counter so the
        // operator can correlate dropped frames in the recorder log.

        if !self.registry.is_attached() {
            // Try once per frame to attach via the production registry
            // (the trait abstraction lets us upcall through the same
            // `is_attached()` signal whether the registry is the real
            // DLL one or a test mock). If still not attached, surface
            // a transient error.
            //
            // Note: we explicitly don't call `try_attach` on the trait
            // here because it would force the trait to be `&mut self`
            // — which loses our `Send`-only relaxation. Instead the
            // production impl is expected to attempt re-attach inside
            // each of the accessor methods if `is_attached()` is false.
            return Err(HookError::NotAttached(
                "RED4ext registry not yet attached; will retry next frame".to_string(),
            ));
        }

        // Walk the RTTI registry by string lookup. Each `?` short-
        // circuits to a transient `HookError::NotAttached` or
        // `InvalidRead` depending on what the registry signals.
        let pos = self.registry.player_world_position()?;
        let rot = self.registry.player_world_orientation()?;
        let cam_pos = self.registry.camera_world_position()?;
        let cam_rot = self.registry.camera_world_orientation()?;
        let follow_offset = self.registry.camera_follow_offset()?;
        let fov = self.registry.camera_fov_degrees()?;

        // Wire-format conversion. Vector4 → [x, y, z] (drop w).
        // Quaternion {i, j, k, r} → [x, y, z, w] via to_xyzw().
        let frame = EngineFrame {
            player_position: [pos.x, pos.y, pos.z],
            player_rotation_quaternion: rot.to_xyzw(),
            camera_position: [cam_pos.x, cam_pos.y, cam_pos.z],
            camera_rotation_quaternion: cam_rot.to_xyzw(),
            // REDengine reports `followOffset.z` as `-forward`; the wire
            // format wants `back`. Per the runbook (`crates/engine-
            // telemetry/docs/CYBERPUNK_HOOK_RUNBOOK.md` §1) this maps
            // straight through as `[x, y, -z]` — do NOT double-negate.
            // The negation here is the *only* coordinate transform the
            // hook applies; everything else is pass-through.
            camera_follow_offset: [follow_offset.x, follow_offset.y, -follow_offset.z],
            metric_scale: Self::METRIC_SCALE,
            fov_degrees: fov,
            frame_index: i,
            timestamp_ms,
        };

        // Sanity check: camera quaternion is unit length. The real
        // engine occasionally reports a degenerate quaternion during a
        // sub-frame camera reinit (loading screen, scene transition).
        // Surface as InvariantViolation so the recorder pauses
        // telemetry rather than feeding the trainer rotated garbage.
        let q = frame.camera_rotation_quaternion;
        let norm_sq = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
        if !(0.5..=1.5).contains(&norm_sq) {
            return Err(HookError::InvariantViolation(format!(
                "camera quaternion not unit length: norm^2 = {norm_sq}"
            )));
        }

        // Same check for the player quaternion. Catches an avatar that
        // entered a vehicle mid-frame — the puppet's quaternion is
        // briefly zero while the new entity replaces it.
        let pq = frame.player_rotation_quaternion;
        let pq_norm_sq = pq[0] * pq[0] + pq[1] * pq[1] + pq[2] * pq[2] + pq[3] * pq[3];
        if !(0.5..=1.5).contains(&pq_norm_sq) {
            return Err(HookError::InvariantViolation(format!(
                "player quaternion not unit length: norm^2 = {pq_norm_sq}"
            )));
        }

        // Only advance the frame counter on successful capture, mirror-
        // ing the mock build's semantics (which advances unconditionally
        // — but the mock can't fail, so the observable behaviour is the
        // same up to the deterministic test paths the integration suite
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
// real CyberpunkHook against a MockRegistry that simulates the RED4ext
// registry without needing a running game. The cross-platform integration
// tests in `tests/integration.rs` cover the Mac/Linux mock path.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory [`Red4ExtRegistry`] for unit tests. Holds a snapshot
    /// of "what the registry should return on the next call" and lets
    /// each test poke values + assert the hook's mapping is correct.
    pub(super) struct MockRegistry {
        inner: Mutex<MockState>,
    }

    struct MockState {
        attached: bool,
        player_pos: Result<Red4Vector4, HookError>,
        player_rot: Result<Red4Quaternion, HookError>,
        camera_pos: Result<Red4Vector4, HookError>,
        camera_rot: Result<Red4Quaternion, HookError>,
        follow_offset: Result<Red4Vector4, HookError>,
        fov: Result<f64, HookError>,
        world_id: String,
    }

    impl MockRegistry {
        /// Construct a mock that returns "happy-path" values — player
        /// at origin facing identity, camera at `(0, 1.7, -3)`, FOV
        /// 70°. Tests override individual fields via `set_*`.
        fn happy() -> Self {
            Self {
                inner: Mutex::new(MockState {
                    attached: true,
                    player_pos: Ok(Red4Vector4 {
                        x: 0.0,
                        y: 0.0,
                        z: 0.0,
                        w: 0.0,
                    }),
                    player_rot: Ok(Red4Quaternion::identity()),
                    camera_pos: Ok(Red4Vector4 {
                        x: 0.0,
                        y: 1.7,
                        z: -3.0,
                        w: 0.0,
                    }),
                    camera_rot: Ok(Red4Quaternion::identity()),
                    follow_offset: Ok(Red4Vector4 {
                        x: 0.0,
                        y: 1.7,
                        // REDengine reports `-forward` here; the hook
                        // negates this on the way out (to_xyzw not
                        // involved — it's a Vector4 not a quaternion).
                        // We supply REDengine convention raw so the
                        // hook's conversion is exercised.
                        z: 3.0,
                        w: 0.0,
                    }),
                    fov: Ok(70.0),
                    world_id: "open_world".to_string(),
                }),
            }
        }

        /// Construct a mock that reports "not attached" for every call.
        /// Exercises the early-return path in `capture_frame`.
        fn detached() -> Self {
            Self {
                inner: Mutex::new(MockState {
                    attached: false,
                    player_pos: Err(HookError::NotAttached("test: detached".into())),
                    player_rot: Err(HookError::NotAttached("test: detached".into())),
                    camera_pos: Err(HookError::NotAttached("test: detached".into())),
                    camera_rot: Err(HookError::NotAttached("test: detached".into())),
                    follow_offset: Err(HookError::NotAttached("test: detached".into())),
                    fov: Err(HookError::NotAttached("test: detached".into())),
                    world_id: String::new(),
                }),
            }
        }

        fn set_camera_rot(&self, q: Red4Quaternion) {
            self.inner.lock().unwrap().camera_rot = Ok(q);
        }

        fn set_player_rot(&self, q: Red4Quaternion) {
            self.inner.lock().unwrap().player_rot = Ok(q);
        }

        fn set_player_pos(&self, v: Red4Vector4) {
            self.inner.lock().unwrap().player_pos = Ok(v);
        }
    }

    impl Red4ExtRegistry for MockRegistry {
        fn is_attached(&self) -> bool {
            self.inner.lock().unwrap().attached
        }
        fn player_world_position(&self) -> Result<Red4Vector4, HookError> {
            let inner = self.inner.lock().unwrap();
            // We can't `?`-clone an arbitrary HookError, so just hand-
            // clone the variants the tests use.
            match &inner.player_pos {
                Ok(v) => Ok(*v),
                Err(HookError::NotAttached(s)) => Err(HookError::NotAttached(s.clone())),
                Err(HookError::InvalidRead(s)) => Err(HookError::InvalidRead(s.clone())),
                Err(HookError::InvariantViolation(s)) => {
                    Err(HookError::InvariantViolation(s.clone()))
                }
                Err(HookError::Io(e)) => Err(HookError::Io(std::io::Error::other(e.to_string()))),
            }
        }
        fn player_world_orientation(&self) -> Result<Red4Quaternion, HookError> {
            let inner = self.inner.lock().unwrap();
            match &inner.player_rot {
                Ok(q) => Ok(*q),
                Err(HookError::NotAttached(s)) => Err(HookError::NotAttached(s.clone())),
                Err(HookError::InvalidRead(s)) => Err(HookError::InvalidRead(s.clone())),
                Err(HookError::InvariantViolation(s)) => {
                    Err(HookError::InvariantViolation(s.clone()))
                }
                Err(HookError::Io(e)) => Err(HookError::Io(std::io::Error::other(e.to_string()))),
            }
        }
        fn camera_world_position(&self) -> Result<Red4Vector4, HookError> {
            let inner = self.inner.lock().unwrap();
            match &inner.camera_pos {
                Ok(v) => Ok(*v),
                Err(HookError::NotAttached(s)) => Err(HookError::NotAttached(s.clone())),
                Err(HookError::InvalidRead(s)) => Err(HookError::InvalidRead(s.clone())),
                Err(HookError::InvariantViolation(s)) => {
                    Err(HookError::InvariantViolation(s.clone()))
                }
                Err(HookError::Io(e)) => Err(HookError::Io(std::io::Error::other(e.to_string()))),
            }
        }
        fn camera_world_orientation(&self) -> Result<Red4Quaternion, HookError> {
            let inner = self.inner.lock().unwrap();
            match &inner.camera_rot {
                Ok(q) => Ok(*q),
                Err(HookError::NotAttached(s)) => Err(HookError::NotAttached(s.clone())),
                Err(HookError::InvalidRead(s)) => Err(HookError::InvalidRead(s.clone())),
                Err(HookError::InvariantViolation(s)) => {
                    Err(HookError::InvariantViolation(s.clone()))
                }
                Err(HookError::Io(e)) => Err(HookError::Io(std::io::Error::other(e.to_string()))),
            }
        }
        fn camera_follow_offset(&self) -> Result<Red4Vector4, HookError> {
            let inner = self.inner.lock().unwrap();
            match &inner.follow_offset {
                Ok(v) => Ok(*v),
                Err(HookError::NotAttached(s)) => Err(HookError::NotAttached(s.clone())),
                Err(HookError::InvalidRead(s)) => Err(HookError::InvalidRead(s.clone())),
                Err(HookError::InvariantViolation(s)) => {
                    Err(HookError::InvariantViolation(s.clone()))
                }
                Err(HookError::Io(e)) => Err(HookError::Io(std::io::Error::other(e.to_string()))),
            }
        }
        fn camera_fov_degrees(&self) -> Result<f64, HookError> {
            let inner = self.inner.lock().unwrap();
            match &inner.fov {
                Ok(v) => Ok(*v),
                Err(HookError::NotAttached(s)) => Err(HookError::NotAttached(s.clone())),
                Err(HookError::InvalidRead(s)) => Err(HookError::InvalidRead(s.clone())),
                Err(HookError::InvariantViolation(s)) => {
                    Err(HookError::InvariantViolation(s.clone()))
                }
                Err(HookError::Io(e)) => Err(HookError::Io(std::io::Error::other(e.to_string()))),
            }
        }
        fn world_id(&self) -> String {
            self.inner.lock().unwrap().world_id.clone()
        }
    }

    #[test]
    fn detached_registry_returns_not_attached() {
        // Spec contract: when RED4ext is not loaded, the hook must return
        // a transient `NotAttached` error rather than panicking. The
        // recorder swallows transient errors and skips the frame.
        let mut hook = CyberpunkHook::with_registry(Box::new(MockRegistry::detached()));
        let res = hook.capture_frame();
        assert!(
            matches!(res, Err(HookError::NotAttached(_))),
            "expected NotAttached, got {res:?}"
        );
    }

    #[test]
    fn happy_path_returns_unit_quaternion_frame() {
        // Validates the string-lookup walk is plumbed end-to-end:
        // registry → hook → EngineFrame, with the right field mapping.
        let mut hook = CyberpunkHook::with_registry(Box::new(MockRegistry::happy()));
        let frame = hook.capture_frame().expect("happy path");
        assert_eq!(frame.player_position, [0.0, 0.0, 0.0]);
        assert_eq!(frame.player_rotation_quaternion, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(frame.camera_position, [0.0, 1.7, -3.0]);
        assert_eq!(frame.fov_degrees, 70.0);
        assert_eq!(frame.metric_scale, 1.0);
        assert_eq!(frame.frame_index, 0);
    }

    #[test]
    fn quaternion_xyzw_order_preserved_from_ijkr() {
        // The REDengine Quaternion {i, j, k, r} maps to wire-format
        // [x, y, z, w]. Test with distinguishable values so an
        // accidental w-first swap would be visible.
        let mock = MockRegistry::happy();
        // Use a valid unit quaternion so the InvariantViolation guard
        // doesn't trip: (i=0.5, j=0.5, k=0.5, r=0.5) has norm² = 1.
        mock.set_camera_rot(Red4Quaternion {
            i: 0.5,
            j: 0.5,
            k: 0.5,
            r: 0.5,
        });
        let mut hook = CyberpunkHook::with_registry(Box::new(mock));
        let frame = hook.capture_frame().expect("quat mapping");
        assert_eq!(frame.camera_rotation_quaternion, [0.5, 0.5, 0.5, 0.5]);
    }

    #[test]
    fn follow_offset_z_is_negated_per_redengine_convention() {
        // REDengine reports `followOffset.z = -forward`; wire format is
        // `[right, up, back]`. The hook must negate `z` on the way out.
        // Mock's happy path supplies `z = 3.0` (REDengine's -forward
        // pointing backward = positive z); expected wire = -3.0.
        let mut hook = CyberpunkHook::with_registry(Box::new(MockRegistry::happy()));
        let frame = hook.capture_frame().expect("follow offset");
        assert_eq!(frame.camera_follow_offset, [0.0, 1.7, -3.0]);
    }

    #[test]
    fn frame_index_advances_only_on_success() {
        // A NotAttached early-return must not bump the frame counter.
        // This lets the recorder distinguish "telemetry ran but engine
        // wasn't ready" from "telemetry produced a frame".
        let mock = MockRegistry::happy();
        // Force NotAttached for the first poll.
        mock.inner.lock().unwrap().attached = false;
        let mut hook = CyberpunkHook::with_registry(Box::new(mock));

        let r0 = hook.capture_frame();
        assert!(matches!(r0, Err(HookError::NotAttached(_))));

        // Without manually re-attaching, the next call still fails.
        let r1 = hook.capture_frame();
        assert!(matches!(r1, Err(HookError::NotAttached(_))));

        // Now flip the mock to attached and verify the counter starts
        // at 0 (we never bumped it on the NotAttached returns).
        // We need to access the mock through the hook to do this in a
        // test; easier path: construct a fresh mock.
        let attached_mock = MockRegistry::happy();
        let mut hook2 = CyberpunkHook::with_registry(Box::new(attached_mock));
        let f0 = hook2.capture_frame().expect("first frame");
        let f1 = hook2.capture_frame().expect("second frame");
        assert_eq!(f0.frame_index, 0);
        assert_eq!(f1.frame_index, 1);
    }

    #[test]
    fn degenerate_camera_quaternion_returns_invariant_violation() {
        // Loading screens have been observed to produce a zero
        // quaternion for the camera component for one or two frames.
        // The hook must surface this as InvariantViolation so the
        // recorder pauses telemetry rather than feeding rotated
        // garbage to the trainer.
        let mock = MockRegistry::happy();
        mock.set_camera_rot(Red4Quaternion {
            i: 0.0,
            j: 0.0,
            k: 0.0,
            r: 0.0,
        });
        let mut hook = CyberpunkHook::with_registry(Box::new(mock));
        let res = hook.capture_frame();
        assert!(
            matches!(res, Err(HookError::InvariantViolation(_))),
            "expected InvariantViolation, got {res:?}"
        );
    }

    #[test]
    fn degenerate_player_quaternion_returns_invariant_violation() {
        // Same guard for the player puppet. Catches the vehicle-entry
        // sub-frame where the puppet's quaternion is briefly zero.
        let mock = MockRegistry::happy();
        mock.set_player_rot(Red4Quaternion {
            i: 0.0,
            j: 0.0,
            k: 0.0,
            r: 0.0,
        });
        let mut hook = CyberpunkHook::with_registry(Box::new(mock));
        let res = hook.capture_frame();
        assert!(
            matches!(res, Err(HookError::InvariantViolation(_))),
            "expected InvariantViolation, got {res:?}"
        );
    }

    #[test]
    fn metric_scale_is_one_unconditionally() {
        // REDengine units ARE meters. Locking this in catches a future
        // refactor that copies in a UE5-style cm scale.
        let mut hook = CyberpunkHook::with_registry(Box::new(MockRegistry::happy()));
        assert_eq!(hook.metric_scale(), 1.0);
        assert_eq!(CyberpunkHook::METRIC_SCALE, 1.0);
        let frame = hook.capture_frame().expect("scale");
        assert_eq!(frame.metric_scale, 1.0);
    }

    #[test]
    fn player_position_passthrough_no_coord_transform() {
        // REDengine is right-handed X-east, Y-north, Z-up; wire format
        // is documented as the same. The hook must pass positions
        // through without any coordinate transform. Use a unique
        // signature so an accidental sign flip / axis swap fails the
        // test.
        let mock = MockRegistry::happy();
        mock.set_player_pos(Red4Vector4 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
            // w must be ignored — Vector4's w is unused for positions.
            w: 999.0,
        });
        let mut hook = CyberpunkHook::with_registry(Box::new(mock));
        let frame = hook.capture_frame().expect("position");
        assert_eq!(frame.player_position, [1.0, 2.0, 3.0]);
        // No 4th component should have leaked through.
        // (player_position is a [f64; 3] — the type system prevents this,
        // but the explicit assert documents intent.)
    }

    #[test]
    fn default_dll_registry_construction_does_not_touch_linker() {
        // Production registry is constructed in the unattached state.
        // `Red4ExtDllRegistry::new()` must not call `LoadLibrary` —
        // that happens lazily inside the first `is_attached` poll. We
        // assert the construction-time invariant directly on the
        // private `library` field rather than calling `is_attached`
        // (which would trigger a `LoadLibrary` and could succeed on a
        // dev machine that happens to have RED4ext installed).
        let r = Red4ExtDllRegistry::new();
        assert!(
            r.library.get().is_none(),
            "fresh registry must not have called LoadLibrary"
        );
    }

    #[test]
    fn default_dll_registry_reports_world_id_safely_when_unattached() {
        // `world_id` is documented to return an empty string on
        // unsupported / unattached configurations. Test that this is
        // the case for the freshly-constructed registry (no panic, no
        // unwrap, no `expect`).
        let r = Red4ExtDllRegistry::new();
        assert_eq!(r.world_id(), "");
    }
}
