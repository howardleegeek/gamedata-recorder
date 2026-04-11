# Experiment H6: Refactor Gamepad Capture Code

## Hypothesis
The xinput and wgi gamepad capture threads share significant duplication that can be extracted into a generic helper, reducing ~40 lines of similar code.

## Current State
Location: `gamepad_capture.rs` lines 140-213
- Two nearly identical thread spawn blocks
- Both use gilrs, iterate events, update metadata, filter duplicates
- Only differences: type aliases and the duplicate filter check

## Proposed Changes
Extract common gamepad thread logic into a generic function that takes:
- Gilrs type parameter
- GamepadId constructor
- Duplicate filter reference (optional)

## Prediction
- Lines reduced: ~30-40 lines
- Maintainability: Single point of change for gamepad logic
- No functional change: Behavior remains identical

## Validation
- [ ] Code compiles
- [ ] Both XInput and WGI controllers still work
- [ ] Duplicate filtering still works correctly
- [ ] No clippy warnings
