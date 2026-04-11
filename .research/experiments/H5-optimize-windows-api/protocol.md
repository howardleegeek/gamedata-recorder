# Experiment H5: Optimize Windows API Call Patterns

## Hypothesis
Encapsulating Windows API calls in safer abstractions and reducing redundant operations will improve code safety and potentially performance.

## Current Issues

### 1. Redundant size_of() calls
Location: kbm_capture.rs
- `size_of::<RAWINPUTHEADER>().try_into().expect(...)` called on every WM_INPUT
- This is a constant that could be computed once

### 2. Unsafe blocks scattered throughout
- Raw pointer operations in parse_wm_input
- Window procedure is unsafe but could be better encapsulated

### 3. Potential performance improvements
- GetRawInputData buffer size could be cached
- Repeated conversions that could be optimized

## Proposed Changes

### 1. Cache constant conversions
```rust
// Before: computed on every event
let header_size = size_of::<RAWINPUTHEADER>()
    .try_into()
    .expect("...");

// After: computed once at startup
const RAWINPUTHEADER_SIZE: u32 = size_of::<RAWINPUTHEADER>() as u32;
```

### 2. Better encapsulation of unsafe operations
- Extract raw input parsing into a safe wrapper
- Use safer abstractions where possible

### 3. Optimize hot paths
- Reduce allocations in event processing
- Cache frequently accessed values

## Prediction
- Safety: Better encapsulation of unsafe code
- Performance: Minor improvements in hot paths
- Maintainability: Clearer separation of safe/unsafe code

## Validation
- [ ] Constant conversions cached
- [ ] Unsafe code better encapsulated
- [ ] No functional changes
- [ ] Code compiles successfully
