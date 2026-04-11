# Research Log: gamedata-recorder Optimization

## 2026-04-11: Bootstrap

**Goal**: Systematically optimize gamedata-recorder Rust codebase

**Initial Assessment**:
- Project: Game data recorder with input capture, OBS integration, and UI
- Stack: Rust (egui + wgpu), Python (FastAPI backend)
- Key crates: input-capture, game-process, constants
- Platform: Windows (Raw Input API, Windows Gamepad APIs)

**Code Structure Analysis**:
1. `input-capture` crate - Core input handling
   - kbm_capture.rs: Windows Raw Input for keyboard/mouse (363 lines)
   - gamepad_capture.rs: Dual gamepad backend (xinput + wgi) (329 lines)
   - timestamp.rs: QPC high-precision timer (79 lines)
   - vkey_names.rs: Key code mapping (113 lines)
   - input_logger.rs: CLI tool (91 lines)

2. Main application - OBS + UI integration
   - main.rs: Application entry (150 lines)
   - UI modules, recording modules, upload modules

**Initial Observations**:
- Code duplication in mouse button handling (kbm_capture.rs lines 234-303)
- Macro usage for gamepad mapping is good but could be cleaner
- Some unwrap() usage that could be more robust
- Windows API calls could benefit from better error context
- Documentation is minimal in some modules

**Next**: Run static analysis to get baseline metrics
