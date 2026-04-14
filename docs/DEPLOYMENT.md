# GameData Recorder — Deployment Guide

## Build Requirements (Windows)

1. **Rust toolchain**: `rustup default stable`
2. **cargo-obs-build**: `cargo install cargo-obs-build`
3. **OBS binaries**: `cargo obs-build build --out-dir target\x86_64-pc-windows-msvc\release`
4. **Build**: `cargo build --release`

## Build Steps

```powershell
# Clone
git clone https://github.com/howardleegeek/gamedata-recorder.git
cd gamedata-recorder

# Install OBS build tool
cargo install cargo-obs-build

# Build debug first (downloads OBS binaries)
cargo obs-build build --out-dir target\x86_64-pc-windows-msvc\debug
cargo build

# Build release
cargo obs-build build --out-dir target\x86_64-pc-windows-msvc\release
cargo build --release
```

## Create Installer

1. Install [Inno Setup](https://jrsoftware.org/isinfo.php)
2. Open `installer/gamedata-recorder.iss`
3. Click Compile
4. Output: `installer/output/GameDataRecorder-Setup-0.2.0.exe`

## Distribution

The installer:
- Does NOT require admin (per-user install)
- Adds to Windows startup (optional, checked by default)
- Auto-launches after install (minimized to tray)
- Supports English and Chinese Simplified

## Environment Variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `GAMEDATA_API_URL` | `https://api.gamedatalabs.com` | Backend API endpoint |
| `RUST_LOG` | `info` | Log level |

## Backend Setup

See [api-spec.md](api-spec.md) for the full API specification.

Minimum backend for MVP:
1. **Auth endpoint**: Return a JWT token (can be hardcoded for testing)
2. **S3 bucket**: For video/input uploads
3. **Upload init/complete**: Return presigned S3 URLs

## Monitoring

Logs are written to:
- `%APPDATA%\GameData Recorder\gamedata-recorder-debug.log`
- Rotated daily, 7 days retained
