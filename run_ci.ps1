# run_ci.ps1 — gamedata-recorder CI pipeline
#
# Steps:
#   1. Build gamedata-recorder
#   2. Build test_game
#   3. Launch test_game (simulates a real GPU-rendered game)
#   4. Launch gamedata-recorder and wait for it to start recording
#   5. Let it record for a few seconds
#   6. Stop recording and close processes
#   7. Check the output video (brightness, fps, duration)
#   8. If all checks pass, git commit + push
#
# Usage:
#   .\run_ci.ps1
#   .\run_ci.ps1 -SkipBuild        # skip cargo build steps (faster iteration)
#   .\run_ci.ps1 -SkipCommit       # check only, don't commit
#   .\run_ci.ps1 -RecordSeconds 8  # record for 8 seconds instead of default 5

param(
    [switch]$SkipBuild   = $false,
    [switch]$SkipCommit  = $false,
    [int]$RecordSeconds  = 5
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ─── Config ───────────────────────────────────────────────────────────────────

$RepoRoot          = $PSScriptRoot
$RecorderExe       = "$RepoRoot\target\release\gamedata-recorder.exe"
$TestGameExe       = "$RepoRoot\test_game\target\release\test_game.exe"
$VideoOutputDir    = "$RepoRoot\ci_output"
$CheckVideoScript  = "$RepoRoot\check_video.py"

$TestGameTitle     = "D3D Test Game"      # must match window title in Rust code
$TestGameProcess   = "test_game"

$MinBrightness     = 10.0
$MinFps            = 27.0
$MinDuration       = 3.0

# ─── Helpers ──────────────────────────────────────────────────────────────────

function Write-Step([string]$msg) {
    Write-Host "`n=== $msg ===" -ForegroundColor Cyan
}

function Write-OK([string]$msg) {
    Write-Host "  ✓ $msg" -ForegroundColor Green
}

function Write-Fail([string]$msg) {
    Write-Host "  ✗ $msg" -ForegroundColor Red
}

function Wait-ForProcess([string]$name, [int]$timeoutSec = 15) {
    $elapsed = 0
    while ($elapsed -lt $timeoutSec) {
        if (Get-Process -Name $name -ErrorAction SilentlyContinue) { return $true }
        Start-Sleep -Milliseconds 500
        $elapsed++
    }
    return $false
}

function Wait-ForWindow([string]$title, [int]$timeoutSec = 20) {
    Add-Type -AssemblyName System.Windows.Forms
    $elapsed = 0
    while ($elapsed -lt $timeoutSec) {
        $found = [System.Windows.Forms.Screen]::AllScreens | ForEach-Object { $_.DeviceName }
        # Use FindWindow via P/Invoke to check for window title
        $hwnd = [Win32.NativeMethods]::FindWindow($null, $title)
        if ($hwnd -ne [IntPtr]::Zero) { return $true }
        Start-Sleep -Milliseconds 500
        $elapsed++
    }
    return $false
}

function Get-LatestVideo([string]$dir) {
    return Get-ChildItem -Path $dir -Filter "*.mp4" -ErrorAction SilentlyContinue |
           Sort-Object LastWriteTime -Descending |
           Select-Object -First 1
}

function Stop-ProcessSafe([string]$name) {
    $p = Get-Process -Name $name -ErrorAction SilentlyContinue
    if ($p) {
        $p | Stop-Process -Force
        Write-OK "Stopped $name"
    }
}

# P/Invoke for FindWindow (to detect when game window appears)
Add-Type @"
using System;
using System.Runtime.InteropServices;
namespace Win32 {
    public class NativeMethods {
        [DllImport("user32.dll", SetLastError = true)]
        public static extern IntPtr FindWindow(string lpClassName, string lpWindowName);
    }
}
"@

# ─── Step 1: Build recorder ───────────────────────────────────────────────────

Write-Step "Build gamedata-recorder"

if ($SkipBuild) {
    Write-Host "  Skipped (--SkipBuild)" -ForegroundColor Yellow
} else {
    Push-Location $RepoRoot
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        Write-Fail "cargo build failed for gamedata-recorder"
        exit 1
    }
    Pop-Location
    Write-OK "gamedata-recorder built"
}

if (-not (Test-Path $RecorderExe)) {
    Write-Fail "Recorder binary not found: $RecorderExe"
    exit 1
}

# ─── Step 2: Build test_game ──────────────────────────────────────────────

Write-Step "Build test_game"

if ($SkipBuild) {
    Write-Host "  Skipped (--SkipBuild)" -ForegroundColor Yellow
} else {
    Push-Location "$RepoRoot\test_game"
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        Write-Fail "cargo build failed for test_game"
        exit 1
    }
    Pop-Location
    Write-OK "test_game built"
}

if (-not (Test-Path $TestGameExe)) {
    Write-Fail "Test game binary not found: $TestGameExe"
    exit 1
}

# ─── Step 3: Prepare output dir ───────────────────────────────────────────────

Write-Step "Prepare output directory"

New-Item -ItemType Directory -Force -Path $VideoOutputDir | Out-Null
# Clear old CI videos so we can reliably pick the latest one
Get-ChildItem -Path $VideoOutputDir -Filter "*.mp4" | Remove-Item -Force
Write-OK "Output dir ready: $VideoOutputDir"

# ─── Step 4: Launch test game ─────────────────────────────────────────────────

Write-Step "Launch test_game"

$gameProc = Start-Process -FilePath $TestGameExe -PassThru

# Wait for the window to appear (proof that D3D init succeeded)
$windowReady = Wait-ForWindow -Title $TestGameTitle -TimeoutSec 15
if (-not $windowReady) {
    Write-Fail "test_game window never appeared (D3D init may have failed)"
    Stop-ProcessSafe $TestGameProcess
    exit 1
}
Write-OK "test_game window visible"

# ─── Step 5: Launch recorder ──────────────────────────────────────────────────

Write-Step "Launch gamedata-recorder"

# Tell recorder where to save output
$env:GAMEDATA_OUTPUT_DIR = $VideoOutputDir

$recorderProc = Start-Process -FilePath $RecorderExe -PassThru

# Wait for recorder process to be alive
$recorderReady = Wait-ForProcess -Name "gamedata-recorder" -TimeoutSec 15
if (-not $recorderReady) {
    Write-Fail "gamedata-recorder process never started"
    Stop-ProcessSafe $TestGameProcess
    exit 1
}
Write-OK "gamedata-recorder started"

# Give recorder time to detect the game and begin capturing
Write-Host "  Waiting for capture to begin..." -ForegroundColor Gray
Start-Sleep -Seconds 3

# ─── Step 6: Record for N seconds ─────────────────────────────────────────────

Write-Step "Recording for $RecordSeconds seconds"

Start-Sleep -Seconds $RecordSeconds
Write-OK "Recording window elapsed"

# ─── Step 7: Stop everything ──────────────────────────────────────────────────

Write-Step "Stop processes"

Stop-ProcessSafe "gamedata-recorder"
Start-Sleep -Seconds 2   # let recorder finalize the video file
Stop-ProcessSafe $TestGameProcess

# ─── Step 8: Find output video ────────────────────────────────────────────────

Write-Step "Locate output video"

$video = Get-LatestVideo -Dir $VideoOutputDir
if (-not $video) {
    Write-Fail "No video file found in $VideoOutputDir"
    exit 1
}
Write-OK "Found video: $($video.FullName)"

# ─── Step 9: Check video ──────────────────────────────────────────────────────

Write-Step "Validate video"

python $CheckVideoScript $video.FullName `
    --min-brightness $MinBrightness `
    --min-fps $MinFps `
    --min-duration $MinDuration

if ($LASTEXITCODE -ne 0) {
    Write-Fail "Video validation failed — blocking commit"
    exit 1
}
Write-OK "Video validation passed"


Write-Host "`n=== CI PASSED ===" -ForegroundColor Green