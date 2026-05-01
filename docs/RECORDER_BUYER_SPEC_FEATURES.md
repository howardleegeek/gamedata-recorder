# Recorder Buyer-Spec Features — Implementation Spec

*Date: 2026-04-28 · Status: Spec (puffydev to implement) · Owner: puffydev · Reviewer: Howard*

> Three recorder-side features required by the buyer-spec acceptance bar (Option A, per `oyster-enrichment/docs/BUYER_SPEC_v1_PLAN.md`). Implementation contract — file/line proposals, tests, rollout — so puffydev can land a PR without back-and-forth.

---

## 1. Scope

Three independent features, each gated behind a Preferences flag so the legacy recording path stays untouched when disabled:

| # | Feature | Why the buyer needs it | Effort |
|---|---|---|---|
| 1 | **route_type tagging via F1/F2/F3** | Buyer's gameinfo schema requires per-clip `route_type ∈ {1,2,3}` (常规漫游 / 特殊路线 / 循环录制). Operator-annotated, not derivable post-hoc. | ~4h |
| 2 | **5-min auto-cap timer** | Buyer-spec acceptance: every clip `5 ≤ duration ≤ 6 min`. Today the recorder runs unbounded until F9; ~30% of clips drift outside the window in operator dry-runs. | ~2h |
| 3 | **UI-element refusal** | Buyer rejects clips with 弹窗/系统通知/水印/模态框/读档CG/游戏配置界面/切出画面/电脑菜单栏. Need defensive abort + warn UX. | ~1d |

**Total: ~1.5 days.** All additive — no behaviour change when the new prefs are disabled.

---

## 2. Feature 1: route_type tagging via F1/F2/F3

### Behaviour

- **F1** → `route_type = 1` (常规漫游) · **F2** → `route_type = 2` (特殊路线) · **F3** → `route_type = 3` (循环录制)
- **Per-clip, sticky** (per Howard's plan §B4). Tag set at start of recording, not per-segment.
- Pressed **before** F9 / before auto-record: tag remembered for next recording.
- Pressed **during** recording: log + tray notification (`"route_type set to N (applies to NEXT recording)"`); current clip's tag stays at its initial value.
- Pressed when no recording exists: just set the pending tag.
- No tag pressed before recording start → `route_type = None`, omitted from `session.json` (buyer treats absent as "operator forgot, flag for review").

### Output — extends `metadata/session.json` (LEM)

```rust
// in src/output_types/lem_metadata.rs SessionMetadata struct
#[serde(skip_serializing_if = "Option::is_none")]
pub route_type: Option<u8>,
```

### File:line proposals

| File | Where | Change |
|---|---|---|
| `src/config.rs:48` (`Preferences`) | After `disable_action_camera_output` | Add `route_type_hotkey_f1/f2/f3: String` (defaults `"F1"/"F2"/"F3"` — rebindable like F9) + `disable_route_type_hotkeys: bool` (default `false`). |
| `src/config.rs:104` (`Default`) | Mirror | Defaults as above. |
| `src/app_state.rs:43` (near `listening_for_new_hotkey`) | New atomic | `pending_route_type: AtomicU8` (0=None, 1/2/3=tag). Set by input handler, consumed at `Recording::start`. |
| `src/record/recording.rs:28` (`RecordingParams`) | After `disable_action_camera_output` (line 42) | `pub route_type: Option<u8>`. |
| `src/record/recording.rs:45` (`Recording`) | After `disable_action_camera_output` field (line 59) | `route_type: Option<u8>`. |
| `src/record/recording.rs:77` (destructure) | Wire it through | Mirror `disable_action_camera_output` exactly. |
| `src/record/recorder.rs:243` (where `RecordingParams` is built) | After `disable_action_camera_output: ...` | `route_type: app_state.pending_route_type.swap(0, Ordering::SeqCst).filter(|n| (1..=3).contains(n))` (consume + clear so next recording starts fresh). |
| `src/tokio_thread.rs:1085` (key-match block in `on_input`) | New arms next to start/stop | Match F1/F2/F3 → `app_state.pending_route_type.store(N, ...)` + `UiUpdate::TrayNotification`. Skip if `disable_route_type_hotkeys`. |
| `src/output_types/lem_metadata.rs:8` (`SessionMetadata`) | Anywhere | Add `route_type: Option<u8>` per snippet above. |
| `src/record/recording.rs:469` (call to `LocalRecording::write_metadata_and_validate`) | Plumb through | Pass `self.route_type`. |

### Tests (4 unit tests in `tests/route_type.rs`)

1. F1 event → `pending_route_type == 1`.
2. F2 event → `pending_route_type == 2`.
3. F3 event → `pending_route_type == 3`.
4. No tag key pressed → `Recording::route_type == None` AND `session.json` does NOT contain `route_type`.

**Effort: ~4h** (mostly plumbing one `Option<u8>` through three structs + LEM writer).

---

## 3. Feature 2: 5-min auto-cap timer

### Behaviour

- Timer starts at `Recording::start()` (uses existing `start_instant: Instant`, line 89).
- Fires when `start_instant.elapsed() >= max_duration`. **Default `330s` (5:30)** — 30s buffer above the buyer's 5-min minimum, well under the 6-min ceiling.
- On fire: graceful stop via the same code path as F9 stop (writes metadata, fsyncs MP4, validates).
- Tray notification: `"Auto-stopped at 5:30 (buyer-spec cap)"`.
- Recording is **not** marked INVALID — clean stop.

### CLI / config override

- `--max-duration-secs <N>` flag at `src/main.rs` for session-only override.
- `pub max_duration_secs: u32` in `Preferences` (default `330`).
- `0` = disabled (operator opt-out).

### File:line proposals

| File | Where | Change |
|---|---|---|
| `src/config.rs:48` (`Preferences`) | New fields | `pub max_duration_secs: u32` (default `330`) + `pub disable_auto_cap_timer: bool` (default `false`). |
| `src/main.rs:~110` (after `color_eyre::install`) | New | Parse `--max-duration-secs` from `std::env::args()` (one flag, no clap dep). Mutate via `app_state.config.write_safe()` like the CI override at line 150. |
| `src/record/recording.rs:28` (`RecordingParams`) | New | `pub max_duration: Option<Duration>`. |
| `src/record/recording.rs:45` (`Recording`) | New | `max_duration: Option<Duration>` (mirror params). |
| `src/record/recorder.rs:243` (params build) | New line | `max_duration: if config.preferences.disable_auto_cap_timer || config.preferences.max_duration_secs == 0 { None } else { Some(Duration::from_secs(config.preferences.max_duration_secs as u64)) }`. |
| `src/record/recorder.rs:341` (`Recorder::poll`) | After FPS update, before workstation_locked check | `if let Some(max) = recording.max_duration && recording.start_instant.elapsed() >= max { tracing::info!("Auto-cap reached"); self.stop(input_capture).await?; }`. Tray notification via `app_state.ui_update_tx`. |

**Why `Recorder::poll`?** Already called periodically with `&InputCapture` in scope — same shape as the existing `workstation_locked_timeout` path at line 368. Reuse that "tick-driven graceful stop" pattern verbatim.

### Tests (3 unit tests in `tests/auto_cap.rs`)

1. **Timer fires** — `max_duration = Duration::from_millis(100)`, sleep 150ms, call `poll`, assert recording stopped + `metadata/session.json` exists.
2. **`--max-duration-secs 60` overrides** — set CLI arg, build Preferences, assert `recording.max_duration == Some(Duration::from_secs(60))`.
3. **Cleanup on fire** — same as #1, assert no INVALID marker in recording dir.

**Effort: ~2h** (smallest feature, single tick check + plumbing).

---

## 4. Feature 3: UI-element refusal

### Buyer rejection criteria (verbatim)

弹窗 (popups) · 系统级通知 (OS notifications) · 水印 (watermarks) · 模态对话框 (modals) · 读档/通关/加载CG (load/win/loading cinematics) · 录制游戏内配置界面 (in-game config menus) · 切出画面 (alt-tab away) · 露出电脑菜单栏 (exposed taskbar)

### Three detection options

| Option | Approach | Cost | Coverage |
|---|---|---|---|
| **HHH-A** | ML frame classifier every N seconds | Heavy: ~50ms/frame, GPU contention with encoder, frame-drop risk at 30fps | All 8 categories |
| **HHH-B** | Window-state heuristic (foreground + fullscreen + topmost) | Light: ~1ms/check, pure Win32 | 4/8: alt-tab, exposed taskbar, OS notifications stealing focus, modals from other apps. **Misses 4/8**: in-game popups/CG/config/watermarks (those are pixels inside the game window) |
| **HHH-C** | Operator pre/post checklist | Trivial: text + checkboxes | 100% — but depends on operator honesty |

### Recommendation: **HHH-B + HHH-C** (defence in depth)

- **HHH-B** as automatic guard (lightweight, deterministic, catches the 4 OS-side categories).
- **HHH-C** as operator checklist for the 4 in-game categories the recorder cannot reliably auto-detect.
- **HHH-A out of scope** for this MVP — revisit only if buyer rejection rate justifies the GPU cost.

### Detection logic (HHH-B)

Every 2 seconds during recording (piggy-back on `Recorder::poll` tick):

1. `let fg_hwnd = GetForegroundWindow();`
2. If `fg_hwnd != recording.hwnd` → log warning + tray notification + increment `ui_violation_count`.
3. `GetWindowRect(recording.hwnd)` and `MonitorFromWindow + GetMonitorInfoW`. If window rect ≠ monitor rect (±2px tolerance for windowed-borderless) → log warning + violation.
4. If `ui_violation_count > 5` (≥10s of violation) → log error and offer abort. **Default: warn but DO NOT abort** — operator can re-record, but we don't trash a 4-min clip on a 10s OS notification.
5. Optional: `Preferences::auto_abort_on_ui_violation = true` (default `false`) → `self.stop(...)` after threshold.

`ui_violations: u32` written to `session.json` so downstream `lint_buyer_spec.py` can flag for review.

### Operator checklist (HHH-C) — two new egui prompts

**Pre-recording** (modal before F9 starts a clip):
- [ ] Game in fullscreen
- [ ] No mods, watermarks, or HUD overlays visible
- [ ] No active game menu / pause / loading screen
- [ ] route_type tag (F1/F2/F3) set correctly

**Post-recording** (after auto-cap or F9):
- [ ] No popups during clip
- [ ] No load screens / cinematics
- [ ] No alt-tab events

If any post box unchecked → `ui_violation_self_reported: true` in `session.json` (lint flags). Both prompts gated by `Preferences::disable_ui_refusal` (default `false`).

### File:line proposals

| File | Where | Change |
|---|---|---|
| **NEW** `src/record/window_guard.rs` | `#[cfg(target_os = "windows")]` | Pure Win32 module: `pub fn check_foreground_and_fullscreen(expected_hwnd: HWND) -> Result<UiViolation>` returning `enum UiViolation { Ok, NotForeground, NotFullscreen, MonitorMismatch }`. Imports pattern already established in `recording.rs:524-565`. |
| `src/record/mod.rs` | After `pub mod recorder;` | `pub mod window_guard;` (cfg-gated). |
| `src/config.rs:48` (`Preferences`) | New fields | `disable_ui_refusal: bool` (default `false`), `auto_abort_on_ui_violation: bool` (default `false`), `ui_violation_threshold: u32` (default `5`, ≈10s). |
| `src/record/recording.rs:45` (`Recording`) | New field | `ui_violation_count: u32`. |
| `src/record/recorder.rs:341` (`Recorder::poll`) | After FPS, before workstation_locked | Every-2s gated check (track via new `Instant` field): call `window_guard::check_foreground_and_fullscreen(recording.hwnd)`. On violation → `recording.ui_violation_count += 1`, log warn, tray notify. If `count > threshold && config.auto_abort_on_ui_violation` → `self.stop(input_capture).await?`. |
| `src/output_types/lem_metadata.rs` (`SessionMetadata`) | New fields | `#[serde(skip_serializing_if = "Option::is_none")] pub ui_violations: Option<u32>` and `pub ui_violation_self_reported: Option<bool>`. |
| `src/record/recording.rs:469` (metadata write call) | Plumb | Pass `Some(self.ui_violation_count)`. |
| `src/ui/views/` | NEW `pre_recording_checklist.rs` + `post_recording_checklist.rs` | Two egui views; gated by `Preferences::disable_ui_refusal`; mutate `RecordingStatus` and `ui_violation_self_reported`. |

### Tests (5 unit tests in `tests/window_guard.rs`)

1. Foreground == game HWND → `Ok(UiViolation::Ok)`.
2. Foreground != game HWND → `Ok(UiViolation::NotForeground)`.
3. Window rect != monitor rect (1920x1080 monitor, 1920x600 window) → `Ok(UiViolation::NotFullscreen)`.
4. 6 consecutive violations + `threshold=5` + `auto_abort_on_ui_violation=true` → `Recorder::poll` calls `stop()`.
5. `session.json` contains `ui_violations` field after injecting violations and stopping.

**Effort: ~1d** (new module + UI surfaces + tests). Bulk of complexity is the egui prompts, not the Win32 detection.

---

## 5. Implementation order

1. **Feature 2 first (~2h)** — single-file change in `Recorder::poll`, lowest risk, immediately unblocks the buyer's hard `5≤x≤6min` requirement. Exercises the "tick-driven graceful stop" pattern Feature 3 reuses.
2. **Feature 1 next (~4h)** — touches more files but no new I/O patterns. Plumbing through the existing `RecordingParams → Recording → session.json` chain.
3. **Feature 3 last (~1d)** — most complex; new module + two new UI views + new metadata fields. By then operators have Feature 2 catching duration drift and Feature 1 tagging clips, so Feature 3 is the polish layer.

**Each feature is independently shippable** behind its Preferences flag.

---

## 6. Rollout plan — feature flags

| Flag | Default | Effect when `true` |
|---|---|---|
| `disable_route_type_hotkeys` | `false` | F1/F2/F3 fall through to game; `route_type` never appears in `session.json`. |
| `disable_auto_cap_timer` | `false` | Timer never fires; recording runs until F9. (Equivalent to `max_duration_secs = 0`.) |
| `disable_ui_refusal` | `false` | `window_guard` checks skipped; pre/post checklists hidden. `ui_violations` and `ui_violation_self_reported` omitted from `session.json`. |

### Migration

All new `Preferences` fields use `#[serde(default)]` with sensible defaults — old configs stay readable, new fields populate from defaults on first read. Mirrors the existing `disable_action_camera_output` pattern at `src/config.rs:93`. Operators on v2.5.x configs upgrading need no manual edit.

### Validation gate (pre-merge)

1. Record a 10s clip with all three flags disabled → no behaviour change vs. trunk (regression guard).
2. Record a 5:30 clip with `auto_cap_timer=enabled` → clean stop at 5:30, `session.json` exists, no INVALID marker.
3. Press F2 before recording, record 5:30 → `session.json` contains `route_type: 2`.
4. Alt-tab to desktop mid-recording for 12s → `ui_violations >= 5` in `session.json`.

---

## 7. Open questions for puffydev

1. **Existing hotkey infrastructure** — `src/tokio_thread.rs:1070` (`on_input`) currently matches start/stop hotkeys via `name_to_virtual_keycode` per event. Is there a cleaner registration pattern (e.g. a `HotkeyRegistry` you've been wanting to introduce), or should F1/F2/F3 just be three more arms in the existing `match (recording_state, key)` block? Latter is lowest-risk; former is the "correct" refactor but expands scope.
2. **Window enumeration API** — codebase already uses the `windows` crate (`recorder.rs:13`, `recording.rs:11`). Should `window_guard.rs` use the same crate, or do you prefer the lighter `winapi`? Default suggestion: stay with `windows` for consistency — flag if binary-size has been a concern.
3. **Settings UI for new preferences** — `src/ui/views/` has the existing pattern for Preferences UI. New flags + route-type-keybind editors: same view as F9 rebind, or a new "Buyer Spec" sub-page? Default suggestion: same view, under a collapsible "Buyer-spec recording" section.
4. **Default for `max_duration_secs`** — picked `330` (5:30) as the buyer-spec midpoint. Reason to use `300` (5:00) instead — operator slack at the *end* for shut-down latency? Configurable either way.
5. **HHH-A (ML detector)** — explicitly out of scope, but if you've prototyped a frame classifier elsewhere, flag it and we can extend Feature 3 to optionally call it. Otherwise HHH-B + HHH-C is what ships.

---

## 8. Out of scope (do NOT do in this PR)

- Engine telemetry hooks (player position / rotation / Follow Offset) — `depth-hook` crate's existing scope, separate spec.
- `metric_scale` calibration — handled at enrichment time in `oyster-enrichment/`, not the recorder.
- gameinfo.xlsx writing — operator-level deliverable, generated post-hoc by `bin/convert_to_buyer_spec.py`.
- `action_camera.json` writer changes — Howard authorized that scope in a prior session; treat as separate.
- Any change to the OBS / recorder backend, MP4 muxing, or input capture path.

---

*Spec answers "what does puffydev implement to close the recorder-side gap to the buyer-spec acceptance bar". Mid-PR questions → ping Howard. Architectural pivots → re-spec first.*
