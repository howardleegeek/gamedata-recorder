//! Tiny stub exposing `durable_write` at `crate::util::durable_write` so any
//! source-included file that does `use crate::util::durable_write;` resolves
//! unmodified.
//!
//! Same pattern as `crates/action-camera-tests/src/util_mod.rs` — Rust's
//! `#[path]` resolution for a nested module is relative to the parent's
//! source file. Three `../` hops land on the repo root (one level above
//! `src/`), then we point at the real durable_write module.

#[path = "../../../src/util/durable_write.rs"]
pub mod durable_write;
