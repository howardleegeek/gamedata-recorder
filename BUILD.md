# Build Instructions

## Quick Start (Windows x86_64)

```powershell
# Run the main build script
.\build-resources\scripts\build.ps1
```

This will create:
- `dist\` - Portable distribution folder
- `dist\GameData-Recorder-Setup-*.exe` - NSIS installer
- `dist\gamedata-recorder-*-windows-x86_64.zip` - Portable zip file

## What the Build Script Does

1. **Builds Rust binary** (`cargo build --release`)
2. **Downloads & builds OBS dependencies** (`cargo obs-build`)
3. **Copies all files to `dist\`**
4. **Creates NSIS installer** (requires NSIS/makensis)
5. **Creates portable zip file**

## Manual Build (If Script Fails)

If `build.ps1` fails, you can manually build:

```powershell
# 1. Build Rust application
cargo build --release --target x86_64-pc-windows-msvc

# 2. Build OBS dependencies
cargo obs-build build --out-dir dist\

# 3. Copy Rust binary
Copy-Item target\x86_64-pc-windows-msvc\release\gamedata-recorder.exe dist\

# 4. Copy OBS mux helper (CRITICAL - required for recording)
Copy-Item target\x86_64-pc-windows-msvc\release\obs-ffmpeg-mux.exe dist\

# 5. Copy OBS DLLs and data
Copy-Item target\x86_64-pc-windows-msvc\release\*.dll dist\
Copy-Item -Recurse target\x86_64-pc-windows-msvc\release\obs-plugins dist\
Copy-Item -Recurse target\x86_64-pc-windows-msvc\release\data dist\

# 6. Copy assets
Copy-Item -Recurse assets dist\

# 7. (Optional) Create zip
Compress-Archive -Path dist\* -DestinationPath gamedata-recorder.zip
```

## Cross-Compilation (ARM64 to Windows/x86_64)

**Building on ARM64?** You CAN still create working releases for x86_64 Windows users!

### Quick Answer: Yes, you can build!

If you're on macOS UTM or another ARM64 Windows environment, use:

```powershell
.\build-resources\scripts\build-arm64.ps1
```

This script:
- Cross-compiles Rust to x86_64
- Copies already-built OBS dependencies
- Creates a zip file that works for x86_64 Windows users
- Skips the broken `cargo-obs-build` download step

### Option 1: ARM64 Build Script (Recommended for ARM64)
```powershell
# Use this if you're on ARM64 Windows (UTM, Parallels, etc.)
.\build-resources\scripts\build-arm64.ps1
```

**Output:** `gamedata-recorder-v*-windows-x86_64.zip` - Ready for x86_64 Windows users!

### Option 2: GitHub Actions (Fully Automated)
Push your code and let GitHub Actions build it on proper x86_64 runners:
```bash
git push origin main
# Or create a release tag
git tag v1.6.1
git push origin v1.6.1
```

### Option 3: Native x86_64 Windows
Build on a physical x86_64 Windows PC or standard x86_64 VM:
```powershell
.\build-resources\scripts\build.ps1
```

### Why does ARM64 need special handling?

The standard build script uses `cargo-obs-build` which:
- Detects ARM64 → Downloads ARM64 OBS binaries
- But we need x86_64 binaries for Windows users

The ARM64 script works around this by:
- Cross-compiling Rust to x86_64 (works great)
- Using pre-built OBS files or skipping the download step
- Creating a zip that's fully compatible with x86_64 Windows

## Common Issues

### "obs-ffmpeg-mux.exe not found"
**Symptom:** Recording fails with "Unable to start the recording helper process"

**Fix:** The build script must copy this file from `target\*\release\obs-ffmpeg-mux.exe` to `dist\`

### "Checksums do not match" / "ARM64" issues
**Symptom:** `cargo obs-build` tries to download ARM64 OBS on Apple Silicon

**Cause:** You're building on ARM64 (macOS UTM) but targeting x86_64 Windows

**Fix:** Build on native x86_64 Windows or use GitHub Actions

### PowerShell Execution Policy
**Symptom:** "cannot be loaded because running scripts is disabled"

**Fix:**
```powershell
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
```

## Requirements

- **Windows 10/11** (for native builds)
- **Rust toolchain** (stable)
- **NSIS** (for installer creation)
- **cargo-obs-build** (automatically installed by build script)
- **Visual C++ Redistributable** (included in build)

## Output Files

After successful build:

| File | Description |
|------|-------------|
| `dist/gamedata-recorder.exe` | Main application |
| `dist/obs-ffmpeg-mux.exe` | OBS recording helper (REQUIRED) |
| `dist/obs-plugins/` | OBS plugins |
| `dist/data/` | OBS data files |
| `dist/*.dll` | Required DLLs |
| `dist/GameData-Recorder-Setup-*.exe` | NSIS installer |
| `gamedata-recorder-*.zip` | Portable distribution |

## Verification

Test your build:

```powershell
# Run the application
.\dist\gamedata-recorder.exe

# Check that obs-ffmpeg-mux.exe exists
Test-Path dist\obs-ffmpeg-mux.exe  # Should return True
```
