//! Tiny stub exposing `durable_write` at `crate::util::durable_write` so the
//! source-included `action_camera_writer.rs` (which does
//! `use crate::util::durable_write;`) resolves unmodified.
//!
//! Rust's `#[path]` for inline nested modules is resolved relative to the
//! parent module's source file plus the module name (Rust 2018+ rules). By
//! pointing this file at the outer level (it's `crate::util`, declared from
//! `lib.rs`), the inner `pub mod durable_write` `#[path]` resolves relative
//! to *this* file's directory: `crates/action-camera-tests/src/`. Three `../`
//! hops then land on `gamedata-recorder/` — one level above `src/`.

#[path = "../../../src/util/durable_write.rs"]
pub mod durable_write;
