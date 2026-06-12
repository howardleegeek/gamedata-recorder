//! Cross-platform shim around `src/record/action_camera_writer.rs`.
//!
//! This crate source-includes two files from the top-level `gamedata-recorder`
//! crate via `#[path = ...]`:
//!
//!   - `src/util/durable_write.rs` (atomic-write helper)
//!   - `src/record/action_camera_writer.rs` (per-frame action_camera.json
//!     replay + writer)
//!
//! The writer file uses `crate::util::durable_write` internally. By exposing
//! a `util::durable_write` module here at the crate root (via `util_mod.rs`
//! which itself re-paths the source-included file), the writer compiles
//! unmodified.
//!
//! Building only this crate on macOS/Linux CI lets us run the writer's
//! `#[cfg(test)]` unit tests + integration tests in `tests/` without pulling
//! the Windows-only deps (libobs-wrapper, glfw, tray-icon, egui_overlay)
//! that the top-level crate requires.

#[path = "util_mod.rs"]
pub mod util;

#[path = "../../../src/record/action_camera_writer.rs"]
pub mod action_camera_writer;

// Re-export the public surface the integration tests need.
pub use action_camera_writer::{ActionCameraRecord, write_action_camera_json};
