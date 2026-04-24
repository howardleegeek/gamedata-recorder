//! Windows-only DX12 command-queue hook.
//!
//! Scaffold only. See `super`'s module doc-comment for the implementation
//! plan. Everything here is behind `#[cfg(windows)]` so the Mac / Linux
//! build of `depth-hook` does not pull in this file at all.

use std::sync::Arc;

use crate::profiles::DepthHookProfile;
use crate::types::DepthFrame;

/// Handle to an installed DX12 hook.
///
/// The real implementation will own:
/// - the MinHook / retour trampoline for `ExecuteCommandLists`,
/// - a readback heap matching `profile.depth_format()`,
/// - a fence + event pair for CPU/GPU synchronisation,
/// - a lock-free queue of captured frames the recorder drains each tick.
///
/// For the scaffold we only carry the profile so `drain_frames()` has
/// something real to log against.
pub(crate) struct DxHook {
    profile: Arc<dyn DepthHookProfile>,
}

impl DxHook {
    /// Install the DX12 command-queue hook.
    ///
    /// TODO(windows-engineer): real implementation
    /// 1. Create throwaway `ID3D12Device` + `ID3D12CommandQueue` via
    ///    `D3D12CreateDevice`.
    /// 2. Read `ExecuteCommandLists` out of the command queue's vtable
    ///    (slot 10 on current SDKs — verify against the target process's
    ///    `d3d12.dll` at runtime; do not hard-code RVAs).
    /// 3. Install a `retour::RawDetour` on that address.
    /// 4. Allocate a readback heap sized for `profile.depth_format()` at
    ///    1080p (and be ready to resize if the game switches resolution).
    /// 5. Return `Ok(Self { ... })`.
    ///
    /// Stub returns Ok with the profile stored so test code can exercise
    /// the Drop path.
    pub(crate) fn install(profile: Arc<dyn DepthHookProfile>) -> Result<Self, String> {
        tracing::warn!(
            profile = profile.name(),
            "DxHook::install is a scaffold; no actual DX12 hook installed. \
             See crate::dx12 module docs for the implementation plan."
        );
        Ok(Self { profile })
    }

    /// Drain all captured frames since the last call.
    ///
    /// TODO(windows-engineer): real implementation pops from the lock-free
    /// queue populated by the hooked `ExecuteCommandLists`. Scaffold
    /// returns empty so the recorder's polling loop is a no-op on a
    /// non-hooked process.
    pub(crate) fn drain_frames(&mut self) -> Vec<DepthFrame> {
        Vec::new()
    }
}

impl Drop for DxHook {
    fn drop(&mut self) {
        tracing::info!(
            profile = self.profile.name(),
            "DxHook dropped; uninstalling (scaffold — nothing to uninstall yet)"
        );
        // TODO(windows-engineer): uninstall the retour detour here. The
        // trampoline must outlive the final `ExecuteCommandLists` call on
        // any thread, so uninstall needs to fence through the GPU queue
        // before detaching. See retour docs for the correct teardown order.
    }
}
