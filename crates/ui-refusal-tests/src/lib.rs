//! Cross-platform shim around `src/record/ui_refusal_detector.rs`.
//!
//! See `Cargo.toml` for context. The detector source file is
//! source-included so the same code runs in both the top-level
//! Windows-only build and the cross-platform CI tests.

#[path = "../../../src/record/ui_refusal_detector.rs"]
pub mod ui_refusal_detector;

pub use ui_refusal_detector::{
    BLACKLISTED_OVERLAY_EXES, RefusalDetector, UiRefusalReason, WindowSnapshot, capture_frame_hash,
    capture_window_snapshot,
};
