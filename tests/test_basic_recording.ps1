#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Basic automated test for GameData Recorder

.DESCRIPTION
    This script tests basic recording functionality:
    1. Starts the recorder
    2. Detects a running game
    3. Waits for recording
    4. Stops recording
    5. Validates output files exist

.PARAMETER RecorderPath
    Path to gamedata-recorder.exe (default: auto-detect from ./target/release/)

.PARAMETER GameExe
    Game executable name to test with (default: notepad.exe as test app)

.PARAMETER RecordingDuration
    How long to record in seconds (default: 10)

.PARAMETER OutputPath
    Recording output directory (default: ./data_dump/games/)

.EXAMPLE
    .\test_basic_recording.ps1
    Runs basic test with notepad.exe

.EXAMPLE
    .\test_basic_recording.ps1 -GameExe "solitaire.exe" -RecordingDuration 20
    Tests with Windows Solitaire for 20 seconds
#>

param(
    [string]$RecorderPath = "",
    [string]$GameExe = "notepad.exe",
    [int]$RecordingDuration = 10,
    [string]$OutputPath = "./data_dump/games/"
)

$ErrorActionPreference = "Stop"
$StartTime = Get-Date

function Log-Message {
    param([string]$Message)
    $Timestamp = Get-Date -Format "HH:mm:ss"
    Write-Host "[$Timestamp] $Message" -ForegroundColor Cyan
}

function Test-FileExists {
    param([string]$Path, [string]$Description)
    if (Test-Path $Path) {
        Log-Message "✓ Found $Description at $Path"
        return $true
    } else {
        Log-Message "✗ $Description not found at $Path"
        return $false
    }
}

function Wait-ForProcess {
    param(
        [string]$ProcessName,
        [int]$TimeoutSeconds = 30
    )

    $ProcessName = $ProcessName -replace '\.exe$', ''
    Log-Message "Waiting for $ProcessName to start..."

    $Elapsed = 0
    while ($Elapsed -lt $TimeoutSeconds) {
        $Process = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue
        if ($Process) {
            Log-Message "✓ $ProcessName is running (PID: $($Process.Id))"
            return $Process
        }
        Start-Sleep -Seconds 1
        $Elapsed++
    }

    throw "$ProcessName did not start within ${TimeoutSeconds}s timeout"
}

function Wait-ForRecordingFiles {
    param(
        [string]$Path,
        [int]$TimeoutSeconds = 30
    )

    Log-Message "Waiting for recording files to appear..."

    $Elapsed = 0
    while ($Elapsed -lt $TimeoutSeconds) {
        $Mp4Files = Get-ChildItem -Path $Path -Filter "*.mp4" -Recurse -ErrorAction SilentlyContinue
        if ($Mp4Files.Count -gt 0) {
            Log-Message "✓ Found $($Mp4Files.Count) recording file(s)"
            return $Mp4Files
        }
        Start-Sleep -Seconds 1
        $Elapsed++
    }

    throw "No recording files found within ${TimeoutSeconds}s timeout"
}

# ===== MAIN TEST =====

Log-Message "=== GameData Recorder Basic Test ==="
Log-Message "Test Parameters:"
Log-Message "  Game: $GameExe"
Log-Message "  Duration: ${RecordingDuration}s"
Log-Message "  Output: $OutputPath"

# Step 1: Find recorder
Log-Message "`n[Step 1] Finding recorder..."
if (-not $RecorderPath) {
    $PossiblePaths = @(
        ".\target\release\gamedata-recorder.exe",
        ".\gamedata-recorder.exe",
        "C:\Program Files\GameData Recorder\gamedata-recorder.exe"
    )

    foreach ($Path in $PossiblePaths) {
        if (Test-Path $Path) {
            $RecorderPath = Resolve-Path $Path
            break
        }
    }

    if (-not $RecorderPath) {
        throw "Could not find gamedata-recorder.exe. Please specify -RecorderPath"
    }
}

Log-Message "✓ Recorder found at: $RecorderPath"

# Step 2: Check if game is running, if not start it
Log-Message "`n[Step 2] Checking game..."
try {
    $GameProcessName = $GameExe -replace '\.exe$', ''
    $GameProcess = Get-Process -Name $GameProcessName -ErrorAction SilentlyContinue

    if (-not $GameProcess) {
        Log-Message "Starting $GameExe..."
        Start-Process $GameExe
        $GameProcess = Wait-ForProcess -ProcessName $GameExe -TimeoutSeconds 10
    } else {
        Log-Message "✓ $GameExe is already running (PID: $($GameProcess.Id))"
    }
} catch {
    throw "Failed to start/find game: $_"
}

# Step 3: Start recorder
Log-Message "`n[Step 3] Starting recorder..."
try {
    $RecorderProcess = Start-Process -FilePath $RecorderPath -PassThru
    Log-Message "✓ Recorder started (PID: $($RecorderProcess.Id))"

    # Give recorder time to initialize
    Start-Sleep -Seconds 3
} catch {
    throw "Failed to start recorder: $_"
}

# Step 4: Wait for recording to start (check for recording files)
Log-Message "`n[Step 4] Waiting for recording to start..."
try {
    # Wait a bit for auto-start or check if recording started
    Start-Sleep -Seconds 5

    # Check if recording files are being created
    $InitialFiles = Get-ChildItem -Path $OutputPath -Filter "*.mp4" -Recurse -ErrorAction SilentlyContinue
    $InitialCount = $InitialFiles.Count

    Log-Message "Recording files: $InitialCount (before waiting)"

    # Wait for recording duration
    Log-Message "Recording for ${RecordingDuration}s..."
    Start-Sleep -Seconds $RecordingDuration

} catch {
    # Clean up on error
    Stop-Process -Id $RecorderProcess.Id -ErrorAction SilentlyContinue
    throw "Error during recording: $_"
}

# Step 5: Stop recording
Log-Message "`n[Step 5] Stopping recording..."
try {
    Stop-Process -Id $RecorderProcess.Id -ErrorAction SilentlyContinue
    Log-Message "✓ Recorder stopped"

    # Wait for files to be written
    Start-Sleep -Seconds 2
} catch {
    Log-Message "Warning: Error stopping recorder: $_"
}

# Step 6: Validate output
Log-Message "`n[Step 6] Validating output..."
$Success = $false

try {
    $FinalFiles = Get-ChildItem -Path $OutputPath -Filter "*.mp4" -Recurse -ErrorAction SilentlyContinue
    $NewFiles = $FinalFiles | Where-Object { $_.LastWriteTime -gt $StartTime }

    Log-Message "Files created during test: $($NewFiles.Count)"

    foreach ($File in $NewFiles) {
        $SizeInMB = [math]::Round($File.Length / 1MB, 2)
        Log-Message "  ✓ $($File.Name) - ${SizeInMB}MB"
    }

    if ($NewFiles.Count -gt 0) {
        $Success = $true
    } else {
        Log-Message "✗ No new recording files found!"
    }

    # Check for metadata files
    $MetadataFiles = Get-ChildItem -Path $OutputPath -Filter "video_metadata.json" -Recurse -ErrorAction SilentlyContinue
    $NewMetadata = $MetadataFiles | Where-Object { $_.LastWriteTime -gt $StartTime }

    if ($NewMetadata.Count -gt 0) {
        Log-Message "✓ Metadata files found: $($NewMetadata.Count)"

        # Show metadata content
        foreach ($MetaFile in $NewMetadata) {
            try {
                $Metadata = Get-Content $MetaFile.FullName -Raw | ConvertFrom-Json
                Log-Message "  Game: $($Metadata.game_exe)"
                Log-Message "  Duration: $($Metadata.duration)s"
                Log-Message "  Resolution: $($Metadata.game_resolution)"
            } catch {
                Log-Message "  Warning: Could not parse metadata"
            }
        }
    }

} catch {
    Log-Message "✗ Error validating output: $_"
}

# Summary
Log-Message "`n=== Test Summary ==="
$Duration = (Get-Date) - $StartTime
Log-Message "Total test duration: $($Duration.TotalSeconds.ToString('F2'))s"

if ($Success) {
    Log-Message "✓ TEST PASSED" -ForegroundColor Green
    exit 0
} else {
    Log-Message "✗ TEST FAILED" -ForegroundColor Red
    exit 1
}
