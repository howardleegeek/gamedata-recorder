#Requires -Version 7.0
<#
.SYNOPSIS
  Autonomous E2E smoke test for GameData Recorder.

.DESCRIPTION
  Launches the recorder, launches a Steam game, waits $DurationSec seconds,
  stops both, then runs 11 assertions against the latest session folder.
  Writes a JSON report and exits 0 on all-pass, 1 on any fail, 2 on abort.

.EXAMPLE
  pwsh -NoProfile -NonInteractive -File scripts\e2e-smoke.ps1

.EXAMPLE
  pwsh -NoProfile -NonInteractive -File scripts\e2e-smoke.ps1 -GameAppId 730 -DurationSec 90
#>
[CmdletBinding()]
param(
    [string]$RecorderPath = "C:\Users\Howard\Downloads\gdr252\gamedata-recorder.exe",
    [int]$GameAppId       = 730,
    [int]$DurationSec     = 90,
    [string]$OutputJson   = (Join-Path $env:TEMP 'gdr-e2e-result.json')
)

$ErrorActionPreference = 'Stop'
$script:StartTime = Get-Date

# --- Game app id -> target process exe name (lowercase, no .exe) ----------
# Mirrors crates/constants/src/lib.rs GAME_WHITELIST entries.
$script:AppIdToExe = @{
    730      = 'cs2'              # Counter-Strike 2 (headless-friendly, no launcher)
    271590   = 'gta5'             # GTA V (Rockstar launcher - will abort)
    1091500  = 'cyberpunk2077'    # Cyberpunk 2077
    570      = 'dota2'            # Dota 2
    578080   = 'tslgame'          # PUBG
    252950   = 'rocketleague'     # Rocket League
}
# App ids that require an interactive launcher dialog - abort cleanly.
$script:LauncherAppIds = @(271590)  # add more as we discover them

# --- State container passed through the run ------------------------------
$script:Run = [ordered]@{
    recorder_proc  = $null
    recorder_pid   = $null
    game_proc_name = $null
    session_dir    = $null
    version        = '2.5.3'
    commit         = $null
    memory_samples = [System.Collections.Generic.List[long]]::new()
    assertions     = [System.Collections.Generic.List[object]]::new()
    aborted        = $false
    abort_reason   = $null
}

# --- Logging helper -------------------------------------------------------
function Write-Step {
    param([string]$Message, [string]$Level = 'INFO')
    $ts = (Get-Date).ToString('HH:mm:ss')
    $color = switch ($Level) {
        'ERROR' { 'Red' }
        'WARN'  { 'Yellow' }
        'OK'    { 'Green' }
        default { 'Cyan' }
    }
    Write-Host "[$ts] [$Level] $Message" -ForegroundColor $color
}

# --- Assertion recorder ---------------------------------------------------
function Add-Assertion {
    param(
        [string]$Name,
        [ValidateSet('PASS','FAIL','WARN','SKIP')][string]$Status,
        [string]$Detail = '',
        $Expected = $null,
        $Actual = $null,
        [ValidateSet('HIGH','MEDIUM','LOW')][string]$Severity = 'HIGH'
    )
    $obj = [ordered]@{
        name     = $Name
        status   = $Status
        detail   = $Detail
    }
    if ($null -ne $Expected) { $obj['expected'] = $Expected }
    if ($null -ne $Actual)   { $obj['actual']   = $Actual }
    if ($Status -eq 'FAIL' -or $Status -eq 'WARN') { $obj['severity'] = $Severity }
    $script:Run.assertions.Add([PSCustomObject]$obj) | Out-Null
    $lvl = if ($Status -eq 'PASS') { 'OK' } elseif ($Status -eq 'WARN' -or $Status -eq 'SKIP') { 'WARN' } else { 'ERROR' }
    Write-Step "$Name -> $Status  $Detail" $lvl
}

# --- Clean slate: stop any leftover recorder ------------------------------
function Stop-RecorderProcesses {
    Get-Process -Name 'gamedata-recorder' -ErrorAction SilentlyContinue |
        ForEach-Object {
            Write-Step "Killing leftover recorder pid=$($_.Id)" 'WARN'
            try { $_ | Stop-Process -Force -ErrorAction SilentlyContinue } catch { }
        }
    Start-Sleep -Milliseconds 500
}

function Stop-GameProcess {
    param([string]$Name)
    if (-not $Name) { return }
    Get-Process -Name $Name -ErrorAction SilentlyContinue |
        ForEach-Object {
            Write-Step "Killing game $Name pid=$($_.Id)" 'WARN'
            try { $_ | Stop-Process -Force -ErrorAction SilentlyContinue } catch { }
        }
}

# --- Fail-closed cleanup (always runs) ------------------------------------
function Invoke-Cleanup {
    Write-Step 'Cleanup: stopping all processes' 'WARN'
    Stop-GameProcess -Name $script:Run.game_proc_name
    Stop-RecorderProcesses
}

# --- Write final JSON report ---------------------------------------------
function Write-Report {
    param([int]$ExitCode)
    $passed = ($script:Run.assertions | Where-Object { $_.status -eq 'PASS' }).Count
    $failed = ($script:Run.assertions | Where-Object { $_.status -eq 'FAIL' }).Count
    $duration = [int]((Get-Date) - $script:StartTime).TotalSeconds
    $expectedExe = $script:AppIdToExe[$GameAppId]

    $report = [ordered]@{
        version           = $script:Run.version
        commit            = $script:Run.commit
        game              = if ($expectedExe) { "$expectedExe.exe" } else { 'unknown' }
        game_app_id       = $GameAppId
        session_folder    = $script:Run.session_dir
        passed            = $passed
        failed            = $failed
        aborted           = $script:Run.aborted
        abort_reason      = $script:Run.abort_reason
        timestamp_utc     = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
        duration_total_s  = $duration
        exit_code         = $ExitCode
        assertions        = @($script:Run.assertions)
    }
    $json = $report | ConvertTo-Json -Depth 8
    $dir = Split-Path -Parent $OutputJson
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    Set-Content -Path $OutputJson -Value $json -Encoding UTF8
    Write-Step "Report written: $OutputJson" 'OK'
    Write-Host ""
    Write-Host "=== SUMMARY ===" -ForegroundColor Yellow
    Write-Host "  passed: $passed" -ForegroundColor Green
    Write-Host "  failed: $failed" -ForegroundColor Red
    Write-Host "  exit:   $ExitCode" -ForegroundColor Cyan
    Write-Host "==============="
}

# =========================================================================
# MAIN
# =========================================================================

try {
    Write-Step "Starting E2E smoke test (recorder=$RecorderPath, app=$GameAppId, dur=${DurationSec}s)"

    # Resolve git commit (best-effort; $PSScriptRoot is <repo>/scripts).
    try {
        if ($PSScriptRoot) {
            $repoRoot = Split-Path -Parent $PSScriptRoot
            $gitSha = (& git -C $repoRoot rev-parse HEAD 2>$null)
            if ($LASTEXITCODE -eq 0 -and $gitSha) { $script:Run.commit = $gitSha.Trim() }
        }
    } catch { }

    # Guard: launcher-gated games abort cleanly (exit 2).
    if ($GameAppId -in $script:LauncherAppIds) {
        $script:Run.aborted = $true
        $script:Run.abort_reason = "app_id $GameAppId requires interactive launcher login (e.g. Rockstar); use a different game_appid"
        Write-Step $script:Run.abort_reason 'ERROR'
        Write-Report -ExitCode 2
        exit 2
    }

    # Guard: recorder binary exists.
    if (-not (Test-Path $RecorderPath)) {
        $script:Run.aborted = $true
        $script:Run.abort_reason = "recorder not found at $RecorderPath"
        Write-Step $script:Run.abort_reason 'ERROR'
        Write-Report -ExitCode 2
        exit 2
    }

    # Soft warn: ffprobe missing.
    $ffprobeAvailable = $null -ne (Get-Command ffprobe -ErrorAction SilentlyContinue)
    if (-not $ffprobeAvailable) {
        Write-Step "ffprobe not on PATH - real_fps_ok will be skipped" 'WARN'
    }

    # 1. Clean slate.
    Stop-RecorderProcesses

    # Snapshot of files under recorder install dir + repo before the run,
    # used for no_garbage_committed assertion.
    $recorderDir = Split-Path -Parent $RecorderPath
    $garbagePatterns = @('.oystercode', '.research', 'autoresearch_log.md')
    $preRunGarbage = @()
    foreach ($pat in $garbagePatterns) {
        $preRunGarbage += Get-ChildItem -Path $recorderDir -Recurse -Force -ErrorAction SilentlyContinue `
            -Filter $pat | Select-Object -ExpandProperty FullName
    }

    # 2. Launch recorder hidden.
    Write-Step "Launching recorder"
    $script:Run.recorder_proc = Start-Process -FilePath $RecorderPath -WindowStyle Hidden -PassThru
    $script:Run.recorder_pid  = $script:Run.recorder_proc.Id

    # 3. Sanity: process alive + memory in expected range.
    Start-Sleep -Seconds 5
    $p = Get-Process -Id $script:Run.recorder_pid -ErrorAction SilentlyContinue
    if (-not $p) {
        $script:Run.aborted = $true
        $script:Run.abort_reason = "recorder died within 5s of launch"
        Write-Step $script:Run.abort_reason 'ERROR'
        Write-Report -ExitCode 2
        exit 2
    }
    $memMb = [int]($p.WorkingSet64 / 1MB)
    $script:Run.memory_samples.Add([long]$p.WorkingSet64) | Out-Null
    Write-Step "Recorder alive pid=$($script:Run.recorder_pid) mem=${memMb}MB" 'OK'
    if ($memMb -lt 100 -or $memMb -gt 800) {
        Write-Step "Recorder memory=${memMb}MB outside 100-800MB window" 'WARN'
    }

    # 4. Launch the game via Steam. Start-Process invokes the steam:// URL
    # handler via ShellExecute; no explicit verb needed.
    Write-Step "Launching Steam game appid=$GameAppId"
    try {
        Start-Process "steam://rungameid/$GameAppId"
    } catch {
        # Fallback: some Windows setups refuse to shell-execute URLs from
        # pwsh. Use cmd's `start` which always goes through the shell.
        cmd.exe /c "start `"`" `"steam://rungameid/$GameAppId`"" | Out-Null
    }
    $expectedExe = $script:AppIdToExe[$GameAppId]
    if (-not $expectedExe) {
        $script:Run.aborted = $true
        $script:Run.abort_reason = "unknown app_id $GameAppId - add mapping to AppIdToExe"
        Write-Step $script:Run.abort_reason 'ERROR'
        Write-Report -ExitCode 2
        exit 2
    }

    # 5. Poll up to 120s for the target exe.
    Write-Step "Waiting up to 120s for $expectedExe.exe"
    $gameProc = $null
    $deadline = (Get-Date).AddSeconds(120)
    $launcherDetected = $false
    while ((Get-Date) -lt $deadline) {
        $gameProc = Get-Process -Name $expectedExe -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($gameProc) { break }
        # Detect a handful of known interactive launchers and abort rather
        # than hang. (Rockstar/PlayGTAV/Epic/EA.)
        $launchers = @('PlayGTAV','Launcher','RockstarService','EpicGamesLauncher','EADesktop')
        foreach ($l in $launchers) {
            if (Get-Process -Name $l -ErrorAction SilentlyContinue) {
                $launcherDetected = $true
                $script:Run.abort_reason = "detected launcher '$l' instead of target game - this game requires interactive login; use a different game_appid"
                break
            }
        }
        if ($launcherDetected) { break }
        Start-Sleep -Seconds 5
    }

    if ($launcherDetected) {
        $script:Run.aborted = $true
        Write-Step $script:Run.abort_reason 'ERROR'
        Write-Report -ExitCode 2
        exit 2
    }
    if (-not $gameProc) {
        $script:Run.aborted = $true
        $script:Run.abort_reason = "target game $expectedExe.exe never appeared within 120s"
        Write-Step $script:Run.abort_reason 'ERROR'
        Write-Report -ExitCode 2
        exit 2
    }

    $script:Run.game_proc_name = $expectedExe
    Write-Step "Game live: $expectedExe pid=$($gameProc.Id)" 'OK'

    # 6. Wait $DurationSec seconds, sampling recorder RAM every 5s.
    $sampleDeadline = (Get-Date).AddSeconds($DurationSec)
    while ((Get-Date) -lt $sampleDeadline) {
        $rp = Get-Process -Id $script:Run.recorder_pid -ErrorAction SilentlyContinue
        if ($rp) { $script:Run.memory_samples.Add([long]$rp.WorkingSet64) | Out-Null }
        Start-Sleep -Seconds 5
    }
    Write-Step "Duration elapsed ($DurationSec s)"

    # 7. Stop the game.
    Stop-GameProcess -Name $expectedExe

    # 8. Wait up to 15s for the recorder to finalize metadata.json.
    $baseRecordings = Join-Path $env:LOCALAPPDATA 'GameData Recorder\recordings'
    if (-not (Test-Path $baseRecordings)) {
        New-Item -ItemType Directory -Path $baseRecordings -Force | Out-Null
    }
    $finalizeDeadline = (Get-Date).AddSeconds(15)
    $session = $null
    while ((Get-Date) -lt $finalizeDeadline) {
        $candidate = Get-ChildItem -Path $baseRecordings -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -like 'session_*' } |
            Sort-Object LastWriteTimeUtc -Descending |
            Select-Object -First 1
        if ($candidate -and (Test-Path (Join-Path $candidate.FullName 'metadata.json'))) {
            $session = $candidate
            break
        }
        Start-Sleep -Seconds 1
    }

    if ($session) {
        $script:Run.session_dir = $session.FullName
        Write-Step "Session folder: $($session.FullName)" 'OK'
    } else {
        Write-Step "No session folder with metadata.json found" 'ERROR'
    }

    # =====================================================================
    # 11 ASSERTIONS
    # =====================================================================

    # Parse metadata.json once (if present).
    $meta = $null
    $metaPath = if ($session) { Join-Path $session.FullName 'metadata.json' } else { $null }
    if ($metaPath -and (Test-Path $metaPath)) {
        try { $meta = Get-Content $metaPath -Raw | ConvertFrom-Json } catch { $meta = $null }
    }

    # 1. metadata_exists
    if ($metaPath -and (Test-Path $metaPath)) {
        Add-Assertion -Name 'metadata_exists' -Status 'PASS' -Detail $metaPath
    } else {
        Add-Assertion -Name 'metadata_exists' -Status 'FAIL' -Detail 'metadata.json missing' -Expected 'present' -Actual 'missing'
    }

    # 2. mp4_size_ok
    $mp4Path = if ($session) { Join-Path $session.FullName 'recording.mp4' } else { $null }
    if ($mp4Path -and (Test-Path $mp4Path)) {
        $sizeMb = [math]::Round((Get-Item $mp4Path).Length / 1MB, 2)
        if ($sizeMb -gt 10) {
            Add-Assertion -Name 'mp4_size_ok' -Status 'PASS' -Detail "${sizeMb} MB"
        } else {
            Add-Assertion -Name 'mp4_size_ok' -Status 'FAIL' -Expected '>10 MB' -Actual "${sizeMb} MB"
        }
    } else {
        Add-Assertion -Name 'mp4_size_ok' -Status 'FAIL' -Detail 'recording.mp4 missing' -Expected 'present' -Actual 'missing'
    }

    # 3. game_exe_match
    if ($meta -and $meta.game_exe) {
        $actualExe = [string]$meta.game_exe
        if ($actualExe -ieq "$expectedExe.exe" -or $actualExe -ieq $expectedExe) {
            Add-Assertion -Name 'game_exe_match' -Status 'PASS' -Detail $actualExe
        } else {
            Add-Assertion -Name 'game_exe_match' -Status 'FAIL' -Expected "$expectedExe.exe" -Actual $actualExe
        }
    } else {
        Add-Assertion -Name 'game_exe_match' -Status 'FAIL' -Detail 'metadata.game_exe missing' -Expected "$expectedExe.exe" -Actual $null
    }

    # 4. not_launcher_window
    if ($meta -and $meta.PSObject.Properties.Match('window_name').Count -gt 0) {
        $wn = [string]$meta.window_name
        if ([string]::IsNullOrEmpty($wn)) {
            Add-Assertion -Name 'not_launcher_window' -Status 'PASS' -Detail 'window_name empty'
        } elseif ($wn -match '(?i)launcher') {
            Add-Assertion -Name 'not_launcher_window' -Status 'FAIL' -Expected 'no Launcher' -Actual $wn
        } else {
            Add-Assertion -Name 'not_launcher_window' -Status 'PASS' -Detail $wn
        }
    } else {
        Add-Assertion -Name 'not_launcher_window' -Status 'PASS' -Detail 'window_name field absent'
    }

    # 5. monitor_capture_active (recorder_extra.window_capture == false)
    if ($meta -and $meta.recorder_extra -and $meta.recorder_extra.PSObject.Properties.Match('window_capture').Count -gt 0) {
        $wc = $meta.recorder_extra.window_capture
        if ($wc -eq $false) {
            Add-Assertion -Name 'monitor_capture_active' -Status 'PASS' -Detail 'window_capture=false'
        } else {
            Add-Assertion -Name 'monitor_capture_active' -Status 'FAIL' -Expected 'false' -Actual $wc
        }
    } else {
        Add-Assertion -Name 'monitor_capture_active' -Status 'FAIL' -Detail 'recorder_extra.window_capture missing' -Expected 'false' -Actual $null
    }

    # 6. duration_sufficient
    if ($meta -and $meta.duration) {
        $d = [double]$meta.duration
        if ($d -gt 60) {
            Add-Assertion -Name 'duration_sufficient' -Status 'PASS' -Detail "$($d)s"
        } else {
            Add-Assertion -Name 'duration_sufficient' -Status 'FAIL' -Expected '>60' -Actual $d
        }
    } else {
        Add-Assertion -Name 'duration_sufficient' -Status 'FAIL' -Detail 'metadata.duration missing' -Expected '>60' -Actual $null
    }

    # 7. real_fps_ok (ffprobe-derived)
    if (-not $ffprobeAvailable) {
        Add-Assertion -Name 'real_fps_ok' -Status 'SKIP' -Detail 'ffprobe not on PATH' -Severity 'LOW'
    } elseif (-not ($mp4Path -and (Test-Path $mp4Path))) {
        Add-Assertion -Name 'real_fps_ok' -Status 'FAIL' -Detail 'no mp4' -Expected '>25 fps' -Actual $null
    } else {
        try {
            $rfps = & ffprobe -v error -select_streams v:0 -show_entries stream=r_frame_rate -of default=nokey=1:noprint_wrappers=1 $mp4Path 2>$null
            if ($rfps -match '^(\d+)/(\d+)$' -and [int]$Matches[2] -ne 0) {
                $fps = [math]::Round([int]$Matches[1] / [int]$Matches[2], 2)
            } elseif ($rfps -match '^\d+(\.\d+)?$') {
                $fps = [double]$rfps
            } else {
                $fps = 0
            }
            if ($fps -gt 25) {
                Add-Assertion -Name 'real_fps_ok' -Status 'PASS' -Detail "${fps} fps"
            } else {
                Add-Assertion -Name 'real_fps_ok' -Status 'FAIL' -Expected '>25 fps' -Actual $fps
            }
        } catch {
            Add-Assertion -Name 'real_fps_ok' -Status 'FAIL' -Detail "ffprobe error: $_" -Expected '>25 fps' -Actual $null
        }
    }

    # 8. input_events_present (soft - warn-only if Raw Input broken)
    $inputsPath = if ($session) { Join-Path $session.FullName 'inputs.jsonl' } else { $null }
    $stderrPath = if ($session) { Join-Path $session.FullName 'stderr.log' } else { $null }
    $rawInputBroken = $false
    if ($stderrPath -and (Test-Path $stderrPath)) {
        $se = Get-Content $stderrPath -Raw -ErrorAction SilentlyContinue
        if ($se -match '(?i)RegisterRawInputDevices failed|raw input.*unavailable|raw input.*error') {
            $rawInputBroken = $true
        }
    }
    if ($inputsPath -and (Test-Path $inputsPath)) {
        $lines = @(Get-Content $inputsPath -ErrorAction SilentlyContinue) | Where-Object { $_ -and $_.Trim() -ne '' }
        $lineCount = @($lines).Count
        if ($lineCount -gt 10) {
            Add-Assertion -Name 'input_events_present' -Status 'PASS' -Detail "$lineCount events"
        } elseif ($rawInputBroken) {
            Add-Assertion -Name 'input_events_present' -Status 'WARN' -Detail "only $lineCount events (Raw Input reported broken in stderr.log)" -Expected '>10' -Actual $lineCount -Severity 'LOW'
        } else {
            Add-Assertion -Name 'input_events_present' -Status 'FAIL' -Expected '>10' -Actual $lineCount
        }
    } else {
        Add-Assertion -Name 'input_events_present' -Status 'FAIL' -Detail 'inputs.jsonl missing' -Expected '>10' -Actual $null
    }

    # 9. no_garbage_committed
    $postRunGarbage = @()
    $repoRoot = Split-Path -Parent $PSScriptRoot
    foreach ($root in @($recorderDir, $repoRoot) | Select-Object -Unique) {
        if (-not $root -or -not (Test-Path $root)) { continue }
        foreach ($pat in $garbagePatterns) {
            $postRunGarbage += Get-ChildItem -Path $root -Recurse -Force -ErrorAction SilentlyContinue `
                -Filter $pat | Select-Object -ExpandProperty FullName
        }
    }
    $newGarbage = @($postRunGarbage | Where-Object { $_ -notin $preRunGarbage })
    if ($newGarbage.Count -eq 0) {
        Add-Assertion -Name 'no_garbage_committed' -Status 'PASS' -Detail 'no new .oystercode/.research/autoresearch_log.md'
    } else {
        Add-Assertion -Name 'no_garbage_committed' -Status 'FAIL' -Expected 'no new garbage files' -Actual ($newGarbage -join '; ')
    }

    # 10. no_panic_in_stderr
    if ($stderrPath -and (Test-Path $stderrPath)) {
        $se = Get-Content $stderrPath -Raw -ErrorAction SilentlyContinue
        if ($se -match 'panicked|thread .* panicked') {
            $firstPanic = ($se -split "`n" | Where-Object { $_ -match 'panicked' } | Select-Object -First 1)
            Add-Assertion -Name 'no_panic_in_stderr' -Status 'FAIL' -Expected 'no panics' -Actual $firstPanic
        } else {
            Add-Assertion -Name 'no_panic_in_stderr' -Status 'PASS' -Detail 'clean stderr'
        }
    } else {
        Add-Assertion -Name 'no_panic_in_stderr' -Status 'PASS' -Detail 'stderr.log absent (likely never hit an error path)'
    }

    # 11. memory_within_bounds
    $maxBytes = 0
    foreach ($s in $script:Run.memory_samples) { if ($s -gt $maxBytes) { $maxBytes = $s } }
    $maxMb = [math]::Round($maxBytes / 1MB, 2)
    if ($maxMb -le 1024) {
        $sampleCount = $script:Run.memory_samples.Count
        Add-Assertion -Name 'memory_within_bounds' -Status 'PASS' -Detail "peak=$($maxMb)MB ($sampleCount samples)"
    } else {
        Add-Assertion -Name 'memory_within_bounds' -Status 'FAIL' -Expected '<=1024 MB' -Actual "${maxMb} MB"
    }

    # 12. Stop recorder (step 10 of behavior spec).
    Stop-RecorderProcesses

    $failCount = ($script:Run.assertions | Where-Object { $_.status -eq 'FAIL' }).Count
    $exitCode = if ($failCount -eq 0) { 0 } else { 1 }
    Write-Report -ExitCode $exitCode
    exit $exitCode
}
catch {
    Write-Step "Unhandled error: $_" 'ERROR'
    $script:Run.aborted = $true
    $script:Run.abort_reason = "unhandled exception: $_"
    Write-Report -ExitCode 2
    exit 2
}
finally {
    Invoke-Cleanup
}
