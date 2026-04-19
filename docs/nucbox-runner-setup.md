# Nucbox Self-Hosted Runner Setup

The nightly E2E smoke workflow (`.github/workflows/nightly-nucbox-e2e.yml`)
runs on a self-hosted runner that must be online, physically connected to a
Steam-licensed machine, and able to launch a supported game unattended.

This doc describes how to bring that runner up on a nucbox (Windows 11)
the first time, or how to replace it after a hardware swap.

## Prerequisites on the nucbox

- Windows 10 or 11, signed in as the user that owns the Steam library.
- Steam installed, game library downloaded, Steam launches without extra
  prompts (no Steam Guard interactive step at startup, no pending updates).
- PowerShell 7 (`pwsh`) on `PATH`.
- `ffprobe` on `PATH` (for the `real_fps_ok` assertion; warns and skips if
  missing, so not hard-blocking).
- GameData Recorder binary at
  `C:\Users\Howard\Downloads\gdr252\gamedata-recorder.exe` (or override
  with `-RecorderPath`).

## Install the runner

Runner registration token is generated per-repo and expires quickly. Fresh
token each time:

1. On the nucbox, open PowerShell and create the runner directory:
   ```powershell
   mkdir C:\actions-runner
   cd C:\actions-runner
   ```
2. In a browser, go to
   `https://github.com/howardleegeek/gamedata-recorder/settings/actions/runners/new`
   and copy the latest download + config commands. Example shape:
   ```powershell
   Invoke-WebRequest -Uri https://github.com/actions/runner/releases/download/v2.319.1/actions-runner-win-x64-2.319.1.zip -OutFile actions-runner.zip
   Expand-Archive -Path .\actions-runner.zip -DestinationPath .
   ```
3. Register with a fresh token and our labels:
   ```powershell
   .\config.cmd --url https://github.com/howardleegeek/gamedata-recorder `
                --token <FRESH_TOKEN> `
                --name nucbox `
                --labels self-hosted,windows,nucbox `
                --unattended `
                --replace
   ```
4. Install as a Windows service so it survives reboots:
   ```powershell
   .\svc.cmd install
   .\svc.cmd start
   ```
5. Confirm the runner is idle-green on the repo Actions > Runners page.

## First-time validation

Trigger the workflow manually and watch it:
- GitHub UI: `Actions > Nightly Nucbox E2E > Run workflow` (defaults appid=730, duration=90).
- From your mac: `scripts/e2e-quick.sh 730 90`.

Expected: exit 0, a green check in Actions, and
`$env:TEMP\gdr-e2e-result.json` on the nucbox with 11 `PASS` assertions
(10 if `ffprobe` is missing and `real_fps_ok` is SKIP).

## Replacing a dead runner

```powershell
cd C:\actions-runner
.\svc.cmd stop
.\svc.cmd uninstall
.\config.cmd remove --token <REMOVAL_TOKEN>
```
Then redo steps 1-5 above with a fresh registration token.

## Troubleshooting

- **Runner offline in GitHub UI**: check `svc.cmd status`; if the service is
  crashing, the `_diag/` folder has the logs.
- **Steam opens but game never appears**: the app id may require an
  interactive launcher (Rockstar, EA Play, etc.). The smoke script exits 2
  with a helpful message in that case. Pick a different app id (CS2 = 730
  is the default because it launches without a dialog).
- **`ffprobe not on PATH`**: install ffmpeg and make sure `ffprobe.exe` is
  reachable. Until then, the script runs but `real_fps_ok` is marked SKIP.
- **Recorder memory out of bounds**: the sample window (every 5s) is logged
  in the JSON; if the recorder is consistently above 800 MB at idle, file
  a Rust-side issue - the smoke script will fail the
  `memory_within_bounds` assertion but not mask it.
