//! Stream BN (rc17.2): cross-platform shim around
//! `src/record/validation.rs`.
//!
//! This crate source-includes `src/record/validation.rs` from the
//! top-level `gamedata-recorder` crate via `#[path = ...]`. The included
//! file references `crate::ui::notification::post_session_toast`, so we
//! expose a local stub at `crate::ui::notification` whose
//! `post_session_toast` is a no-op tracing log. The real Windows toast
//! is in the parent crate's `src/ui/notification.rs` and is only
//! exercised by manual smoke tests on a Windows host.
//!
//! Building only this crate on macOS / Linux lets us run validation's
//! unit + integration tests (parser, writer, run_lint_v3 with a stub
//! python script) without dragging in libobs-wrapper / glfw / tray-icon
//! / egui_overlay (the Windows-only top-level deps).

pub mod ui {
    pub mod notification {
        /// Stub stand-in for the real
        /// `gamedata_recorder::ui::notification::post_session_toast`. On
        /// non-Windows hosts the production code also no-ops, so this
        /// preserves observable behaviour for unit tests.
        pub fn post_session_toast(title: &str, body: &str, click_dir: &std::path::Path) {
            tracing::info!(
                title = %title,
                body = %body,
                click_dir = %click_dir.display(),
                "validation-tests stub: post_session_toast no-op"
            );
        }
    }
}

#[path = "../../../src/record/validation.rs"]
pub mod validation;

pub use validation::{
    EXPECTED_TOTAL_CRITERIA, LINT_RESULT_FILENAME, LintFailure, LintResult, run_lint_v3,
};
