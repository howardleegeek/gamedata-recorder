//! Cross-platform mock body for [`crate::CyberpunkHook`].
//!
//! Compiled on every platform *except* `target_os = "windows"`. Emits a
//! deterministic walking-along-`+X` mock frame so the cross-platform tests
//! in `tests/integration.rs` and the in-file unit tests in `lib.rs` can
//! validate the JSON contract end-to-end without a running REDengine 4
//! process. The Windows build swaps this out for the real RED4ext / RTTI
//! walker in [`crate::cyberpunk_windows`].
//!
//! The mock body is intentionally identical (byte-for-byte field values)
//! to the body that lived in `lib.rs` before the platform split — that is
//! the explicit contract of the split: a no-op refactor on Mac/Linux.

use crate::{EngineFrame, EngineHook, HookError};

/// Cyberpunk 2077 (REDengine 4) engine-state hook — mock half.
///
/// See [`crate::CyberpunkHook`] for the canonical doc-comment (struct-level
/// RTTI walk reference, attach-surface notes, DX12 timing contract). This
/// re-export only carries the mock-only fields so the Windows build can
/// substitute a different struct shape behind the platform-cfg.
///
/// Field names and the `next_frame_index` / `epoch` semantics are kept
/// identical to the pre-split scaffold so an in-repo grep for "next_frame_index"
/// still resolves uniformly across platforms.
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
    /// Mock-only: simply zeroes the counter. The Windows impl performs
    /// the same surface-level construction (no RED4ext I/O happens here —
    /// it's lazily resolved on first `capture_frame`).
    pub fn new() -> Self {
        Self {
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
    /// Mock implementation. Emits a deterministic frame so the
    /// cross-platform tests can validate the JSON contract end-to-end
    /// without a running game. The real RTTI walker lives in
    /// [`crate::cyberpunk_windows`] and is selected automatically on
    /// `target_os = "windows"`.
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
