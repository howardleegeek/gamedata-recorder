# Multi-Game Capture — Architecture Roadmap

**Context:** The v2.5.11 capture path is a single mode (OBS `duplicator-monitor-capture`) that works for windowed/borderless games (test_game, most indie D3D11 titles) but fails silently on fullscreen-exclusive D3D12 games (CS2 on AMD integrated, many AAA titles). The in-flight PR adds a second mode (game-capture hook via `win-capture.dll`) driven by a static list of known fullscreen-exclusive titles.

**Howard's direction (2026-04-20):** "我们以后要支持很多类型的游戏的" — we'll support many types of games. The static hardcoded list is a temporary crutch; the real architecture must grow.

This document is the plan for getting from "two hardcoded modes" to a system that handles any game the client throws at us.

---

## Capture modes we'll need

| Mode | OBS source | Works on | Fails on |
|------|-----------|----------|----------|
| **Monitor duplication** | `monitor_capture` / `duplicator-monitor-capture` | Windowed, borderless-windowed | Exclusive fullscreen on many AMD drivers; HDR swapchain on some NVIDIA games |
| **Game-capture hook** | `game_capture` (win-capture plugin) | Exclusive fullscreen, HDR, variable-size windows | Anti-cheat that kernel-blocks hooks (BattlEye, EAC kernel, Vanguard — already excluded by R47); some DX12 Frame Generation titles |
| **Window capture** | `window_capture` | Any specific HWND | Games that change HWND at state transitions; any hidden/minimized state |
| **WGC (Windows.Graphics.Capture)** | `wgc_capture` (OBS 31+ bundled) | Win10 1903+: covers most fullscreen-exclusive cases AND is friendlier to HDR than monitor-duplication | Pre-1903, some older integrated GPUs |
| **NVIDIA FBC / AMD AMF** | driver-level capture | High-FPS titles (240Hz esports) | Non-matching GPU vendor |

For 2026: **Monitor + GameHook + WGC** covers ~99% of the games we care about. FBC/AMF are optional next-gen.

---

## The decision engine

Instead of a static `KNOWN_FULLSCREEN_EXCLUSIVE_GAMES` list, we want a **decision engine** that picks the mode dynamically. Rough order of signals:

1. **Per-game config override** (highest priority) — `config.json` has `preferences.games["<exe_stem>"].capture_mode: "monitor" | "game_hook" | "wgc" | "auto"`. If set to anything other than `auto`, use that. This is how QA / ops can pin a game's mode when Auto misdetects.

2. **Learned cache from last successful run** — after the first successful recording of a given game, persist the winning mode in the config. Next run: start with that mode. This converges on the right answer after one try.

3. **Static hint list** (seed) — for the first-ever run of a game, seed Auto's initial guess from `KNOWN_FULLSCREEN_EXCLUSIVE_GAMES` if present, else default to Monitor.

4. **Runtime fallback detection** — after ~5s of recording, probe the output:
   - Sample mean brightness of the last 3 frames via `obs_get_video_frame` or a staging texture read
   - If mean brightness is near zero AND the game has been rendering (we see the process is alive + window is visible), the current mode isn't working
   - Hot-switch to the next mode in the chain: Monitor → GameHook → WGC → give up
   - Cache the successful mode so next run skips the trial

5. **Explicit user override in UI** (long-term) — settings panel lets the player force a mode if they know better.

---

## Data model

```rust
// In crates/constants/src/lib.rs (or src/record/capture_mode.rs — new file)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Engine decides at runtime. Default.
    Auto,
    /// Desktop duplication. Works on windowed/borderless. Default for most games.
    Monitor,
    /// win-capture game_capture source. Works on exclusive fullscreen.
    GameHook,
    /// Windows.Graphics.Capture API. Modern, HDR-friendly. Win10 1903+.
    Wgc,
}

impl Default for CaptureMode {
    fn default() -> Self { Self::Auto }
}

// In src/config.rs GameConfig
pub struct GameConfig {
    pub use_window_capture: bool,   // legacy, being deprecated
    pub capture_mode: CaptureMode,  // NEW — replaces use_window_capture
    pub last_successful_mode: Option<CaptureMode>,  // NEW — learned cache
    pub mode_learned_at: Option<chrono::DateTime<Utc>>, // when the learned value was last confirmed
}
```

---

## Migration from current design

The in-flight PR (`feat(capture): game-capture hook fallback`) ships:
- `CaptureMode { Monitor, GameHook, Auto }` enum (3 modes, not 4 — WGC comes later)
- `GameConfig::capture_mode` field defaulting to `Auto`
- `KNOWN_FULLSCREEN_EXCLUSIVE_GAMES` static list for Auto's initial guess
- No runtime fallback detection (ships in a later PR)

Next PRs (in order):

1. **Dynamic fallback detection** (`feat(capture): runtime mode fallback on black frames`)
   - Frame-brightness probe at T+5s
   - Hot-switch without restarting the MP4 file (needs OBS pause + scene rebuild + unpause; if that's too invasive, restart within same session folder and post-process concat)
   - Persist `last_successful_mode` to `GameConfig`

2. **Learned cache** (`feat(capture): persist successful capture mode per game`)
   - Config-on-disk now carries `last_successful_mode`
   - Auto mode reads that first, then falls back to static hint list, then to Monitor default

3. **WGC source** (`feat(capture): windows.graphics.capture mode for HDR + exclusive`)
   - Add `wgc_capture` via libobs-wrapper (or raw `ObsSourceRef::new` if no builder)
   - Update Auto logic: try WGC first on Win10 1903+, fall back to Monitor/GameHook
   - Test matrix: HDR titles, DX12 games, integrated-GPU edge cases

4. **UI mode override** (`feat(ui): per-game capture mode picker in settings`)
   - egui settings panel lists supported games, per-game dropdown for mode
   - Mostly cosmetic — ops already has config.json edit access

---

## Testing strategy

For every new game the client wants us to support:

1. **Initial profiling session**: run with Auto mode, let the engine log which mode it lands on
2. **Confirmation run**: record a 30s session, verify MP4 is non-black
3. **Add to test matrix**: CI runs all known-good games against all capture modes, flags regressions per-game-per-mode

Candidate test matrix (top 10 games our client wants):

| Game | Expected mode | Why |
|------|---------------|-----|
| GTA V | GameHook | Rockstar uses fullscreen exclusive even in "borderless" |
| CS2 | GameHook | D3D12 exclusive on most hardware |
| Dota 2 | Monitor | Usually windowed-fullscreen, DWM-friendly |
| League of Legends | EXCLUDED (Vanguard kernel AC, R47) | — |
| Valorant | EXCLUDED (same) | — |
| Minecraft | Monitor | OpenGL, composited |
| Fortnite | GameHook | DX12 exclusive |
| Roblox | Monitor | Windowed-fullscreen by default |
| Overwatch 2 | GameHook | DX11 exclusive |
| Apex Legends | EXCLUDED (EAC kernel) | — |

---

## Open questions for the team

1. **Should Auto mode try GameHook FIRST for unknown games?** Tradeoff: faster convergence for most AAAs (which are typically exclusive fullscreen) vs. unnecessary DLL injection into indie games that'd be fine with Monitor.
2. **Frame-brightness probe threshold** — 3/255 may false-positive on games with legitimately dark scenes (horror games in pitch-black rooms). Maybe compare against a short histogram — if >99% of pixels are (0,0,0), it's capture failure, not dark content.
3. **Hot-switch without breaking the MP4 timeline** — OBS Studio itself doesn't support changing source mid-output without breaking the file. libobs-wrapper may let us swap sources in the scene, but we may need to test. Worst case: split into two MP4s and concat server-side.
4. **Anti-cheat compatibility for GameHook** — VAC is fine (user-mode). BattlEye user-mode (Squad, PAYDAY3) is generally fine. EAC user-mode depends on version. We should maintain a `KNOWN_HOOK_HOSTILE_AC` list separate from kernel AC list and refuse GameHook for those.

---

*Last updated: v2.5.11, 2026-04-20*
