//! `depth-hook` — per-title DX12 depth-buffer capture for gamedata-recorder.
//!
//! # Why this crate exists
//!
//! The commodity tier of gameplay-data suppliers (Chinese network-café
//! video farms, UGC footage scrapers) produces raw video + input logs.
//! That is a race to the bottom on margin. This crate is how we produce
//! ground-truth 3D data — depth buffer + camera matrices per frame,
//! straight from the GPU — which cannot be replicated without per-title
//! DX12 reverse engineering.
//!
//! Every profile added to this crate is one more title where
//! gamedata-recorder ships the $300–800/hr enriched-tier depth data
//! instead of $30–80/hr raw video. Profiles compound: each one reuses
//! the heuristic and hook infrastructure built for the last one.
//!
//! # Architecture
//!
//! - [`types`] — platform-agnostic types (`Matrix4`, `DepthFormat`,
//!   `DepthFrame`, `CameraMatrices`, `DetectionHeuristic`). Pure Rust,
//!   compiles everywhere.
//! - [`profiles`] — per-title [`DepthHookProfile`] implementations plus
//!   the [`ProfileRegistry`] that looks them up by executable stem.
//!   Pure Rust, compiles everywhere.
//! - [`dx12`] — Windows-only DX12 command-queue hook scaffolding (empty
//!   module on other platforms). Where the Windows engineer will plug
//!   in `windows-rs` + `retour` / `detours-rs` for real hooking.
//! - [`capture`] — `CaptureSession` orchestration that ties a profile to
//!   an installed hook. Cfg-gated internally so the public API is
//!   identical across platforms.
//!
//! # Public API example
//!
//! ```no_run
//! use depth_hook::profiles::ProfileRegistry;
//! use depth_hook::capture::CaptureSession;
//!
//! let registry = ProfileRegistry::with_builtin_profiles();
//! if let Some(profile) = registry.find_for_exe_stem("cyberpunk2077") {
//!     let mut session = CaptureSession::start(profile).expect("hook install");
//!     // … inside recorder tick loop …
//!     let _frames = session.take_frames();
//!     // Drop session to uninstall the hook.
//! }
//! ```
//!
//! # Adding a new profile
//!
//! 1. Create `src/profiles/<title>.rs` implementing `DepthHookProfile`.
//! 2. Add it to `ProfileRegistry::with_builtin_profiles`.
//! 3. Confirm the exe stem also appears in
//!    `crates/constants/src/lib.rs`'s `GAME_WHITELIST` — otherwise the
//!    recorder won't even record that title, and your depth hook will
//!    never be invoked.
//!
//! See `README.md` for a worked skeleton.

#![warn(missing_docs)]

pub mod capture;
pub mod dx12;
pub mod profiles;
pub mod types;

// Re-exports so the public surface is one-stop.
pub use capture::{CaptureError, CaptureSession};
pub use profiles::{DepthHookProfile, ProfileRegistry};
pub use types::{CameraMatrices, DepthFormat, DepthFrame, DetectionHeuristic, Matrix4};
