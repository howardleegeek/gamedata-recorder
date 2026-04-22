//! Window capture validation utilities.
//!
//! This module provides validation functions to ensure Window Capture
//! targets the correct game window and doesn't accidentally capture
//! the recorder's own UI or other non-game windows.

use color_eyre::{Result, eyre::eyre};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

/// Minimum reasonable game window dimensions.
///
/// Modern games typically run at resolutions >= 1280x720.
/// Smaller dimensions suggest we're capturing a non-game window.
pub const MIN_GAME_WINDOW_WIDTH: u32 = 1280;
pub const MIN_GAME_WINDOW_HEIGHT: u32 = 720;

/// Maximum reasonable game window dimensions.
///
/// Larger than 8K suggests we're capturing the desktop or a monitor.
pub const MAX_GAME_WINDOW_WIDTH: u32 = 7680;
pub const MAX_GAME_WINDOW_HEIGHT: u32 = 4320;

/// Validation error types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// HWND belongs to a different process than expected.
    PidMismatch { hwnd_pid: u32, expected_pid: u32 },
    /// Window dimensions are too small for a game window.
    DimensionsTooSmall {
        width: u32,
        height: u32,
        min_width: u32,
        min_height: u32,
    },
    /// Window dimensions are too large (likely desktop/monitor).
    DimensionsTooLarge {
        width: u32,
        height: u32,
        max_width: u32,
        max_height: u32,
    },
    /// Window area is suspiciously small (weird aspect ratio).
    AreaTooSmall { area: u64, min_area: u64 },
    /// Failed to get window information.
    WindowInfoUnavailable,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::PidMismatch {
                hwnd_pid,
                expected_pid,
            } => {
                write!(
                    f,
                    "HWND belongs to PID {} but expected PID {}",
                    hwnd_pid, expected_pid
                )
            }
            ValidationError::DimensionsTooSmall {
                width,
                height,
                min_width,
                min_height,
            } => {
                write!(
                    f,
                    "Window dimensions {}x{} are too small (minimum: {}x{})",
                    width, height, min_width, min_height
                )
            }
            ValidationError::DimensionsTooLarge {
                width,
                height,
                max_width,
                max_height,
            } => {
                write!(
                    f,
                    "Window dimensions {}x{} are too large (maximum: {}x{})",
                    width, height, max_width, max_height
                )
            }
            ValidationError::AreaTooSmall { area, min_area } => {
                write!(
                    f,
                    "Window area {} pixels is too small (minimum: {} pixels)",
                    area, min_area
                )
            }
            ValidationError::WindowInfoUnavailable => {
                write!(f, "Window information is unavailable")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validates that an HWND belongs to the expected process ID.
///
/// # Arguments
/// * `hwnd` - The window handle to validate
/// * `expected_pid` - The process ID that should own this window
///
/// # Returns
/// * `Ok(())` if the HWND belongs to the expected PID
/// * `Err(ValidationError::PidMismatch)` if the PIDs don't match
pub fn validate_window_pid(hwnd: HWND, expected_pid: u32) -> Result<()> {
    let hwnd_pid = get_window_pid(hwnd)?;

    if hwnd_pid != expected_pid {
        return Err(ValidationError::PidMismatch {
            hwnd_pid,
            expected_pid,
        }
        .into());
    }

    Ok(())
}

/// Gets the process ID that owns a window.
///
/// # Arguments
/// * `hwnd` - The window handle
///
/// # Returns
/// * `Ok(u32)` - The process ID
/// * `Err` - If the operation fails
pub fn get_window_pid(hwnd: HWND) -> Result<u32> {
    unsafe {
        let mut pid = 0u32;
        if GetWindowThreadProcessId(hwnd, Some(&mut pid)) == 0 {
            return Err(eyre!("Failed to get window PID"));
        }
        if pid == 0 {
            return Err(eyre!("Window has no PID"));
        }
        Ok(pid)
    }
}

/// Validates that window dimensions are reasonable for a game.
///
/// This prevents capturing the recorder's own small UI window
/// (which might be 600x840 or similar) or desktop-sized windows.
///
/// # Arguments
/// * `width` - Window width in pixels
/// * `height` - Window height in pixels
///
/// # Returns
/// * `Ok(())` if dimensions are reasonable
/// * `Err(ValidationError)` if dimensions are out of range
pub fn validate_window_dimensions(width: u32, height: u32) -> Result<()> {
    // Check minimum dimensions
    if width < MIN_GAME_WINDOW_WIDTH || height < MIN_GAME_WINDOW_HEIGHT {
        return Err(ValidationError::DimensionsTooSmall {
            width,
            height,
            min_width: MIN_GAME_WINDOW_WIDTH,
            min_height: MIN_GAME_WINDOW_HEIGHT,
        }
        .into());
    }

    // Check maximum dimensions
    if width > MAX_GAME_WINDOW_WIDTH || height > MAX_GAME_WINDOW_HEIGHT {
        return Err(ValidationError::DimensionsTooLarge {
            width,
            height,
            max_width: MAX_GAME_WINDOW_WIDTH,
            max_height: MAX_GAME_WINDOW_HEIGHT,
        }
        .into());
    }

    // Check minimum area (catches weird aspect ratios like 4000x1)
    let area = (width as u64) * (height as u64);
    let min_area = (MIN_GAME_WINDOW_WIDTH as u64) * (MIN_GAME_WINDOW_HEIGHT as u64);
    if area < min_area {
        return Err(ValidationError::AreaTooSmall { area, min_area }.into());
    }

    Ok(())
}

/// Comprehensive window capture target validation.
///
/// Validates both that the HWND belongs to the expected process
/// and that the window dimensions are reasonable for a game.
///
/// # Arguments
/// * `hwnd` - The window handle to validate
/// * `expected_pid` - The process ID that should own this window
/// * `width` - Window width in pixels
/// * `height` - Window height in pixels
///
/// # Returns
/// * `Ok(())` if the window is a valid capture target
/// * `Err` - With detailed error information
pub fn validate_capture_target(
    hwnd: HWND,
    expected_pid: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    // Validate PID first (fastest check)
    validate_window_pid(hwnd, expected_pid)?;

    // Validate dimensions
    validate_window_dimensions(width, height)?;

    tracing::debug!(
        "Window capture target validated: hwnd={:?}, pid={}, resolution={}x{}",
        hwnd,
        expected_pid,
        width,
        height
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_window_dimensions_rejects_small_windows() {
        // Recorder UI window size (from bug report)
        let result = validate_window_dimensions(600, 840);
        assert!(result.is_err());

        if let Err(e) = result {
            assert!(matches!(
                e.downcast_ref::<ValidationError>(),
                Some(ValidationError::DimensionsTooSmall { .. })
            ));
        }
    }

    #[test]
    fn test_validate_window_dimensions_accepts_720p() {
        assert!(validate_window_dimensions(1280, 720).is_ok());
    }

    #[test]
    fn test_validate_window_dimensions_accepts_1080p() {
        assert!(validate_window_dimensions(1920, 1080).is_ok());
    }

    #[test]
    fn test_validate_window_dimensions_rejects_huge_dimensions() {
        let result = validate_window_dimensions(10000, 10000);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_window_dimensions_rejects_thin_windows() {
        // 4000x1 should fail area check even though individual dimensions pass
        let result = validate_window_dimensions(4000, 1);
        assert!(result.is_err());
    }
}
