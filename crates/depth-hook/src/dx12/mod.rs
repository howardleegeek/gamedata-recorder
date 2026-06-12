//! DX12 hook scaffolding.
//!
//! Every item in this module is gated on `#[cfg(windows)]`. On non-Windows
//! platforms the module is intentionally empty so `cargo check` on a Mac
//! compiles the crate without needing `windows-rs` in the dependency tree.
//!
//! # How the hook is structured (implementation plan)
//!
//! The Windows engineer picking this up should implement the following
//! sequence in `hook.rs`:
//!
//! 1. **Bootstrap D3D12.** Create a throwaway device + command queue and
//!    fish `ID3D12CommandQueue::ExecuteCommandLists` out of the vtable at
//!    a known offset (slot 10 on current SDKs; always verify against the
//!    running process's copy of `d3d12.dll`, do not hard-code an RVA).
//!
//! 2. **Install an inline hook.** Use `retour` (or `detours-rs`) to detour
//!    `ExecuteCommandLists`. Keep the trampoline so the game continues to
//!    submit normally; the hook's only job is to observe.
//!
//! 3. **Observe every frame.** For each submission, walk the command lists
//!    and track which `ID3D12Resource`s were bound as DSVs, which were
//!    cleared with `ClearDepthStencilView`, their dimensions, and their
//!    formats.
//!
//! 4. **Pick the canonical depth buffer per frame** using the profile's
//!    `DetectionHeuristic` (see [`crate::types::DetectionHeuristic`] for
//!    the contract and the ReShade Generic Depth lineage).
//!
//! 5. **Copy depth to a CPU-readable resource.** Allocate a readback heap
//!    matching `profile.depth_format()`, record a `CopyResource` into it
//!    after the depth buffer's final write for the frame, and fence on
//!    completion before mapping.
//!
//! 6. **Read camera matrices.** `profile.near_far_from_matrix(proj)` takes
//!    the projection from whichever CB slot the profile identifies (for
//!    Cyberpunk 2077 this reverse-engineering is still TODO). View is
//!    usually adjacent to projection in the same CB.
//!
//! 7. **Push a `DepthFrame` onto the outbound queue** for the recorder to
//!    drain via `CaptureSession::take_frames`.
//!
//! # Why we aren't implementing this in the scaffold commit
//!
//! Real DX12 hooking is not a thing that survives "write it blind and
//! validate later". It needs:
//!
//! - a Windows 11 box with the target title installed,
//! - RenderDoc or PIX running to sanity-check the chosen DSV,
//! - iteration on ExecuteCommandLists' vtable offset across SDK versions,
//! - anti-cheat compatibility testing (REDmod / REDLauncher in CP2077's
//!   case, which is permissive but still sees our detour),
//! - and `windows-rs` + `retour` crates which pull in a Windows toolchain
//!   we do not want to require on the Mac developer box that does every
//!   non-DX12 piece of the recorder.
//!
//! So this scaffold commit establishes the API shape that the real hook
//! will plug into (`DxHook::install` / `drain_frames` / Drop) and leaves
//! the body for the Windows engineer.

#[cfg(windows)]
pub(crate) mod hook;

#[cfg(windows)]
pub(crate) use hook::DxHook;
