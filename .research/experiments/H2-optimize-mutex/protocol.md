# Experiment H2: Optimize Mutex Usage Patterns

## Hypothesis
Replacing `Mutex` with `RwLock` for read-heavy operations and using `expect()` with descriptive messages instead of `unwrap()` will improve code robustness and debuggability.

## Current State
Locations with `unwrap()` on mutex locks:
1. `lib.rs:122` - `self.active_keys.lock().unwrap()`
2. `lib.rs:123` - `self.active_gamepad.lock().unwrap()`
3. `lib.rs:132` - `self.gamepads.read().unwrap()`
4. `kbm_capture.rs:171` - `self.active_keys.lock().unwrap()`

## Issues
1. `unwrap()` provides no context on panic
2. `Mutex` is used even for read-only operations
3. Poisoned lock handling is implicit

## Proposed Changes
1. Use `RwLock` for `gamepads` (already done) and consider for `active_keys`
2. Replace `unwrap()` with `expect()` with descriptive messages
3. Consider `parking_lot` for better performance (optional)

## Prediction
- Better error messages when locks fail
- Potential performance improvement for read-heavy operations
- More robust poisoned lock handling

## Validation
- [ ] Code compiles
- [ ] Error messages are descriptive
- [ ] No functional change
