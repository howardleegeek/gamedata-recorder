//! Depth-buffer capture orchestration.
//!
//! This is the glue between a [`crate::profiles::DepthHookProfile`] and the
//! real DX12 hook in [`crate::dx12`]. It is deliberately thin — all the
//! title-specific knowledge lives in the profile, all the Win32 plumbing
//! lives in `dx12::`, and this module just owns the start / stop / tick
//! lifecycle and the outgoing `DepthFrame` queue.
//!
//! Today this compiles on every platform because the DX12 hook is a stub
//! on non-Windows. The `install_hook()` / `tick()` signatures intentionally
//! match what the future Windows implementation will expose, so once the
//! real hook lands no caller changes.

use std::sync::Arc;

use crate::profiles::DepthHookProfile;
use crate::types::DepthFrame;

/// One-stop handle for a live depth-capture session.
///
/// Lifecycle:
/// 1. `CaptureSession::start(profile)` — installs the DX12 hook (Windows) or
///    returns a no-op session (other platforms).
/// 2. Recorder polls `take_frames()` each tick to drain any depth frames the
///    hook has produced since the last poll.
/// 3. `CaptureSession::stop()` on Drop — uninstalls the hook cleanly.
pub struct CaptureSession {
    profile: Arc<dyn DepthHookProfile>,
    #[cfg(windows)]
    hook: crate::dx12::DxHook,
}

impl CaptureSession {
    /// Start a capture session for the given profile.
    ///
    /// On Windows this installs the DX12 hook described by
    /// [`crate::dx12`]. On other platforms it returns `Ok` and does nothing
    /// — the resulting session's `take_frames()` always returns empty, which
    /// is the correct behaviour for the recorder running on a Mac CI job.
    pub fn start(profile: Arc<dyn DepthHookProfile>) -> Result<Self, CaptureError> {
        tracing::info!(
            profile = profile.name(),
            targets = ?profile.game_exe_stems(),
            "Starting depth capture session"
        );

        #[cfg(windows)]
        {
            let hook = crate::dx12::DxHook::install(profile.clone())
                .map_err(CaptureError::HookInstallFailed)?;
            Ok(Self { profile, hook })
        }

        #[cfg(not(windows))]
        {
            tracing::warn!(
                "depth-hook: non-Windows platform, capture is a no-op (real hook is \
                 Windows-only — see crate docs)"
            );
            Ok(Self { profile })
        }
    }

    /// Drain any depth frames produced since the last call.
    ///
    /// Returns `Vec::new()` on non-Windows. On Windows this pops everything
    /// currently in the DX hook's outbound queue; callers should call this
    /// at least as often as the recorder's frame cadence (30 Hz) to keep
    /// the queue bounded.
    pub fn take_frames(&mut self) -> Vec<DepthFrame> {
        #[cfg(windows)]
        {
            self.hook.drain_frames()
        }

        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }

    /// Name of the active profile (for logging / telemetry).
    pub fn profile_name(&self) -> &'static str {
        self.profile.name()
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        tracing::info!(
            profile = self.profile.name(),
            "Dropping depth capture session"
        );
        // On Windows, the DxHook's own Drop impl uninstalls the hook. We
        // intentionally do not put uninstall logic here — RAII on `hook`
        // keeps install / uninstall symmetric.
    }
}

/// Errors produced while starting or running a capture session.
#[derive(Debug)]
pub enum CaptureError {
    /// The DX12 hook failed to install. On Windows this is almost always
    /// caused by (a) the target process not running, (b) a DRM overlay
    /// already holding the command-queue vtable, or (c) the target title
    /// using DX11 instead of DX12 (wrong profile selected).
    HookInstallFailed(String),
    /// The profile claims to support the running executable but the
    /// detection heuristic rejected every candidate depth buffer. Usually
    /// means a game patch changed the render graph — bump the profile.
    NoDepthBufferFound,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HookInstallFailed(msg) => write!(f, "DX12 hook install failed: {msg}"),
            Self::NoDepthBufferFound => {
                write!(f, "profile heuristic did not match any depth buffer")
            }
        }
    }
}

impl std::error::Error for CaptureError {}
