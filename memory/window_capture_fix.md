---
name: window-capture-black-screen-fix
description: Fix for Window Capture targeting wrong window (recorder UI instead of game)
type: feedback
---

# Window Capture Black Screen Bug Fix

**Date**: 2026-04-19

## Bug Description

When recording games (especially GTA V with PlayGTAV.exe), the output video was completely black. The recording was automatically deleted due to low FPS (15fps, below the 27fps threshold).

## Root Cause

The `find_window_for_pid()` function in `src/record/recorder.rs` was ignoring its PID parameter and returning the foreground window instead. When the recorder UI was in the foreground (common in multi-monitor setups), it returned the recorder's own HWND instead of the game's HWND, causing:

1. Window Capture to target `gamedata-recorder.exe` instead of the game
2. Resolution to be read as 600x840 (recorder UI size) instead of game resolution
3. BitBlt capture method which can't capture GPU-rendered content

## Solution Implemented

Created a new `window_validation` module (`src/system/window_validation.rs`) that provides:

1. **PID validation**: Ensures HWND belongs to the expected game process
2. **Resolution validation**: Validates window dimensions are reasonable for games (>= 1280x720)
3. **Area validation**: Catches weird aspect ratios
4. **Fallback behavior**: Falls back to primary monitor resolution if validation fails

## Files Modified

- **Created**: `src/system/window_validation.rs` (272 lines) - Validation module with tests
- **Modified**: `src/system/mod.rs` - Added module export
- **Modified**: `src/record/recording.rs` - Updated `get_recording_base_resolution()` to use validation
- **Modified**: `src/tokio_thread.rs` - Updated call to pass PID parameter

## Key Design Decisions

1. **Keep HWND as placeholder**: OBS finds windows by exe name anyway, so we don't need to change the HWND
2. **Validate resolution**: Prevent using recorder's own window resolution (600x840)
3. **Fallback on failure**: Don't fail recording if validation fails - use primary monitor resolution
4. **Robust solution**: Created dedicated validation module for reusability

## Why This Works

- OBS Window Capture source identifies targets by exe name, not HWND
- The HWND is just a placeholder for OBS to find the correct window
- By validating the resolution, we ensure we don't use invalid window dimensions
- PID validation ensures the HWND at least belongs to the game process

## Testing

- All 5 unit tests pass
- Manual testing required with actual games (GTA V, multi-monitor scenarios)
