# Build script for ARM64 Windows environments building for x86_64
# Use this when building on macOS UTM or other ARM64 Windows setups

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

# Copy OBS mux helper (CRITICAL)
Write-Status "Copying OBS FFmpeg mux helper..."
$MUX_HELPER = "target\x86_64-pc-windows-msvc\release\obs-ffmpeg-mux.exe"
if (Test-Path $MUX_HELPER) {
    Copy-Item -Path $MUX_HELPER -Destination "dist\obs-ffmpeg-mux.exe"
    Write-Status "OBS FFmpeg mux helper copied successfully"
}
else {
    Write-Warning-Custom "OBS FFmpeg mux helper not found - recording may not work!"
}

# Copy OBS DLLs and dependencies
Write-Status "Copying OBS dependencies..."
$OBS_FILES = @(
    "target\x86_64-pc-windows-msvc\release\*.dll",
    "target\x86_64-pc-windows-msvc\release\obs-plugins",
    "target\x86_64-pc-windows-msvc\release\data"
)

foreach ($pattern in $OBS_FILES) {
    if (Test-Path $pattern) {
        Copy-Item -Path $pattern -Destination dist\ -Recurse -Force
        Write-Status "Copied: $pattern"
    }
    else {
        Write-Warning-Custom "Not found: $pattern"
    }
}

# Copy additional resources
Write-Status "Copying additional resources..."
if (Test-Path README.md) {
    Copy-Item -Path README.md -Destination dist\README.md
}
if (Test-Path LICENSE) {
    Copy-Item -Path LICENSE -Destination dist\LICENSE
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
Write-Host "NOTE: NSIS installer creation is skipped on ARM64." -ForegroundColor Yellow
Write-Host "The zip file contains everything needed for x86_64 Windows users." -ForegroundColor Yellow
