# Experiment H1: Reduce Code Duplication in kbm_capture.rs

## Hypothesis
By extracting mouse button handling into a helper function or macro, we can reduce ~70 lines of repetitive code and improve maintainability.

## Current State
Location: `kbm_capture.rs` lines 234-303
- 5 mouse buttons × 2 states (DOWN/UP) = 10 nearly identical code blocks
- Each block is ~7 lines of code
- Total: ~70 lines of duplication

## Proposed Change
Extract common button handling logic into a helper that takes:
- Button flag constant
- Virtual key code
- Press state

## Prediction
- Lines of code reduced: ~50-60 lines
- Maintainability improved: single point of change for button handling
- No functional change: behavior remains identical

## Validation
- [ ] Code compiles successfully
- [ ] All mouse buttons still work correctly
- [ ] No clippy warnings introduced
- [ ] Lines of code reduced
