# supported_games.json — Anti-Cheat Policy

This file documents the whitelist policy for `supported_games.json` and the
mirrored `GAME_WHITELIST` constant in `lib.rs`. **Read this before adding any
game to the list.**

## Policy (R47, non-negotiable)

> **No game that ships a kernel-mode anti-cheat driver may be added to this
> whitelist — ever.**

GameData Recorder uses these Windows calls against every detected game process:

- `OpenProcess(PROCESS_QUERY_INFORMATION, ...)` — to read PID/EXE metadata
- `CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, ...)` — to enumerate modules
- Foreground-window polling via `GetForegroundWindow` + `GetWindowThreadProcessId`

For games with a **kernel-mode anti-cheat driver** in the game's process space,
the above pattern is a well-documented ban vector — kernel-AC drivers flag this
as third-party tampering and issue **HWID bans** (hardware fingerprint bans that
survive reinstalls and account resets). Our testers are using their personal
machines; a single HWID ban can cost them access to every game guarded by that
anti-cheat for life.

This is not a theoretical risk. It is the failure mode.

## Kernel-mode anti-cheats that MUST stay off the whitelist

These are the drivers most likely to ban our testers. The list is not
exhaustive — when in doubt, assume kernel-mode and leave the game off.

| Anti-cheat | Notes |
|---|---|
| **BattlEye (kernel driver mode)** | `BEDaisy.sys` / `BEService.exe`. Loaded by Tarkov, PUBG, R6 Siege, DayZ, ARMA, etc. Detects external process handles. |
| **Easy Anti-Cheat (kernel driver mode)** | `EasyAntiCheat.sys` / `EasyAntiCheat_EOS.sys`. Loaded by Halo Infinite, Hell Let Loose, Apex, Fortnite (some modes), Elden Ring (online), etc. |
| **Riot Vanguard** | `vgk.sys` / `vgc.exe`. Loads at Windows boot. Required by Valorant and now by the League of Legends client in-game. One of the most aggressive kernel AC in circulation. |
| **FACEIT Anti-Cheat** | `faceitclient.sys`. Installed alongside FACEIT client for CS2/CS:GO tournament play. |
| **Xigncode3 (kernel mode)** | `x3.xem` / kernel module. Used by various Asian F2P titles. |
| **Ricochet** | Activision's kernel driver (Modern Warfare 2019+, Warzone, Vanguard, MW2/MW3/BO6). |
| **nProtect GameGuard (kernel mode)** | `GameMon.des`. Korean MMO staple. |
| **Hyperion** | Roblox's kernel-level AC (since 2023). |

**Decision rule:** If you can find *any* published reference that a game's
anti-cheat uses a `.sys` kernel driver, it does not belong here.

## User-mode anti-cheats — acceptable but warrant a per-title warning

User-mode-only anti-cheats are acceptable **but every such title should warrant
a per-title warning before recording starts**, because the behavior can change
between patches (a user-mode AC vendor can always ship a kernel driver in a
future update, and several have). Treat this bucket as "allowed for now,
re-verify quarterly."

| Anti-cheat | Notes |
|---|---|
| **VAC (Valve Anti-Cheat)** | Pure user-mode, signature- and heuristics-based. CS2, TF2, L4D2, Dota 2. |
| **FairFight** | Server-side telemetry only. No client driver. |
| **BattlEye (non-kernel / user-mode mode)** | Some titles run BE in user-mode only. Verify before trusting. |
| **Easy Anti-Cheat (non-kernel / user-mode mode)** | Some titles (and EOS integrations) run EAC in user-mode. Verify before trusting. |

## Process for adding a new game

1. Identify the game's anti-cheat vendor (check SteamDB's "DRM notices" field,
   the game's own support page, and community reports on r/antitamper).
2. If the anti-cheat is in the kernel-mode table above, **stop. Do not add it.**
3. If the anti-cheat is user-mode, add the entry and open an issue to wire up
   a per-title warning dialog before recording starts.
4. If the game has **no anti-cheat**, add it freely.

## Audit history

| Date | Ref | Change |
|---|---|---|
| 2026-04-19 | R47 | Removed Escape from Tarkov (BattlEye kernel), Halo: Infinite (EAC kernel), Hell Let Loose (EAC kernel), ARMA 3 (BattlEye) from JSON and `lib.rs::GAME_WHITELIST`. Removed Valorant + League of Legends from `GAME_WHITELIST` top-requested section (Riot Vanguard). Created this README. |
