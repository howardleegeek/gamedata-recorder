# Build script for ARM64 Windows (UTM) using local OBS Studio installation
# This script builds x86_64 executables and copies OBS dependencies from
# C:/Program Files/obs-studio/bin/64bit/ to avoid ARM64 DLL issues

$ErrorActionPreference = "Stop"

function Write-Status {
    param([string]$Message)
    Write-Host "[*] $Message" -ForegroundColor Green
}

function Write-Error-Custom {
    param([string]$Message)
    Write-Host "[ERROR] $Message" -ForegroundColor Red
}

function Write-Warning-Custom {
    param([string]$Message)
    Write-Host "[WARNING] $Message" -ForegroundColor Yellow
}

Write-Host "======================================" -ForegroundColor Cyan
Write-Host "Building for x86_64 on ARM64 Windows" -ForegroundColor Cyan
Write-Host "Using local OBS Studio dependencies" -ForegroundColor Cyan
Write-Host "======================================" -ForegroundColor Cyan

# Extract version
Write-Status "Extracting version from Cargo.toml..."
$CARGO_VERSION = Select-String -Path "Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' | ForEach-Object { $_.Matches[0].Groups[1].Value }
$VERSION = "v$CARGO_VERSION"
Write-Status "Building version: $VERSION"

# Build Rust application for x86_64
Write-Status "Building Rust application for x86_64..."
cargo build --release --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) {
    Write-Error-Custom "Rust build failed"
    exit 1
}

# Create distribution directory
Write-Status "Creating distribution directory..."
if (Test-Path dist) {
    Remove-Item -Path dist -Recurse -Force
}
New-Item -ItemType Directory -Force -Path dist | Out-Null

# Copy assets
Write-Status "Copying assets..."
Copy-Item -Path assets -Destination dist\assets -Recurse

# Copy Rust binary
Write-Status "Copying Rust binary..."
$RUST_BINARY = "target\x86_64-pc-windows-msvc\release\gamedata-recorder.exe"
if (Test-Path $RUST_BINARY) {
    Copy-Item -Path $RUST_BINARY -Destination "dist\gamedata-recorder.exe"
}
else {
    Write-Error-Custom "Rust binary not found at $RUST_BINARY"
    exit 1
}

# Copy OBS mux helper (from build if available)
Write-Status "Copying OBS FFmpeg mux helper..."
$MUX_HELPER = "target\x86_64-pc-windows-msvc\release\obs-ffmpeg-mux.exe"
if (Test-Path $MUX_HELPER) {
    Copy-Item -Path $MUX_HELPER -Destination "dist\obs-ffmpeg-mux.exe"
    Write-Status "OBS FFmpeg mux helper copied from build"
}
else {
    # Try to find in OBS installation
    $OBS_MUX = "C:\Program Files\obs-studio\bin\64bit\obs-ffmpeg-mux.exe"
    if (Test-Path $OBS_MUX) {
        Copy-Item -Path $OBS_MUX -Destination "dist\obs-ffmpeg-mux.exe"
        Write-Status "OBS FFmpeg mux helper copied from OBS installation"
    }
    else {
        Write-Warning-Custom "OBS FFmpeg mux helper not found - recording may not work!"
    }
}

# Copy OBS DLLs from local OBS Studio installation (x86_64 only!)
Write-Status "Copying OBS dependencies from local OBS Studio installation..."
$OBS_BIN_PATH = "C:\Program Files\obs-studio\bin\64bit"

if (-not (Test-Path $OBS_BIN_PATH)) {
    Write-Error-Custom "OBS Studio not found at $OBS_BIN_PATH"
    Write-Error-Custom "Please install OBS Studio for Windows (64-bit) from https://obsproject.com"
    exit 1
}

# Core OBS DLLs
$OBS_DLLS = @(
    "obs.dll",
    "avcodec-61.dll",
    "avdevice-61.dll",
    "avfilter-10.dll",
    "avformat-61.dll",
    "avutil-59.dll",
    "swresample-5.dll",
    "swscale-8.dll",
    "libx264-164.dll",
    "datachannel.dll",
    "libcurl.dll",
    "srt.dll",
    "zlib.dll",
    "w32-pthreads.dll",
    "librist.dll",
    "libobs-d3d11.dll",
    "libobs-opengl.dll",
    "libobs-winrt.dll"
)

foreach ($dll in $OBS_DLLS) {
    $source = Join-Path $OBS_BIN_PATH $dll
    if (Test-Path $source) {
        Copy-Item -Path $source -Destination "dist\$dll" -Force
        Write-Status "Copied: $dll"
    }
    else {
        Write-Warning-Custom "Not found: $dll"
    }
}

# Copy obs-plugins directory
Write-Status "Copying obs-plugins directory..."
$OBS_PLUGINS_SOURCE = "C:\Program Files\obs-studio\obs-plugins"
if (Test-Path $OBS_PLUGINS_SOURCE) {
    Copy-Item -Path $OBS_PLUGINS_SOURCE -Destination dist\obs-plugins -Recurse -Force
    Write-Status "Copied obs-plugins directory"
}
else {
    Write-Warning-Custom "obs-plugins directory not found"
}

# Copy data directory
Write-Status "Copying data directory..."
$OBS_DATA_SOURCE = "C:\Program Files\obs-studio\data"
if (Test-Path $OBS_DATA_SOURCE) {
    Copy-Item -Path $OBS_DATA_SOURCE -Destination dist\data -Recurse -Force
    Write-Status "Copied data directory"
}
else {
    Write-Warning-Custom "data directory not found"
}

# Copy additional resources
Write-Status "Copying additional resources..."
if (Test-Path README.md) {
    Copy-Item -Path README.md -Destination dist\README.md
}
if (Test-Path LICENSE) {
    Copy-Item -Path LICENSE -Destination dist\LICENSE
}

# Verify architecture match
Write-Status "Verifying architecture compatibility..."
$EXE_ARCH = (dumpbin /HEADERS "dist\gamedata-recorder.exe" 2>$null | Select-String "machine.*x64") -ne $null
$DLL_ARCH = (dumpbin /HEADERS "dist\obs.dll" 2>$null | Select-String "machine.*x64") -ne $null

if (-not $EXE_ARCH -or -not $DLL_ARCH) {
    Write-Warning-Custom "Could not verify architectures - dumpbin not available"
}
else {
    Write-Status "Architecture verification passed: x86_64"
}

# Create portable zip file
Write-Status "Creating portable zip file..."
$ZIP_FILE = "gamedata-recorder-${VERSION}-windows-x86_64.zip"
if (Test-Path $ZIP_FILE) {
    Remove-Item -Path $ZIP_FILE -Force
}

# Change to dist directory first, then compress everything (more reliable)
$currentDir = Get-Location
try {
    Set-Location "dist"
    Compress-Archive -Path ".\*" -DestinationPath "..\$ZIP_FILE" -Force
}
finally {
    Set-Location $currentDir
}
Write-Status "Portable zip file created: $ZIP_FILE"

Write-Status "Build completed successfully!"
Write-Host "======================================" -ForegroundColor Cyan
Write-Host "Output files:" -ForegroundColor Cyan
Write-Host "  Portable: $ZIP_FILE" -ForegroundColor Cyan
Write-Host "  Folder:  dist\" -ForegroundColor Cyan
Write-Host "======================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "NOTE: This build uses OBS dependencies from your local" -ForegroundColor Yellow
Write-Host "OBS Studio installation at C:\Program Files\obs-studio\" -ForegroundColor Yellow
