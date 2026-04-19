# GameData Recorder — Bug Log (v2.0 → v2.5.1)

This document tracks every bug discovered during client red-team + real-user testing, grouped by symptom category. All bugs listed here have been fixed or mitigated in v2.5.1.

---

## 1. Memory / Resource Crashes

### BUG-001: GTA V OOM crash (ERR_GFX_D3D_DEFERRED_MEM) on 16 GB clients
- **Symptom**: User (BINGDILIU) reported GTA V Enhanced crashed with memory error while recorder was active.
- **Root cause**: `input_recorder` and `lem_input_recorder` used `mpsc::unbounded_channel()`. Under heavy input (60+ events/sec), the channel grew without bound, consuming memory in competition with the game.
- **Fix (v2.3.x)**: Switched to `mpsc::channel(16_384)` with `try_send()` and graceful drop on full. Capacity `INPUT_CHANNEL_CAPACITY = 16_384` + `LEM_CHANNEL_CAPACITY = 16_384`.
- **Files**: `src/record/input_recorder.rs`, `src/record/lem_input_recorder.rs`.

### BUG-002: FPS logger unbounded growth
- **Symptom**: Long recording sessions leaked memory via FPS statistics buffer.
- **Fix**: Added `MAX_FPS_ENTRIES = 600` cap with drain-on-overflow.
- **File**: `src/record/fps_logger.rs`.

---

## 2. Focus-Stealing / Desktop Ejection

### BUG-003: MessageBox popups during active recording kicked user to desktop
- **Symptom**: "有跳出桌面我根本没办法玩" — user got kicked out of fullscreen game mid-session.
- **Root cause**: `error_message_box()`, `warning_message_box()`, `info_message_box()` all called Win32 `MessageBoxW` which creates a modal window and steals focus from the game.
- **Fix (v2.4.x)**: Removed all non-startup MessageBox calls. Converted to `tracing::warn!` / `tracing::error!`. Only a single pre-startup error box remains (before any game is running).
- **Files**: `src/ui/notification.rs`, `src/tokio_thread.rs`.

### BUG-004: "Outdated version" popup interrupted recording
- **Symptom**: Version-check popup appeared mid-game, stealing focus.
- **Fix**: Removed blocking popup; version-check now logs a warning.
- **File**: `src/tokio_thread.rs`.

### BUG-005: Overlay window stole focus via `SW_SHOWDEFAULT`
- **Symptom**: In-game overlay activating the recorder window and minimizing the game.
- **Root cause**: `ShowWindow(hwnd, SW_SHOWDEFAULT)` both shows and activates a window.
- **Fix**: Changed to `SW_SHOWNA` (non-activating show).
- **File**: `src/ui/overlay.rs`.

### BUG-006: Tray "Quit" handler called `focus_window()` on game
- **Symptom**: Exiting the tray silently re-focused the last foreground app.
- **Fix**: Removed `focus_window()` and `set_minimized(false)` from quit handler.
- **File**: `src/ui/tray_icon.rs`.

---

## 3. Black Screen / Capture Failures

### BUG-007: Black video for fullscreen-exclusive games
- **Symptom**: "录了五分钟 一点游戏内容都没录到 全部都桌面的东西" — only desktop was captured, not GTA V.
- **Root cause**: OBS window-capture can't reach fullscreen-exclusive or DX12/Vulkan games without hook injection.
- **Fix (v2.4.x)**: Switched default to `MonitorCaptureSourceBuilder` (full-screen monitor capture) instead of window capture.
- **File**: `src/record/obs_embedded_recorder.rs`.

### BUG-008: Multi-monitor setup captured wrong screen
- **Symptom**: On multi-display machines, the recorder captured the primary monitor while the game ran on a secondary one.
- **Fix**: Added `MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)` to select the monitor the game is on (`hmonitor_ptr_for_hwnd`).
- **File**: `src/record/obs_embedded_recorder.rs`.

### BUG-009: Zero-resolution crash when game window dimensions = 0×0
- **Symptom**: OBS panicked with "invalid parameter" when game hadn't finished creating its swap chain.
- **Fix**: Fallback to `RECORDING_WIDTH × RECORDING_HEIGHT` (1920×1080) when source dims are zero.
- **File**: `src/record/obs_embedded_recorder.rs`.

### BUG-010: Aspect-ratio distortion (output forced to 1920×1080 regardless of source)
- **Symptom**: "比例不对 出来3个文件夹" — game recorded at wrong aspect ratio.
- **Fix**: Output resolution now matches source resolution (no forced scaling).
- **File**: `src/record/obs_embedded_recorder.rs`.

### BUG-011: "Application was never hooked" aborted recordings that otherwise worked
- **Symptom**: Monitor capture worked but recording was marked invalid because game hook didn't fire (which is expected for many games).
- **Fix**: Removed `bail!("Application was never hooked")`. Recordings now accepted without hook as long as video data was written.
- **File**: `src/record/obs_embedded_recorder.rs`.

---

## 4. Game Detection / State Machine Bugs

### BUG-012: Game not detected until foregrounded
- **Symptom**: User alt-tabbed around before launching game → recorder missed game start.
- **Fix**: Added `find_running_game()` process-scan of **all running processes** via `CreateToolhelp32Snapshot`, not just the foreground window.
- **File**: `src/record/recorder.rs`.

### BUG-013: PlayGTAV.exe launcher recorded instead of GTA V
- **Symptom**: User had 3 session folders — two for the Rockstar launcher, one (partial) for the game.
- **Root cause**: `playgtav` was in `GAME_WHITELIST` (treated as a target game).
- **Fix**: Removed `playgtav` from whitelist; added `playgtav.exe`, `rockstarerrorhandler.exe`, `launcher.exe` to `SELF_AND_SYSTEM_BLACKLIST`.
- **Files**: `crates/constants/src/lib.rs`, `src/record/recorder.rs`.

### BUG-014: "Recording started before user entered game"
- **Symptom**: "还是没进游戏就开始录了 还是不能手动开始或者关" — recording started while still on main menu / loading screen.
- **Root cause**: Recording trigger was `game_process_detected` only; no idle/inactivity gating.
- **Mitigation**: Extended `MAX_IDLE_DURATION` from 30s → 300s so menus/cutscenes don't pause recording. Manual F9 toggle added for user control.
- **File**: `crates/constants/src/lib.rs`, `src/tokio_thread.rs`.

### BUG-015: Idle timeout too aggressive — recording stopped during game loading
- **Symptom**: 30-second idle timer stopped recording during 60+ second loading screens.
- **Fix**: `MAX_IDLE_DURATION` = 30s → 300s (5 min).

### BUG-016: Window-focus-loss paused recording for momentary alt-tab
- **Symptom**: Brief alt-tab (e.g. checking Discord) stopped the recording prematurely.
- **Fix**: Focus-loss no longer pauses; only process-exit does.

### BUG-017: `actively_recording_window` never cleared after stop
- **Symptom**: Post-stop, stale HWND reference could resurrect old recording state.
- **Fix**: Clear `actively_recording_window` in stop path.

---

## 5. Hotkey / Configuration Bugs

### BUG-018: F5 hotkey didn't work for test user
- **Symptom**: "F5 热键应该能用——你试过按 F5 吗？ 按了没反应"
- **Root cause**: F5 is reserved by many games (Save State / Quick Save).
- **Fix**: Changed default hotkey F5 → F9. Added auto-migration for old configs (`F5` → `F9` on load).
- **File**: `src/config.rs`.

### BUG-019: Corrupted config JSON crashed startup
- **Symptom**: Malformed `config.json` (from power loss mid-write) prevented app start.
- **Fix**: Graceful fallback — log warning, load defaults, attempt atomic re-save.
- **File**: `src/config.rs`.

### BUG-020: Config save wasn't atomic (power-loss corruption risk)
- **Fix**: Added atomic write pattern (write to `.tmp`, then rename).
- **File**: `src/config.rs`.

### BUG-021: Default recording path `./data_dump/games` broke on non-installed runs
- **Fix**: Default is now `dirs::data_local_dir()/gamedata-recorder/sessions`.
- **File**: `src/config.rs`.

### BUG-022: `use_window_capture` defaulted to `false` but code relied on it being `true`
- **Fix**: Default changed to `true` (though monitor capture is the new default).

---

## 6. Audio Device Requirement (Crash on Nucbox)

### BUG-023: App crashed on machines without audio device
- **Symptom**: Nucbox (headless Windows NUC) crashed on startup because rodio failed to init an audio stream — entire app exited.
- **Root cause**: `Sink` was mandatory; recording pipeline required it for cue playback.
- **Fix (v2.5.1)**: Changed `Sink` → `Option<Sink>`. All call sites use `self.sink.as_ref().map(|s| (s, honk, &*self.app_state))` to skip audio cues gracefully when no device is present.
- **Files**: `src/tokio_thread.rs`, `Cargo.toml` (rodio made optional dep path).

### BUG-024: `Arc<AppState>` passed where `&AppState` expected (v2.5.1 compile error)
- **Symptom**: 5× `E0308` type mismatches in `tokio_thread.rs` (lines 1287, 1315, 1340, 1404, 1412) during the BUG-023 fix.
- **Fix**: `&self.app_state` → `&*self.app_state` at all notification call sites (Arc auto-deref).

---

## 7. Validation / Format Bugs

### BUG-025: CSV validation crashed on JSONL input
- **Symptom**: Post-recording validation threw a parse error when input was in the new JSON Lines format.
- **Fix**: Auto-detect JSONL vs legacy CSV by inspecting first non-empty line for `{`. Skip malformed CSV lines instead of failing whole recording.
- **File**: `src/validation/mod.rs`.

### BUG-026: Input-activity warnings flagged otherwise-valid low-input recordings
- **Symptom**: Exploration / cutscene-heavy gameplay marked `INVALID`.
- **Fix**: Input activity checks changed from `invalid_reasons.push(...)` to `tracing::warn!`. Video content is primary signal; low input is acceptable.
- **File**: `src/validation/mod.rs`.

### BUG-027: Metadata file corruption risk on OS OOM
- **Symptom**: If system ran out of memory mid-write, metadata could be wiped.
- **Fix**: Atomic write pattern — write to `.tmp`, then rename.
- **File**: `src/validation/mod.rs`.

---

## 8. Asset / UI Robustness

### BUG-028: Missing `assets/owl-logo.png` crashed startup
- **Symptom**: Running from a dir without `assets/` subfolder caused panic.
- **Fix**: Multi-path search (`cwd/assets/` + `exe_dir/assets/`) + 1×1 red-pixel fallback icon via `winit::window::Icon::from_rgba`.
- **File**: `src/assets.rs`, `src/ui/mod.rs`.

### BUG-029: Tray tooltip said "OWL Control" (pre-rebrand)
- **Fix**: Tooltip now says "GameData Recorder — F9 to record".

### BUG-030: No "Open Recordings" action in tray
- **Fix**: Added menu item to open the session directory in Explorer.

---

## 9. Encoder / Hardware Compatibility

### BUG-031: User with RTX 4060 forced onto x264 (software) encoder
- **Symptom**: GPU-capable user stuck at software encoding, with heavy CPU load.
- **Partial fix**: NVENC preferred in encoder selection chain. Still investigating why auto-detection missed the RTX 4060 on that machine.
- **File**: `src/record/obs_embedded_recorder.rs`.

### BUG-032: HOOK_TIMEOUT too short (5s) for slow-loading games
- **Fix**: Extended to 15s.
- **File**: `crates/constants/src/lib.rs`.

### BUG-033: MIN_AVERAGE_FPS = 27 rejected 30fps recordings that dropped a few frames
- **Fix**: Lowered to 5 — we keep training-useful data even at low FPS.

### BUG-034: MAX_FOOTAGE = 10 min split long sessions unnecessarily
- **Fix**: Extended to 30 min.

---

## 10. Build / CI Issues

### BUG-035: Repeated `cargo fmt` CI failures after rapid fixes
- **Symptom**: Multiple commits failed CI with formatting nits (lines >100 chars, import order).
- **Mitigation**: Added `cargo fmt` to pre-commit convention; still not enforced in hook.

### BUG-036: Autoresearch pushed 63 junk files (`CLAUDE.md`, `.research/`, `.oystercode/`)
- **Symptom**: Autonomous red-team run committed its own research artifacts to the repo.
- **Fix**: Deleted all junk files; added `.research/`, `.oystercode/` to `.gitignore` (pending).

### BUG-037: SSH to nucbox spawned visible cmd.exe windows on Howard's desktop
- **Symptom**: "我现在 还看见的你cmd" — CMD popups during remote ops.
- **Fix**: Migrated all automated commands to WSL-only SSH path (port 22 → WSL service), silent.

---

## Summary

| Category | Count | Status |
|---|---|---|
| Memory / OOM | 2 | ✅ Fixed |
| Focus stealing | 4 | ✅ Fixed |
| Black screen / capture | 5 | ✅ Fixed |
| Game detection | 6 | ✅ Fixed |
| Hotkey / config | 5 | ✅ Fixed |
| Audio requirement | 2 | ✅ Fixed (v2.5.1) |
| Validation | 3 | ✅ Fixed |
| Asset / UI | 3 | ✅ Fixed |
| Encoder / hardware | 4 | ⚠️ Partial (NVENC detection TBD) |
| Build / CI | 3 | ⚠️ Mitigated |
| **Total** | **37** | **35 fixed, 2 partial** |

---

## Remaining Known Issues (v2.5.1)

1. **NVENC auto-detection** — RTX 4060 users may still fall through to x264. Need to verify on nucbox hardware.
2. **Pre-game recording trigger** — Recorder may still start before user fully enters game (main menu / launcher). Mitigated by F9 manual toggle, but not eliminated.
3. **Frames.jsonl not yet emitted** — Per-frame timestamp log planned (competitor parity).
4. **verify.py equivalent** — Standalone validation CLI not yet implemented.

---

*Last updated: v2.5.1 release (2026-04-19)*

---

# Addendum — v2.5.2 → v2.5.7 (2026-04-19)

After v2.5.1 shipped we ran a 50-round parallel audit (see `MEGA_AUDIT.md`) that surfaced ~110 unique defects. 5 client-blocking ones landed in v2.5.5 (Gate A), and a further 9-agent batch landed in v2.5.7 (Gate B). This addendum tracks them.

## Gate A — v2.5.5 (shipped 2026-04-19)

### BUG-038: `find_window_for_pid(_pid)` ignored its parameter
- **Symptom**: Recording captured the desktop / Rockstar launcher instead of the actual game window.
- **Root cause**: `find_window_for_pid` returned `GetForegroundWindow()` regardless of the PID argument, so any focus change mid-startup (launcher → game) lost the target.
- **Fix (v2.5.4)**: Real `EnumWindows` callback with PID filter + largest-visible-window heuristic; launcher-title blocklist + self-PID guard.
- **File**: `src/record/recorder.rs`.

### BUG-039: JSONL → CSV reconstruction zeroed `input_stats`
- **Symptom**: Every recording's metadata reported `mouse_moves: 0, key_presses: 0, ...` regardless of actual input activity.
- **Root cause**: Validation re-serialized JSONL into CSV then parsed back, splitting `"MOUSE_MOVE,[10,5]"` on the comma inside the JSON array → `event_args = "5]"` (invalid JSON) → silent drop.
- **Fix (v2.5.5)**: Parse JSONL directly via `parse_jsonl_event`, skip the CSV round-trip.
- **File**: `src/validation/mod.rs`.

### BUG-040: 200ms `thread::sleep` in stop path + mpsc capacity 10 dropped input
- **Symptom**: Stop-recording lost 100+ keyboard/mouse events at the tail of every session.
- **Root cause**: Tokio runtime stalled on a 200ms sleep waiting for OBS skipped-frames log; meanwhile the Win32 raw-input thread `blocking_send`-ed into a 10-slot channel that backed up and overflowed.
- **Fix (v2.5.5)**: mpsc capacity 10 → 10_000; sleep replaced with `tokio::sync::Notify` signaled by OBS log.
- **Files**: `crates/input-capture/src/lib.rs`, `src/record/obs_embedded_recorder.rs`.

### BUG-041: ANSI `PROCESSENTRY32` blind to Chinese locale
- **Symptom**: Chinese Windows users (华硕 client) had the recorder silently skip game processes whose paths contained non-ASCII characters.
- **Fix (v2.5.5)**: Migrated to W-suffix wide APIs (`QueryFullProcessImageNameW`, `PROCESSENTRY32W`).
- **File**: `crates/game-process/src/lib.rs`.

### BUG-042: `wmic` NVENC probe missing on Win11 22H2+ / N / LTSC
- **Symptom**: NVIDIA users on newer/stripped SKUs silently fell back to x264 software encoding.
- **Root cause**: `wmic.exe` deprecated on Win11 22H2+, absent on Windows N / LTSC / Group-Policy-hardened installs; shell-out swallowed the error.
- **Fix (v2.5.5)**: Direct DXGI adapter enumeration via `wgpu::Instance::enumerate_adapters(DX12)` and `PCI_VENDOR == 0x10DE`.
- **File**: `src/config.rs`.

## Gate B — v2.5.7 (shipped 2026-04-19, 9-agent batch)

### BUG-043 (R47): Kernel-anti-cheat titles in supported-games whitelist
- **Symptom**: Testers recording Escape from Tarkov / Halo Infinite / Hell Let Loose / Arma 3 / CoD: Vanguard / Valorant / LoL would trip BattlEye / EAC kernel / Ricochet / Vanguard and risk HWID bans.
- **Root cause**: Upstream OWL Control whitelisted all popular titles without considering anti-cheat architecture. Our `OpenProcess(PROCESS_QUERY_INFORMATION)` + `TH32CS_SNAPMODULE` is a documented ban vector against kernel AC.
- **Fix (v2.5.7)**: Removed 7 kernel-AC titles from `GAME_WHITELIST`, added `supported_games.README.md` documenting policy (no kernel-AC titles ever).
- **File**: `crates/constants/src/supported_games.json`, `crates/constants/src/lib.rs`.

### BUG-044 (R46): Consent gate bypassed — GDPR/CCPA exposure
- **Symptom**: Raw Input hooks + OBS capture installed before any consent UI was shown. Disclosure said "during gameplay" but we captured system-wide keyboard/mouse from startup.
- **Root cause**: ConsentView routing was commented out in `src/ui/views/mod.rs:321` during an earlier refactor.
- **Fix (v2.5.7)**: `ConsentGuard` type threaded through `InputCapture::new`, `VideoRecorder::start_recording`, `Recorder::start`. `ConsentStatus::Granted` required before any hook install. Semver-bumped consent invalidates stored acceptance. Disclosure rewritten to accurately state monitor-wide video + global keyboard/mouse.
- **Files**: `src/ui/views/mod.rs`, `src/config.rs`, `crates/input-capture/src/{lib,kbm_capture}.rs`, `src/record/{recorder,recording,obs_embedded_recorder,obs_socket_recorder}.rs`, `src/tokio_thread.rs`, `src/ui/consent.md`.

### BUG-045: Silent audio in monitor-capture recordings
- **Symptom**: When monitor-capture (default since v2.5.4) was used, MP4s had no audio track.
- **Root cause**: OBS scene for monitor-capture had no audio source attached. Game-capture (hook) mode worked because OBS's hook injects its own audio tap.
- **Fix (v2.5.7)**: `wasapi_output_capture` on channel 1 (desktop audio); optional `wasapi_input_capture` on channel 2 (microphone, default OFF for privacy). Clear + detach on stop to avoid dangling refs.
- **File**: `src/record/obs_embedded_recorder.rs`, `src/config.rs` (`record_microphone: bool` preference).

### BUG-046: Metadata could reference an unflushed MP4 on crash
- **Symptom**: Power-loss mid-stop left MP4 without moov atom finalized, while `metadata.json` referenced it as if valid.
- **Fix (v2.5.7)**: New `src/util/durable_write.rs` helper (write-tmp → fsync → rename → dir-sync). 16 final-path writes converted. MP4 `File::sync_all()` in a `spawn_blocking` task between OBS stop and metadata write.
- **Files**: `src/util/durable_write.rs` (new), `src/validation/mod.rs`, `src/record/local_recording.rs`, `src/record/recording.rs`, `src/record/fps_logger.rs`, `src/record/metadata_writer.rs`, `src/record/session_manager.rs`, `src/play_time.rs`, `src/config.rs`.

### BUG-047: Three Windows-specific attack classes open at startup
- **DLL hijack**: no `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` at startup; a planted DLL in the app folder could preempt System32.
- **Symlink attack**: `recording_location` was user-configurable with no validation; a malicious symlink to `C:\Windows\System32` could turn a "safe cleanup" into a system-file wipe.
- **Plaintext API key**: `config.json` stored `api_key` as plaintext — anyone exfil-ing the file had login.
- **Fix (v2.5.7)**: DLL search lockdown top of `main()`. `validate_recording_location` wired at load/pick/confirm. DPAPI-wrap `api_key` via custom Serialize/Deserialize; legacy plaintext auto-migrates on first save.
- **Files**: `src/main.rs`, `src/config.rs`, `Cargo.toml`.
- **Migration note**: Users with `recording_location` outside `%LocalAppData%` are silently reset to default on first launch.

### BUG-048: OBS shutdown stalled on 200ms sleep + recorder Drop blocked forever
- **Symptom**: Stop-recording tail path stalled tokio; recorder thread Drop could hang process shutdown indefinitely on OBS bug.
- **Fix (v2.5.7)**: `tokio::sync::Notify` + 3s timeout for OBS log-line wait. `JoinHandle::is_finished()` poll loop with 3s deadline in Drop; abandon + warn after timeout. `LemInputStream::stop` retry loop with deadline.
- **File**: `src/record/obs_embedded_recorder.rs`, `src/record/lem_input_recorder.rs`.

### BUG-049: Five hardcoded metadata stubs polluting AI training data
- **Symptom**: Every recording's metadata reported the same `"NVMe SSD"` / `"Unknown"` GPU / FOV `90` / wrong CPU-core count / heartbeat-approximated FPS — regardless of actual hardware.
- **Fix (v2.5.7)**: `src/system/disk_type.rs` detects via `GetDriveTypeW` + `IOCTL_STORAGE_QUERY_PROPERTY` (NVMe / SATA SSD / SATA HDD / USB). GPU from DXGI adapter list already enumerated at startup. `FrameStats` parses OBS's `X/TOTAL` skipped-frames counter (real FPS). CPU/RAM from `sysinfo`. FOV changed `u32 → Option<f32>` (analysts were ignoring the stub anyway).
- **Files**: `src/system/disk_type.rs` (new), `src/record/metadata_writer.rs`, `src/output_types/lem_metadata.rs`.

### BUG-050: Session folder collision on second-boundary restarts
- **Symptom**: Two recordings starting within the same wall-clock second overwrote each other's folders.
- **Fix (v2.5.7)**: UUIDv4 8-hex suffix on session folder names: `session_YYYYMMDD_HHMMSS_<xxxxxxxx>`. Parser handles new + old + legacy formats.
- **Files**: `src/tokio_thread.rs` (`generate_session_dir_name`), `src/record/session_manager.rs` (`generate_session_id`), `src/record/local_recording.rs` (`parse_session_timestamp`).

### BUG-051: Upload queue race on stop-recording
- **Symptom**: Rapid stop events could double-enqueue the same session to the upload worker. S3 idempotency saved us from corruption but we burned bandwidth and backend could flag us as a duplicate-upload actor.
- **Fix (v2.5.7)**: Replaced `RwLock<Vec<T>>` with `tokio::sync::mpsc::UnboundedSender<UploadTrigger>`. Worker task owns receiver + `HashSet<PathBuf>` dedup. Enqueue is now branch-free.
- **Files**: `src/upload/mod.rs`, `src/app_state.rs`, `src/main.rs`, `src/tokio_thread.rs`, `src/record/recorder.rs`.

### BUG-052 (R33): Hotkey `RwLock<bool>` TOCTOU
- **Symptom**: Two rapid "rebind hotkey" clicks could both succeed and one would overwrite the other's target.
- **Fix (v2.5.7)**: `AtomicListeningForNewHotkey` wrapper over `AtomicU32` (2-bit tag + target + 16-bit captured key). `begin_listening()` uses `compare_exchange` → lost race returns false. 32-thread Barrier test proves single winner.
- **File**: `src/app_state.rs`.

### BUG-053: DPI-virtualized monitor capture
- **Symptom**: On 125%/150% DPI displays Windows reported a scaled resolution to our unaware process; we captured at 1536×864 instead of 1920×1080 and input coordinates were off.
- **Fix (v2.5.7)**: `embed-manifest` DPI awareness set to `PerMonitorV2` (Win10 1607+) + `true/pm` legacy fallback. Monitor DPI scale logged at recording start for debugging.
- **Files**: `build.rs`, `src/record/obs_embedded_recorder.rs`.

### BUG-054: CI `windows::Win32::Foundation::BOOL` regression (build-breaking)
- **Symptom**: Every commit since v2.5.4 had red CI. `BOOL` was relocated to `windows::core::BOOL` in `windows` crate 0.60+. Local nucbox build resolved differently and hid the error.
- **Fix (v2.5.6 intermediate)**: Changed import path; callback ABI unchanged.
- **File**: `src/record/recorder.rs`.

---

## Deferred to v2.5.8

- **BUG-055 (R-DXGI): workstation lock (Win+L / RDP) crashes monitor-capture recording** — `DXGI_ERROR_ACCESS_LOST` should pause OBS output + poll for session return, resume on success, give up at 5 min. Agent delivered the patch but it had 5-way conflicts with audio + shutdown + consent PRs in `obs_embedded_recorder.rs`; rebasing fresh against v2.5.7 main.

## Still in Gate C (v3.0 / public rollout)

See `TRIAGE.md` for the full list. Highlights: content attestation (perceptual hash + server-side frame fingerprint), HID-replay detection, authenticode signing of the recorder binary, per-title anti-cheat warning before recording, sensitive-window blocklist (password managers, banking), alt-tab pause mode, monitor-capture overlay masking.

---

*Last updated: v2.5.7 release (2026-04-19)*
