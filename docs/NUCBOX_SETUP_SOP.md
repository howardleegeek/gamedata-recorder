# Nucbox Setup SOP — for OpenCode to execute

**Audience:** OpenCode agent running on nucbox (Windows 11, user `Howard`)
**Goal:** Get `C:\Users\Howard\Downloads\gdr-src\run_ci.ps1` passing end-to-end on nucbox so the team can run full-pipeline integration tests locally instead of only on GitHub Actions.
**Runtime:** ~25-30 minutes total (mostly VS Build Tools download + cargo build)
**Requires:** admin/elevated execution. If OpenCode is running non-elevated, stop and tell Howard to relaunch as admin.

---

## Pre-flight: are you elevated?

Run this FIRST. If it prints `False`, stop — ask Howard to relaunch OpenCode as Administrator.

```powershell
([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
```

Expected: `True`

---

## Step 1 — Verify what's already installed

A previous session installed `rustup` and `cargo 1.95.0`, and a git clone of the repo exists at `C:\Users\Howard\Downloads\gdr-src`. Confirm before re-installing.

```powershell
# Cargo
& "C:\Users\Howard\.cargo\bin\cargo.exe" --version
# Expect: cargo 1.95.0 (f2d3ce0bd 2026-03-21) or newer

# Repo
Test-Path "C:\Users\Howard\Downloads\gdr-src\.git"
# Expect: True

# MSVC linker (THIS is the thing we're about to install)
Test-Path "C:\BuildTools\VC\Tools\MSVC"
# If False → proceed to Step 2. If True → skip to Step 3.

# ffmpeg
Get-Command ffprobe -ErrorAction SilentlyContinue
# If null → noted for later (not blocking; check_video.py uses it)
```

If any of the first two checks fail, you need to re-run rustup install and/or git clone. See Appendix A at the bottom.

---

## Step 2 — Install Visual Studio Build Tools (needed for MSVC `link.exe`)

**Only run if `C:\BuildTools\VC\Tools\MSVC` does NOT exist.**

### 2a. Download the bootstrapper (if not already in Temp)

```powershell
$installer = "C:\Windows\Temp\vs_BuildTools.exe"
if (-not (Test-Path $installer)) {
    Invoke-WebRequest `
        -Uri "https://aka.ms/vs/17/release/vs_BuildTools.exe" `
        -OutFile $installer
}
Get-Item $installer | Select-Object Name, Length
# Expect: ~4.5 MB, ~4463552 bytes
```

### 2b. Install silently — MSVC + Win11 SDK workload

This takes **15-20 minutes** and downloads ~3 GB. Run it synchronously so you know when it finishes.

```powershell
$args = @(
    '--quiet', '--wait', '--norestart', '--nocache',
    '--installPath', 'C:\BuildTools',
    '--add', 'Microsoft.VisualStudio.Workload.VCTools',
    '--add', 'Microsoft.VisualStudio.Component.Windows11SDK.22621',
    '--includeRecommended'
)
$proc = Start-Process -FilePath "C:\Windows\Temp\vs_BuildTools.exe" `
    -ArgumentList $args -Wait -PassThru
Write-Host "Installer exit code: $($proc.ExitCode)"
```

Exit codes:
- `0` — success
- `3010` — success, reboot required (fine for us, ignore)
- anything else — failure; check `C:\ProgramData\Microsoft\VisualStudio\Packages\_Instances\*\state.json`

### 2c. Verify MSVC landed

```powershell
Test-Path "C:\BuildTools\VC\Tools\MSVC"
Get-ChildItem "C:\BuildTools\VC\Tools\MSVC" | Select-Object -First 1 Name
# Expect: True, and some version like 14.41.34120
```

If this returns False after exit code 0, something's wrong with the installer choice — read `C:\ProgramData\Microsoft\VisualStudio\Setup\Logs\*.log` and report back to Howard before proceeding.

---

## Step 3 — Install ffmpeg (provides ffprobe for check_video.py validation)

```powershell
if (-not (Get-Command ffprobe -ErrorAction SilentlyContinue)) {
    # Try winget first, fall back to Chocolatey.
    $wingetOk = $false
    try {
        winget install --id Gyan.FFmpeg --silent --accept-source-agreements --accept-package-agreements
        $wingetOk = $LASTEXITCODE -eq 0
    } catch {}

    if (-not $wingetOk) {
        # Chocolatey fallback — install it if not present, then ffmpeg
        if (-not (Get-Command choco -ErrorAction SilentlyContinue)) {
            Set-ExecutionPolicy Bypass -Scope Process -Force
            [System.Net.ServicePointManager]::SecurityProtocol = `
                [System.Net.ServicePointManager]::SecurityProtocol -bor 3072
            Invoke-Expression ((New-Object System.Net.WebClient).DownloadString(
                'https://community.chocolatey.org/install.ps1'))
        }
        choco install -y ffmpeg --no-progress
    }
}

# Refresh PATH so ffprobe is visible in this session
$env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + `
    ";" + [System.Environment]::GetEnvironmentVariable("Path","User")

# Verify
ffprobe -version | Select-Object -First 1
# Expect: ffprobe version N.N.N Copyright ...
```

---

## Step 4 — Make cargo/link.exe discoverable in every new shell

The MSVC toolchain lives at `C:\BuildTools\VC\Tools\MSVC\*\bin\Hostx64\x64\` and needs to be on PATH when `cargo build` runs, OR cargo needs the VS environment variables set.

**Recommended (cleanest):** create a persistent user-PATH addition for cargo and leverage the VS dev-shell initializer for MSVC.

```powershell
# Make cargo permanent for user
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*\.cargo\bin*") {
    [Environment]::SetEnvironmentVariable(
        "Path", "$userPath;C:\Users\Howard\.cargo\bin", "User")
}

# Create a wrapper that runs cargo inside the VS dev environment.
# `Enter-VsDevShell` brings in cl.exe, link.exe, vcvars, SDK libs.
# Save this as C:\Users\Howard\bin\cargo-vs.ps1 so future runs can use it.
$wrapperDir = "C:\Users\Howard\bin"
New-Item -ItemType Directory -Force -Path $wrapperDir | Out-Null
@'
# cargo-vs.ps1 — runs cargo inside the Visual Studio dev environment
$vsInstall = "C:\BuildTools"
Import-Module "$vsInstall\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
Enter-VsDevShell -VsInstallPath $vsInstall -DevCmdArguments '-arch=x64' -SkipAutomaticLocation | Out-Null
& "C:\Users\Howard\.cargo\bin\cargo.exe" @args
exit $LASTEXITCODE
'@ | Set-Content -Path "$wrapperDir\cargo-vs.ps1" -Encoding UTF8
```

Verify the wrapper works:

```powershell
& "C:\Users\Howard\bin\cargo-vs.ps1" --version
# Expect: cargo 1.95.0 (or similar) — proves VS env + cargo both reachable
```

---

## Step 5 — Sync the repo to latest main

```powershell
Set-Location "C:\Users\Howard\Downloads\gdr-src"
git fetch --all --tags
git reset --hard origin/main
git log --oneline -3
# Top commit should start with c79035e (or newer if Howard pushed more)
```

---

## Step 6 — First full build

This is the smoke test that the setup worked. Takes ~10 minutes.

```powershell
Set-Location "C:\Users\Howard\Downloads\gdr-src"
# Run cargo build inside VS dev environment so link.exe is on PATH.
& "C:\Users\Howard\bin\cargo-vs.ps1" build --release 2>&1 |
    Tee-Object -FilePath build.log |
    Select-Object -Last 20

# Verify artifact
Test-Path "target\x86_64-pc-windows-msvc\release\gamedata-recorder.exe"
# Expect: True
```

If build fails, the last 20 lines of `build.log` on-screen plus full `build.log` on disk are the evidence to hand back to Howard.

---

## Step 7 — Install cargo-obs-build and stage OBS runtime DLLs

The recorder needs libobs DLLs at runtime next to the exe.

```powershell
& "C:\Users\Howard\bin\cargo-vs.ps1" install cargo-obs-build
& "C:\Users\Howard\bin\cargo-vs.ps1" obs-build build `
    --out-dir target\x86_64-pc-windows-msvc\release

# Verify the expected layout
Get-ChildItem target\x86_64-pc-windows-msvc\release\gamedata-recorder.exe,
              target\x86_64-pc-windows-msvc\release\obs-plugins,
              target\x86_64-pc-windows-msvc\release\data `
    -ErrorAction SilentlyContinue |
    Select-Object FullName, Length
# Expect all three paths to exist.
```

---

## Step 8 — Run the full E2E test

```powershell
Set-Location "C:\Users\Howard\Downloads\gdr-src"
$env:GAMEDATA_CI_MODE = "1"
& "C:\Users\Howard\bin\cargo-vs.ps1" fmt -- --check
if ($LASTEXITCODE -ne 0) {
    Write-Host "WARN: cargo fmt check failed; not blocking but heads-up for Howard"
}

# Use the test harness the team already wrote
.\run_ci.ps1 -RecordSeconds 5 -SkipCommit
Write-Host "run_ci.ps1 exit: $LASTEXITCODE"

# If 0 → SUCCESS. Report back with:
#   - The exit code
#   - Contents of ci_output\*.mp4 (the size in MB + a one-line ffprobe summary)
#   - Last 30 lines of any recorder log at %LocalAppData%\GameData Recorder\*.log
```

---

## Success criteria (Howard can tick these off)

- [ ] `C:\BuildTools\VC\Tools\MSVC\*` exists (MSVC toolchain installed)
- [ ] `ffprobe -version` works
- [ ] `cargo-vs.ps1 build --release` completes with exit code 0
- [ ] `target\x86_64-pc-windows-msvc\release\gamedata-recorder.exe` exists
- [ ] `cargo-vs.ps1 obs-build build` staged `obs-plugins\` + `data\` next to exe
- [ ] `.\run_ci.ps1 -RecordSeconds 5 -SkipCommit` exits 0
- [ ] `ci_output\*.mp4` exists, ≥3 seconds, brightness ≥10, fps ≥27

---

## Failure recovery

If any step fails, report back to Howard with:

1. **What step number** (1-8)
2. **Exact error message** (full, not paraphrased)
3. **Log file paths** the user should look at
4. **Your guess at root cause** if you have one
5. **Whether a retry would help** or we need a code change

Do NOT silently skip failures. This pipeline is worthless if any step is broken.

---

## Appendix A — Rebuild rustup / git clone (if Step 1 reported missing)

### Rustup (if `cargo.exe` missing)

```powershell
Invoke-WebRequest `
    -Uri "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe" `
    -OutFile "C:\Windows\Temp\rustup-init.exe"
& "C:\Windows\Temp\rustup-init.exe" -y `
    --default-host x86_64-pc-windows-msvc `
    --default-toolchain stable `
    --profile minimal `
    --no-modify-path
```

### Git clone (if `gdr-src` missing)

```powershell
git clone https://github.com/howardleegeek/gamedata-recorder.git `
    C:\Users\Howard\Downloads\gdr-src
```

---

## Appendix B — How to self-verify the SOP completed

Paste this at the very end. If it prints `ALL GOOD`, everything worked.

```powershell
$checks = @(
    @{ name = "MSVC toolchain"; test = { Test-Path "C:\BuildTools\VC\Tools\MSVC" } }
    @{ name = "cargo"; test = { Test-Path "C:\Users\Howard\.cargo\bin\cargo.exe" } }
    @{ name = "cargo-vs wrapper"; test = { Test-Path "C:\Users\Howard\bin\cargo-vs.ps1" } }
    @{ name = "ffprobe on PATH"; test = { $null -ne (Get-Command ffprobe -ErrorAction SilentlyContinue) } }
    @{ name = "recorder binary"; test = { Test-Path "C:\Users\Howard\Downloads\gdr-src\target\x86_64-pc-windows-msvc\release\gamedata-recorder.exe" } }
    @{ name = "OBS plugins staged"; test = { Test-Path "C:\Users\Howard\Downloads\gdr-src\target\x86_64-pc-windows-msvc\release\obs-plugins" } }
    @{ name = "run_ci.ps1 exists"; test = { Test-Path "C:\Users\Howard\Downloads\gdr-src\run_ci.ps1" } }
)
$fails = @()
foreach ($c in $checks) {
    $ok = & $c.test
    $icon = if ($ok) { "[OK]" } else { "[FAIL]" }
    Write-Host "$icon $($c.name)"
    if (-not $ok) { $fails += $c.name }
}
if ($fails.Count -eq 0) {
    Write-Host "ALL GOOD" -ForegroundColor Green
} else {
    Write-Host "FAIL: $($fails -join ', ')" -ForegroundColor Red
}
```
