# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

GameData Recorder is a Windows desktop application that records gameplay (video + input logs) for AI world model training. The app automatically detects supported games and records them; users earn money for their uploaded recordings.

**Architecture**: Rust desktop app (egui UI) + Python FastAPI backend + PostgreSQL database.

## Development Commands

### Building

```powershell
# Quick build (Windows)
.\build-resources\scripts\build.ps1

# Manual build
cargo build --release

# Install OBS dependencies (first time or when OBS version changes)
cargo install cargo-obs-build
cargo obs-build build --out-dir target\x86_64-pc-windows-msvc\release
```

**ARM64 builds**: Use `.\build-resources\scripts\build-arm64.ps1` when cross-compiling from ARM64.

### Running

```powershell
cargo run
```

### Code Quality

```powershell
cargo fmt
cargo clippy
```

### Backend (Python)

```bash
cd backend
python -m venv venv
source venv/bin/activate  # or venv\Scripts\activate on Windows
pip install -r requirements.txt

# Database setup
chmod +x setup_database.sh
./setup_database.sh development

# Run backend
uvicorn main:app --reload
```

### Version Bumping

```bash
cargo run -p bump-version -- major    # 1.0.0 -> 2.0.0
cargo run -p bump-version -- minor    # 1.0.0 -> 1.1.0
cargo run -p bump-version -- patch    # 1.0.0 -> 1.0.1
cargo run -p bump-version -- 1.2.3    # custom version
```

## Architecture

### Two-Thread Architecture

The application splits execution across two threads:

1. **Main thread**: Runs egui UI, handles user input, renders UI
2. **Tokio thread**: Runs async runtime, handles recording, uploads, I/O

Communication happens via:
- `async_request_tx` (mpsc): UI → Tokio (requests)
- `ui_update_tx` (mpsc): Tokio → UI (responses, forces repaint)
- `ui_update_unreliable_tx` (broadcast): Tokio → UI (progress updates, may drop)

### Workspace Structure

```
crates/
├── input-capture/   # Keyboard/mouse/gamepad input capture library
├── game-process/    # Game process detection and window management
└── constants/       # Supported games list, constants, file names

src/
├── main.rs          # Entry point: sets up logging, spawns tokio thread
├── app_state.rs     # Shared state (RecordingStatus, config, channels)
├── tokio_thread.rs  # Main async runtime loop
├── record/          # Recording logic
│   ├── recorder.rs              # Trait for video recorders
│   ├── obs_embedded_recorder.rs # OBS embedded in process
│   ├── obs_socket_recorder.rs   # OBS via socket connection
│   ├── local_recording.rs       # Local recording metadata
│   ├── recording.rs             # Recording state machine
│   └── input_recorder.rs        # Input event logging
├── upload/          # Upload logic (tar creation, API calls)
├── ui/              # egui UI components
│   ├── overlay.rs   # In-game overlay
│   └── views/       # Main views
├── config.rs        # Configuration loading/persistence
└── output_types.rs  # Event types for input logging

backend/             # FastAPI server
├── main.py          # API endpoints
├── models.py        # SQLAlchemy models
└── alembic/         # Database migrations
```

### Key Types

- `AppState`: Shared state container (RwLock for thread safety)
- `RecordingStatus`: Read-only reflection of recording state for UI
- `AsyncRequest`: Requests from UI to tokio thread
- `UiUpdate`: Responses from tokio to UI (forces repaint)
- `VideoRecorder`: Trait implemented by OBS recorders
- `LocalRecording`: Represents a completed recording on disk

### Input Capture

The `input-capture` crate captures:
- Keyboard events (via Windows hooks)
- Mouse events (via Windows hooks)
- Gamepad events (via XInput)

Input events are timestamped and written to CSV lines during recording. The `timestamp` module provides microsecond-precision timing.

### Recording Flow

1. Game detected → foregrounded → checks `unsupported_games.json`
2. User starts recording (hotkey or UI button)
3. `Recording::start()` initializes video recorder and input writer
4. Video recorder writes H.265 video via OBS
5. Input writer logs events to CSV
6. User stops recording → metadata written, recording validated
7. Upload happens asynchronously (tar → upload to API)

## Important Constraints

### Data Structure Changes

**Before modifying output formats** (CSVs, metadata), check with the data team. The code must maintain backwards compatibility with old recordings.

### Event Types

**Never remove event types** from `output_types.rs`. Mark deprecated instead — old recordings may contain them.

### OBS Dependencies

The build requires `obs-ffmpeg-mux.exe` to be copied to `dist/`. Missing this file causes "Unable to start the recording helper process" errors.

### Supported Games

Games are defined in `crates/constants/src/supported_games.json`. Update via:
```bash
cargo run --p update-games --release
```

### Unsupported Games

`unsupported_games.json` blocks recording of games we have enough data for. Update via:
```bash
python build-resources/scripts/update_unsupported_games.py path/to/games.csv
```

## Platform Constraints

- **Windows-only**: Uses Windows API (Win32) for window management, input capture
- **OBS dependency**: Recording requires OBS Studio libraries (libobs-wrapper)
- **GPU encoding**: Requires H.265-capable GPU (NVENC, AMD VCE, Intel QSV)

## Backend API

FastAPI server handles:
- User authentication (email, Google, Discord)
- Upload endpoints (tar files, metadata)
- User statistics and payout management

Database uses SQLAlchemy 2.0 async with PostgreSQL. Migrations via Alembic.

## Testing

Manual testing via `tests/test_ui_clicks.ahk` (AutoHotkey). Shell scripts exist for various test rounds but require manual execution. No automated Rust tests currently exist.
