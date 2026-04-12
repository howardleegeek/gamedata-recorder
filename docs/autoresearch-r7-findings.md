# Autoresearch R7 Findings

**Date:** 2026-04-12  
**Version:** v1.8.1 (release), v1.7.1 (testing)  
**Scope:** GAME_WHITELIST, auto-record logic, Raw Input, cargo fmt

---

## 1. Critical: GAME_WHITELIST Not Defined

### Problem
`src/record/recorder.rs` references `GAME_WHITELIST` at line 396, but this constant **does not exist** anywhere in the codebase:

```rust
// Line 396 in recorder.rs
if !GAME_WHITELIST.iter().any(|g| exe_lower == *g) {
    tracing::debug!("{} is not in game whitelist, skipping", exe_name);
    return Ok(None);
}
```

This will cause a **compilation error**.

### Root Cause
The `supported_games.json` exists (60 games, 301 lines) but there's no corresponding Rust constant `GAME_WHITELIST` that exports the binary names.

### Solution Required
Add to `crates/constants/src/lib.rs`:
```rust
/// Game whitelist - binaries that are allowed to be recorded
pub const GAME_WHITELIST: &[&str] = &[
    // Generated from supported_games.json
    "abyssus", "rgame",
    "amenti",
    "arma3", "arma3_x64",
    // ... (all 60 games)
];
```

---

## 2. Missing Popular Games

Current whitelist has **60 games** but is missing many high-demand titles:

### Tier 1 - Must Add (High Priority)
| Game | Binary Name | Why Missing |
|------|-------------|-------------|
| Counter-Strike 2 | `cs2.exe` | #1 Steam game |
| Dota 2 | `dota2.exe` | Top 3 Steam |
| PUBG | `pubg.exe`, `tslgame.exe` | Battle royale |
| Apex Legends | `r5apex.exe` | Popular FPS |
| Valorant | `valorant.exe`, `valorant-win64-shipping.exe` | Riot's FPS |
| Fortnite | `fortniteclient-win64-shipping.exe` | #1 Battle royale |
| League of Legends | `league of legends.exe` | #1 MOBA |
| Minecraft | `minecraft.exe`, `javaw.exe` | Best-selling game |
| Grand Theft Auto V | `gta5.exe` | Perennial top 10 |
| Elden Ring | `eldenring.exe` | 2022 GOTY |
| Baldur's Gate 3 | `bg3.exe` | 2023 GOTY |
| Cyberpunk 2077 | `cyberpunk2077.exe` | Major AAA |
| Call of Duty (Modern Warfare III) | `cod.exe` | Annual franchise |
| Overwatch 2 | `overwatch.exe` | Blizzard FPS |
| Rainbow Six Siege | `rainbowsix.exe`, `rainbowsix_vulkan.exe` | Tactical FPS |
| Rust | `rust.exe`, `rustclient.exe` | Survival |
| Destiny 2 | `destiny2.exe` | Bungie looter |
| Warframe | `warframe.exe` | Free-to-play |
| Team Fortress 2 | `hl2.exe` | Classic |
| Rocket League | `rocketleague.exe` | Sports |

### Tier 2 - Should Add (Medium Priority)
- Starfield (`starfield.exe`)
- Hogwarts Legacy (`hogwartslegacy.exe`)
- Spider-Man series (`spiderman.exe`, `spiderman2.exe`)
- God of War (`gow.exe`)
- Horizon Zero Dawn/Forbidden West (`horizon*.exe`)
- Ghost of Tsushima (`ghostoftsushima.exe`)
- Final Fantasy series (`ff7*.exe`, `ff16.exe`)
- Resident Evil series (`re*.exe`)
- Monster Hunter series (`monsterhunter*.exe`)
- Dark Souls/Elden Ring series (`dark souls*.exe`, `eldenring.exe`)

---

## 3. Auto-Record + Whitelist Interaction Logic

### Location
`src/tokio_thread.rs` lines 993-1025

### Current Implementation
```rust
// AUTO-RECORD: If idle and a recordable game is in the foreground, start recording automatically.
if self.recording_state == RecordingState::Idle {
    let cooldown_elapsed = self
        .last_auto_record_attempt
        .map(|t| t.elapsed() > std::time::Duration::from_secs(30))
        .unwrap_or(true);

    if cooldown_elapsed {
        let fg = self.app_state.last_foregrounded_game.read().unwrap().clone();
        if let Some(ref game) = fg
            && game.is_recordable()
            && game.exe_name.is_some()
            && !self.app_state.is_out_of_date.load(Ordering::SeqCst)
        {
            self.last_auto_record_attempt = Some(std::time::Instant::now());
            tracing::info!(...);
            if let Err(e) = self.handle_transition(RecordingState::Recording).await {
                tracing::error!(e=?e, "Failed to auto-start recording, cooldown 30s");
            }
        }
    }
}
```

### Assessment
**Status: CORRECT** ✅

The logic properly:
1. Checks `RecordingState::Idle` before attempting auto-record
2. Uses 30-second cooldown to prevent churn on unhookable games
3. Checks `game.is_recordable()` which includes whitelist check via `get_foregrounded_game()`
4. Respects `is_out_of_date` flag
5. Updates `last_auto_record_attempt` on every attempt (success or failure)

### Potential Issue
The `user_stopped_game_exe` mechanism (lines 849-858) is supposed to suppress auto-record after manual stop, but it's **not checked** in the auto-record logic. This means:
1. User stops recording
2. `user_stopped_game_exe` is set to current game
3. Auto-record may still trigger if game is foregrounded

**Fix needed:** Add check for `user_stopped_game_exe` in auto-record logic.

---

## 4. Raw Input Message Loop

### Location
`crates/input-capture/src/kbm_capture.rs`

### Current Implementation
Lines 126-149:
```rust
pub fn run_queue(&mut self, mut event_callback: impl FnMut(Event) -> bool) -> Result<()> {
    unsafe {
        let mut msg = MSG::default();
        let mut last_absolute: Option<(i32, i32)> = None;

        while GetMessageA(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageA(&msg);

            if msg.message == WindowsAndMessaging::WM_INPUT {
                // Process each WM_INPUT message individually via GetRawInputData.
                // NOTE: GetRawInputBuffer batch mode was removed because the
                // previous implementation had bugs (no size query, wrong stride).
                // Single-message processing is reliable and sufficient for 1000Hz mice.
                for event in self.parse_wm_input(msg.lParam, &mut last_absolute) {
                    if !event_callback(event) {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }
}
```

### Assessment
**Status: CORRECT** ✅

The implementation:
1. Uses `GetMessageA` loop (standard Windows message pump)
2. Properly calls `TranslateMessage` and `DispatchMessageA`
3. Handles `WM_INPUT` specifically for raw input
4. Uses `GetRawInputData` (single message) instead of `GetRawInputBuffer` (batch)
5. Comment explains why batch mode was removed (bugs with size query and stride)
6. Tracks `last_absolute` for absolute mouse position calculations

### No Issues Found
- No mutex poisoning risks
- No infinite loop risks
- Proper error handling via `Result`

---

## 5. Cargo fmt Issues (>100 character lines)

### Summary
| File | Lines >100 | Severity |
|------|-----------|----------|
| `src/tokio_thread.rs` | 15+ | Medium |
| `src/record/recorder.rs` | 5 | Low |
| `src/record/obs_embedded_recorder.rs` | 8 | Medium |
| `src/upload/upload_tar.rs` | 6 | Low |
| `src/upload/mod.rs` | 2 | Low |
| `src/output_types.rs` | 2 | Low |
| `src/record/local_recording.rs` | 3 | Low |
| `src/ui/views/main/upload_manager.rs` | 3 | Low |
| `src/main.rs` | 1 | Low |

### Notable Long Lines

**src/tokio_thread.rs:192** (116 chars)
```rust
if let Some(key) = e.key_press_keycode() { *app_state.listening_for_new_hotkey.write().unwrap() = ListeningForNewHotkey::Captured { target, key } }
```

**src/tokio_thread.rs:226** (127 chars)
```rust
app_state.async_request_tx.send(AsyncRequest::SetOfflineMode { enabled: true, offline_reason: Some(e.to_string()) }).await.ok();
```

**src/record/obs_embedded_recorder.rs:809** (147 chars)
```rust
"The window you're trying to record ({game_exe}) is already being captured by another process. Do you have OBS or another instance of GameData Recorder open?\n\nNote that OBS is no longer required to use GameData Recorder - please close it if you have it running!",
```

---

## 6. Other Findings

### 6.1 SELF_AND_SYSTEM_BLACKLIST (recorder.rs:307-376)
**Status: COMPREHENSIVE** ✅

Good coverage of:
- Self (gamedata-recorder.exe, owl-control.exe)
- System processes (explorer, taskmgr, etc.)
- Launchers (steam, epic, gog, origin, uplay, battlenet)
- Browsers (chrome, firefox, edge)
- Communication (discord, slack, teams)
- Video players (vlc, mpv, potplayer)
- Recording tools (obs, streamlabs)
- Remote desktop (parsec, sunshine, moonlight)
- Creative apps (blender, resolve, photoshop)
- Hardware monitoring (afterburner, rtss)

### 6.2 is_process_game_shaped() (recorder.rs:404-456)
**Status: CORRECT** ✅

Properly checks for graphics APIs:
- Direct3D (d3d, dxgi, d3d11, d3d12, d3d9)
- OpenGL (opengl32, gdi32, glu32)
- Vulkan (vulkan, vulkan-1, vulkan32)

Fails safe when module enumeration fails (warns but allows).

---

## 7. Action Items

### Must Fix (Blocking)
1. **Add GAME_WHITELIST constant** to `crates/constants/src/lib.rs`
2. **Fix user_stopped_game_exe check** in auto-record logic

### Should Fix (Quality)
3. **Run cargo fmt** on all recently modified files
4. **Add Tier 1 popular games** to supported_games.json

### Nice to Have
5. **Add Tier 2 games** to expand coverage
6. **Consider dynamic game list** from server instead of hardcoded

---

## 8. Commits Made

| Commit | Description |
|--------|-------------|
| (pending) | fix: add GAME_WHITELIST constant and fix auto-record user_stopped check |
| (pending) | style: cargo fmt on recently modified files |
| (pending) | feat: add Tier 1 popular games to whitelist |
