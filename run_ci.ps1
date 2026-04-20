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
# .cargo/config.toml pins target = "x86_64-pc-windows-msvc", so the
# recorder binary lives under target\x86_64-pc-windows-msvc\release\
# — NOT target\release\ as a naive cargo setup would produce.
# These are evaluated as *candidates*; the actual selection happens
# AFTER cargo build, via Resolve-RecorderExe below.
$RecorderExePinned = "$RepoRoot\target\x86_64-pc-windows-msvc\release\gamedata-recorder.exe"
$RecorderExeShort  = "$RepoRoot\target\release\gamedata-recorder.exe"
$RecorderExe       = $RecorderExePinned  # default; re-resolved post-build

# test_game is excluded from the workspace (a01d5b2) BUT cargo still
# walks up for `.cargo/config.toml` which pins the MSVC target for the
# whole tree — so the test_game binary also lands under the pinned dir,
# not the default one. Try both.
$TestGameExePinned = "$RepoRoot\test_game\target\x86_64-pc-windows-msvc\release\test_game.exe"
$TestGameExeShort  = "$RepoRoot\test_game\target\release\test_game.exe"
$TestGameExe       = $TestGameExePinned  # default; re-resolved post-build

function Resolve-RecorderExe {
    # `dist` zip layout: the exe is a sibling of this script.
    $sibling = "$script:RepoRoot\gamedata-recorder.exe"
    if (Test-Path $sibling) { $script:RecorderExe = $sibling; return $true }
    if (Test-Path $script:RecorderExePinned) {
        $script:RecorderExe = $script:RecorderExePinned
        return $true
    }
    if (Test-Path $script:RecorderExeShort) {
        $script:RecorderExe = $script:RecorderExeShort
        return $true
    }
    return $false
}

function Resolve-TestGameExe {
    # `dist` zip layout: the exe is a sibling of this script.
    $sibling = "$script:RepoRoot\test_game.exe"
    if (Test-Path $sibling) { $script:TestGameExe = $sibling; return $true }
    if (Test-Path $script:TestGameExePinned) {
        $script:TestGameExe = $script:TestGameExePinned
        return $true
    }
    if (Test-Path $script:TestGameExeShort) {
        $script:TestGameExe = $script:TestGameExeShort
        return $true
    }
    return $false
}
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

function Wait-ForProcessReady([string]$name, [int]$timeoutSec = 15) {
    $elapsed = 0
    while ($elapsed -lt $timeoutSec) {
        $proc = Get-Process -Name $name -ErrorAction SilentlyContinue
        if ($proc -and $proc.MainWindowHandle -ne [IntPtr]::Zero) {
            Write-Host "    Found $name with window" -ForegroundColor Green
            return $true
        }
        if ($proc) {
            Write-Host "    $name running, waiting for window... (elapsed: ${elapsed}s)" -ForegroundColor Gray
        }
        Start-Sleep -Milliseconds 500
        $elapsed++
    }
    return $false
}

function Get-LatestVideo([string]$dir) {
    # Recordings land at $dir\session_YYYYMMDD_HHMMSS_<suffix>\*.mp4 — use
    # -Recurse so we find them inside the session subfolders, not just at
    # the top level.
    return Get-ChildItem -Path $dir -Filter "*.mp4" -Recurse -ErrorAction SilentlyContinue |
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
if (-not ("Win32.NativeMethods" -as [type])) {
    Add-Type @"
using System;
using System.Runtime.InteropServices;
namespace Win32 {
    public class NativeMethods {
        [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Ansi)]
        public static extern IntPtr FindWindowA(string lpClassName, string lpWindowName);
    }
}
"@
}

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

# Resolve the binary path now that cargo has placed the artifact —
# pinned target dir is preferred, short path is the fallback.
if (-not (Resolve-RecorderExe)) {
    Write-Fail "Recorder binary not found at either path:"
    Write-Fail "  $RecorderExePinned"
    Write-Fail "  $RecorderExeShort"
    exit 1
}
Write-OK "Resolved recorder binary: $RecorderExe"

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

if (-not (Resolve-TestGameExe)) {
    Write-Fail "Test game binary not found at either path:"
    Write-Fail "  $TestGameExePinned"
    Write-Fail "  $TestGameExeShort"
    exit 1
}
Write-OK "Resolved test_game binary: $TestGameExe"

# ─── Step 3: Prepare output dir ───────────────────────────────────────────────

Write-Step "Prepare output directory"

New-Item -ItemType Directory -Force -Path $VideoOutputDir | Out-Null
# Clear everything in the output dir so we don't confuse a prior run's
# mp4 for the current one. Recordings nest under session_*/ subfolders.
Get-ChildItem -Path $VideoOutputDir -Recurse -Force -ErrorAction SilentlyContinue |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Write-OK "Output dir ready: $VideoOutputDir"

# ─── Step 4: Launch test game ─────────────────────────────────────────────────

Write-Step "Launch test_game"

$gameProc = Start-Process -FilePath $TestGameExe -PassThru

# Wait for the window to appear (proof that D3D init succeeded)
$windowReady = Wait-ForProcessReady -Name $TestGameProcess -TimeoutSec 15
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

# Redirect recorder stdout/stderr to files so CI can surface errors
# that would otherwise vanish into a detached console window.
$recorderStdout = Join-Path $VideoOutputDir "recorder.stdout.log"
$recorderStderr = Join-Path $VideoOutputDir "recorder.stderr.log"
$recorderProc = Start-Process -FilePath $RecorderExe -PassThru `
    -RedirectStandardOutput $recorderStdout `
    -RedirectStandardError  $recorderStderr

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
    Write-Host "--- output dir listing ---" -ForegroundColor Yellow
    Get-ChildItem -Path $VideoOutputDir -Recurse -Force -ErrorAction SilentlyContinue |
        Select-Object FullName, Length | Format-Table
    foreach ($logName in @("recorder.stdout.log", "recorder.stderr.log")) {
        $logPath = Join-Path $VideoOutputDir $logName
        if (Test-Path $logPath) {
            Write-Host "--- tail of $logName ---" -ForegroundColor Yellow
            Get-Content $logPath -Tail 60
        }
    }
    $appLogDir = Join-Path $env:LOCALAPPDATA "GameData Recorder"
    if (Test-Path $appLogDir) {
        Write-Host "--- recorder LocalAppData listing ---" -ForegroundColor Yellow
        Get-ChildItem -Path $appLogDir -Recurse -Force -ErrorAction SilentlyContinue |
            Select-Object FullName, Length | Format-Table
    }
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