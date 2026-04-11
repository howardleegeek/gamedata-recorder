# Experiment H3: Improve Error Handling with Custom Error Types

## Hypothesis
Replacing `unwrap()` and `expect()` with proper error handling using `thiserror` will make the library more robust and user-friendly.

## Current Issues Found

### gamepad_capture.rs
- Line 63, 66: `unwrap()` on parse - will panic on invalid input
- Line 161, 210: `expect()` on Gilrs::new() - will panic on init failure
- Lines 170, 183, 193, 219, 228, 245: `expect()` on locks - acceptable for poisoned locks

### kbm_capture.rs
- Line 85, 87: `expect()` on Windows API in Drop - could panic during cleanup
- Line 158, 277: `expect()` on try_into() - will panic on size overflow (extremely unlikely)

### lib.rs
- Lines 169, 177: `expect()` in thread spawn - will panic on init failure

## Proposed Changes

### High Priority (User-Facing)
1. **GamepadId parsing** - Return Result instead of panic
2. **Gilrs initialization** - Return Result with context
3. **InputCapture::new()** - Already returns Result, improve error context

### Medium Priority (Internal)
4. **Drop implementation** - Log errors instead of panic
5. **Size conversions** - Keep expect() but with better messages (these are truly invariant violations)

## Implementation Plan

1. Add `thiserror` dependency for ergonomic error types
2. Create `InputCaptureError` enum covering:
   - GamepadInitError
   - ParseError
   - WindowsApiError
3. Update `GamepadId::from_str` to return proper error
4. Update `initialize_thread` to propagate errors
5. Update `Drop` implementations to log instead of panic

## Prediction
- API becomes more robust
- Users get descriptive errors instead of panics
- Library becomes suitable for production use

## Validation
- [ ] GamepadId parsing returns Result
- [ ] Gilrs init errors are descriptive
- [ ] Drop implementations don't panic
- [ ] All errors implement std::error::Error
