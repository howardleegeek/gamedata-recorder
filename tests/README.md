# GameData Recorder Tests

This directory contains automated tests for the GameData Recorder.

## Quick Start

### Basic Recording Test

Tests that the recorder can capture video of a running application:

```powershell
# Using PowerShell
.\tests\test_basic_recording.ps1

# With custom game and duration
.\tests\test_basic_recording.ps1 -GameExe "solitaire.exe" -RecordingDuration 20

# Using batch wrapper (simpler)
.\tests\test_basic.bat solitaire.exe 30
```

### UI Click Test (AutoHotkey)

Tests UI elements by clicking through the interface (requires AutoHotkey):

```powershell
# Install AutoHotkey first, then:
.\tests\test_ui_clicks.ahk
```

## Test Files

### `test_basic_recording.ps1`
PowerShell script that tests basic recording functionality:
- Starts the recorder
- Detects/starts a game process
- Waits for recording
- Validates output files exist

**Parameters:**
- `-RecorderPath`: Path to gamedata-recorder.exe (auto-detects from ./target/release/)
- `-GameExe`: Game to test with (default: notepad.exe)
- `-RecordingDuration`: How long to record in seconds (default: 10)
- `-OutputPath`: Recording output directory (default: ./data_dump/games/)

**Returns:** Exit code 0 on success, 1 on failure

### `test_basic.bat`
Simple batch wrapper for the PowerShell test. Usage:
```
test_basic.bat [game_exe] [duration_seconds]
```

### `test_ui_clicks.ahk`
AutoHotkey script that clicks through UI elements to verify they work.
- Tests all UI buttons and dropdowns
- Requires 1920x1080 resolution with 125% DPI scaling
- Requires the recorder window to be visible

## Manual Testing

For comprehensive manual testing, use these test games:

### Safe Test Games (no anti-cheat)
- **Notepad**: Simple window capture test
- **Paint**: Basic graphics application
- **Windows Solitaire**: Built-in casual game
- **Minesweeper**: Built-in casual game

### Challenging Test Games
- **GTA V**: Anti-cheat, fullscreen exclusive
- **Valorant/COD**: Kernel anti-cheat (won't work with game capture)
- **Any Epic/Steam game**: Tests launcher detection

## Test Scenarios

### Scenario 1: Basic Recording (Quick Test)
```
1. Run: .\tests\test_basic.bat notepad.exe 10
2. Expected: Recording created in data_dump/games/
3. Check: video_metadata.json exists
4. Check: FPS > 27 (validation requirement)
```

### Scenario 2: Multi-Monitor Setup
```
1. Move game to second monitor
2. Run recorder
3. Expected: Captures correct monitor (not primary)
```

### Scenario 3: Fullscreen Game
```
1. Start game in fullscreen mode
2. Run recorder
3. Expected: Screen capture captures the display
4. Check: Resolution matches game resolution
```

### Scenario 4: Game with Anti-Cheat
```
1. Start GTA V or similar
2. Run recorder with default settings (screen capture)
3. Expected: Recording works (no hook injection needed)
4. Game capture mode: Would fail due to anti-cheat
```

## Adding New Tests

To add a new test scenario:

1. Create a new PowerShell script: `test_your_feature.ps1`
2. Use `test_basic_recording.ps1` as a template
3. Follow the same structure:
   - Log messages with timestamps
   - Return exit code 0 for success, 1 for failure
   - Clean up processes on error
4. Document in this README

## CI/CD Integration

These tests can be integrated into CI/CD pipelines:

```yaml
# Example GitHub Actions workflow
- name: Run basic recording test
  shell: pwsh
  run: |
    cargo build --release
    ./tests/test_basic_recording.ps1 -GameExe "notepad.exe" -RecordingDuration 10
```

## Troubleshooting

### Test fails with "recorder not found"
- Build the project first: `cargo build --release`
- Or specify `-RecorderPath` parameter

### Test fails with "no recording files found"
- Check if recorder is actually starting (look for gamedata-recorder.exe in Task Manager)
- Check logs in the recorder window or `%LOCALAPPDATA%\vg-control\logs\`
- Try increasing `-RecordingDuration` to give it more time

### Game doesn't start
- Make sure the game executable is in your PATH
- Or use full path: `-GameExe "C:\Games\YourGame\game.exe"`
- Some games may require administrator privileges

### UI click test doesn't work
- Requires AutoHotkey v2.0+
- Must match exact resolution (1920x1080) and DPI (125%)
- Recorder window must be visible and not minimized
