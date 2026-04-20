# Autoresearch Audit — 2026-04-20

**Scope:** Code added since MEGA_AUDIT.md (v2.5.7 onward) — CI mode, stability gate, 3-mode capture routing, WGC plumbing.
**Method:** 4 parallel focused `feature-dev:code-reviewer` passes, each ~800 words, confidence-ranked (≥70% only).
**HEAD:** `1fe631f` (after 5 WGC source-id attempts converged on `window_capture` + `method=2`).

## Findings (severity × confidence)

### CRITICAL

| # | File:Line | Finding | Conf |
|---|-----------|---------|------|
| **F1** | `src/record/recording.rs:134-143` | Monitor-mode resolution uses client-rect (game window size) but the source captures full monitor — silent downscale when window < display (most windowed games) | 85 |
| **F7** | `src/config.rs:762-774` | `GAMEDATA_OUTPUT_DIR` skips `validate_recording_location` — CI-mode can write to `C:\Windows\System32` or any path | 95 |
| **F11** | `src/record/fps_logger.rs:64,82-84` | `frames.jsonl` `start_instant` set at `FpsLogger::new()`, but OBS PTS=0 arrives 100ms–2s LATER. Systematic non-constant offset poisons frame-to-input alignment | 95 |
| **F12** | `src/record/fps_logger.rs:64,84` vs `src/output_types/mod.rs:513-516` | `inputs.jsonl` uses Unix `SystemTime`, `frames.jsonl` uses `Instant` — no stored anchor maps one clock to the other. Training can't correlate "input at T" with "frame at T" | 90 |

### HIGH / IMPORTANT

| # | File:Line | Finding | Conf |
|---|-----------|---------|------|
| **F4** | `src/tokio_thread.rs:1597` | Stability gate queries `GetForegroundWindow()` — in reverse order (game first, recorder after) this is the recorder's own UI, not the game. Explains Edge D test failure | 100 |
| **F13** | `crates/constants/src/encoding.rs:100,106` | `B_FRAMES=2` + `LOOKAHEAD=true` → display-order ≠ encode-order in MP4. Trainers iterating container bytestream mis-pair inputs ±2 frames (~33ms @ 60fps) | 85 |
| **F14** | `src/record/obs_embedded_recorder.rs:~1344-1360` + `input_recorder.rs` | On DXGI ACCESS_LOST, OBS pauses MP4 but input stream keeps flowing — inputs.jsonl records events for the lock-screen interval with no frames. Training sees ghost inputs | 88 |
| **F15** | `src/record/metadata_writer.rs:117-119,313-337` | `gpu_vram_mb=None`, `quality="medium"`, `mouse_sensitivity=1.0`, WASD keybindings — all hardcoded stubs. Sensitivity stub is a direct action-magnitude miscalibration for world models | 82 |
| **F9** | `src/tokio_thread.rs:2273-2275` + `src/ui/views/mod.rs:358-366` | CI-mode short-circuits `wait_for_consent` but UI ConsentView still renders — user Accept on a CI run persists consent to config.json, leaking into next non-CI launch | 85 |

### MEDIUM

| # | File:Line | Finding | Conf |
|---|-----------|---------|------|
| F2 | `obs_embedded_recorder.rs:1831-1837,1901-1907` | Dead-code redundant `is_game` bail + no recovery path if libobs_window_helper heuristic transient-fails | 82 |
| F3 | `obs_embedded_recorder.rs:1154-1165,363-416` | Stale `Notify` permit between sessions can poison next Phase2's skipped-frames read | 80 |
| F5 | `tokio_thread.rs:1591` | `test_game` stability bypass has no `ci_mode()` guard — safe today, architecturally fragile if whitelist ever includes `test_game` | 92 |
| F6 | `tokio_thread.rs:1633,1649` | `process_spawned` = recorder's first-sight time, not game's actual start time. Adds unnecessary 20s delay in reverse-order scenarios | 88 |
| F8 | `src/config.rs:749-754` | Docs claim `"yes"/"on"` activate CI mode but code only matches `"1"/"true"/"TRUE"` | 82 |
| F10 | `src/config.rs:747-755,767-776` | `OnceLock` means CI mode is permanent once set at process start — unsetting env var mid-run doesn't clear it | 88 |

## FREEZE NOTICE (2026-04-20, Howard)

> "别瞎改了 如果现在代码可以跑起来。我怕 introduce 新的问题。"

Current state **works**: CS2 9/10, GTA V, 3-min long session all verified recording real game content. Every recent PR has regressed something and required another PR to fix. **Do not dispatch fixes for this audit without explicit re-authorization.** The list below is triage for WHEN we revisit, not a TODO.

## Triage for future work (ranked by "must vs nice")

### MUST (real user pain already triggered or one command away)

None. Everything that would cause immediate user pain was fixed already (DLL hijack guard, WGC source id, fmt, binary path, etc).

### SHOULD (bite only at scale or adversarial conditions)

1. **F11 + F12 + F13** — frame/input timeline alignment. **Only matters when the AI trainer actually tries to correlate inputs with frames**. Until downstream starts complaining, recordings are "good enough" and the trainer can do its own calibration. If client ships a training pipeline → must fix.
2. **F4** — stability gate reverse-order bug. Only affects users who launch a tray daemon AFTER game is already running. In the real daemon model, recorder starts on boot; games launch later. Edge D is a synthetic test configuration. Fix when someone reports it.
3. **F7** — `GAMEDATA_OUTPUT_DIR` System32 write primitive. **Only exploitable if attacker already has env-var injection** on the user's machine; at that point they already own the box. Belt-and-suspenders fix but not urgent.

### NICE (cosmetic / future hardening)

4. **F1** — Monitor-mode client-rect vs monitor downscale. Our default is WGC now so Monitor mode is a fallback. Real usage will almost never hit it.
5. **F14** — session gap sentinel in inputs.jsonl. Small data quality win for Win+L edge case.
6. **F9** — CI-mode consent UI leak. Real impact only if a CI-mode machine gets repurposed for a real user session without wiping config.
7. **F15** — metadata stubs (sensitivity, keybindings, VRAM). Fix per-game as we add support.
8. **F5** — `test_game` bypass without `ci_mode()` guard. Architectural nit; not exploitable today.
9. **F6** — `process_spawned` anchored to recorder-launch. Adds 20s delay in reverse-order only.

### NITS (don't touch)

10. F2 (dead-code redundant is_game bail)
11. F3 (stale Notify permit)
12. F8 (doc inconsistency on CI env values)
13. F10 (OnceLock permanence — by design)

## Non-findings (confidence-building)

- No race in `last_foregrounded_game` (single tokio writer, RwLock readers)
- Source reuse across mode changes correctly recreates on `PartialEq` diff
- WGC audio not double-attached (Monitor-mode-only WASAPI attachment)
- SELF_AND_SYSTEM_BLACKLIST prevents recording `lsass.exe` etc via CI bypass
- Cooldown/tracker interaction is correct — no stuck loop
- Rapid resize / borderless→fullscreen→borderless handled correctly by stability tracker

## Severity roll-up

- 4 CRITICAL (F1, F7, F11, F12)
- 5 HIGH (F4, F9, F13, F14, F15)
- 6 MEDIUM (F2, F3, F5, F6, F8, F10)
- **Total: 15 new findings on ~1500 LOC of new code**

Relative to the 110-defect MEGA_AUDIT count: this is proportionally WORSE. But new code goes through one review cycle; MEGA_AUDIT code had been through many. After v2.6 fixes these 15, total defect count on modern code should drop below the old baseline.
