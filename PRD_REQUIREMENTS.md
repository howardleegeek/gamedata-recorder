# GameData Recorder — PRD Requirements Checklist

*Source: consolidated from `README.md`, `ONBOARDING.md`, `docs/RECORDER_BUYER_SPEC_FEATURES.md`, `BUGS.md`, `TRIAGE.md`.*
*Last updated: 2026-05-12 · Owner: Howard Li (CEO, Oysterworld Inc.)*

**Priority key**
- **P0** — MVP; ship-blocking
- **P1** — Gate B (≤100 friendly testers)
- **P2** — Gate C (public payout open)

---

## R1. Product Identity

| # | Requirement | Priority |
|---|---|---|
| R1.1 | Single-binary Windows desktop installer, double-click install, zero configuration on first run | P0 |
| R1.2 | Tray-resident background app — main UI minimized after first launch | P0 |
| R1.3 | Auto-update channel (delta patches for PATCH versions, prompt for MINOR) | P1 |
| R1.4 | Authenticode-signed installer (no SmartScreen warning) | P1 |
| R1.5 | MIT-licensed source code published; users can audit every line | P0 |

## R2. Capture — Video

| # | Requirement | Priority |
|---|---|---|
| R2.1 | **Trigger condition**: recording starts only when a whitelisted game process is foreground for ≥3 s | P0 |
| R2.2 | **Stop condition**: recording stops on game exit, alt-tab >5 s, screensaver, or workstation lock | P0 |
| R2.3 | **Capture API**: OBS embedded SDK using `MonitorCaptureSource` (window-relative, multi-monitor aware via `MonitorFromWindow`) | P0 |
| R2.4 | **Fullscreen-exclusive support**: DX12 / Vulkan / D3D11 fullscreen games must capture without black frames | P0 |
| R2.5 | **Resolution**: native game window resolution, **no forced scaling**, no aspect-ratio distortion | P0 |
| R2.6 | **Codec**: H.265/HEVC preferred, H.264 fallback if no HEVC encoder available | P0 |
| R2.7 | **Encoder**: hardware-only (NVENC / AMF / Intel QSV), runtime-detect available adapter | P0 |
| R2.8 | **Framerate**: 30 fps target, may degrade to 24 fps under CPU pressure | P0 |
| R2.9 | **Container**: MP4 with fragmented `moov` (streamable, recoverable from crash) | P0 |
| R2.10 | **Bitrate**: target 8 Mbps ± 2 Mbps at 1080p30 (buyer accepts 6–12 Mbps band) | P0 |
| R2.11 | **Segment length**: uniform `5 ≤ duration ≤ 6 min` per clip (force keyframe at segment boundary) | P0 |
| R2.12 | **Recovery**: recording resumes within 2 s after game crash + restart, monitor sleep/wake, DXGI access loss | P1 |
| R2.13 | **No focus stealing**: zero `MessageBox`/modal popup during active recording; all errors go to `tracing` logs | P0 |
| R2.14 | **Overlay**: any in-app overlay window uses `SW_SHOWNA` (non-activating); never focuses game window | P0 |

## R3. Capture — Input Stream

| # | Requirement | Priority |
|---|---|---|
| R3.1 | **API**: Win32 Raw Input (`RegisterRawInputDevices`), W-suffix only (non-ASCII path support) | P0 |
| R3.2 | **Events captured**: keyboard, mouse (move/click/wheel), gamepad (XInput + DirectInput) | P0 |
| R3.3 | **Format**: JSON Lines (`.jsonl`), one event per line, UTC monotonic timestamp in nanoseconds | P0 |
| R3.4 | **Schema**: `{ "ts": <ns>, "type": "key|mouse|pad", "code": <int>, "action": "down|up|move", "x": <int>, "y": <int>, "modifiers": [...] }` | P0 |
| R3.5 | **Auto-repeat filtering**: held-key repeats MUST be filtered (only `down` once + `up` once); use `(lParam & 0x40000000) != 0` test on `WM_KEYDOWN` | P0 |
| R3.6 | **Backpressure**: bounded mpsc channel (`capacity = 16_384`), `try_send` with graceful drop counter | P0 |
| R3.7 | **Time alignment**: input timestamp clock-aligned with video presentation timestamp (single monotonic source) | P0 |
| R3.8 | **Privacy**: no event recorded while game window not foreground | P0 |
| R3.9 | **PII redaction**: clipboard content excluded; password-field heuristics blocked | P1 |

## R4. Engine Telemetry (Premium 2–4× revenue tier)

| # | Requirement | Priority |
|---|---|---|
| R4.1 | **Schema**: `EngineFrame { ts, pos: [x,y,z,w], rot: [x,y,z,w], fov, world_id }`, one row per video frame, sidecar JSONL | P0 |
| R4.2 | **Schema is buyer-contract**: field names + array order **cannot change without buyer sign-off** | P0 |
| R4.3 | **Cyberpunk 2077 hook**: RED4ext RTTI walk; reads `gamePuppetEntity::GetWorldPosition()` + `Quaternion` | P1 |
| R4.4 | **GTA V Enhanced hook**: ScriptHookV `.asi` plugin; native table call | P1 |
| R4.5 | **Coordinate system**: right-handed (X east, Y north, Z up); document conversion per-game | P1 |
| R4.6 | **Anti-cheat safety**: no DLL injection, no memory write, no kernel hook — read-only RTTI / script API only | P0 |
| R4.7 | **Per-game opt-in**: user warned + must consent before engine hook attempt | P1 |
| R4.8 | **Cyberpunk depth-buffer capture** (Layer 3 moat) | P2 |
| R4.9 | **Future-game hooks**: Elden Ring, Red Dead Redemption 2, Hogwarts Legacy — see `docs/MULTI_GAME_ROADMAP.md` | P2 |

## R5. Session Metadata

| # | Requirement | Priority |
|---|---|---|
| R5.1 | **`gameinfo.json` per session**: `{ "game_title", "game_version", "session_id", "route_type" ∈ {1,2,3}, "started_at", "duration_s" }` | P0 |
| R5.2 | **`system.json` per session**: CPU model, GPU model + driver version, RAM, OS build, primary monitor resolution | P0 |
| R5.3 | **`fps_stats.json` per session**: median, p1, p5, p50, p95, p99 of in-game FPS over session lifetime | P1 |
| R5.4 | **Operator annotation hotkeys F1/F2/F3**: set `route_type` per session before recording starts | P0 |
| R5.5 | **5-min auto-cap timer**: every clip self-terminates at 6 min if user has not stopped manually | P0 |
| R5.6 | **Atomic finalization**: metadata written via temp-file + rename, never partially flushed | P0 |

## R6. UI Element Refusal (Buyer Rejection Conditions)

| # | Requirement | Priority |
|---|---|---|
| R6.1 | **Reject if**: modal popup detected (Windows toast, browser notification, OS dialog) | P0 |
| R6.2 | **Reject if**: in-game pause menu / settings menu / loading screen visible | P0 |
| R6.3 | **Reject if**: any non-game window overlay (Discord, OBS, MSI Afterburner watermark) | P0 |
| R6.4 | **Reject if**: alt-tab to desktop / taskbar / Start menu within clip | P0 |
| R6.5 | **Reject if**: cutscene / pre-rendered CG / save-load screen | P1 |
| R6.6 | **Reject UX**: defensive abort + tray notification ("clip rejected: <reason>") | P0 |

## R7. Compliance & Privacy

| # | Requirement | Priority |
|---|---|---|
| R7.1 | **Game-only capture**: never record desktop / browsers / non-game apps | P0 |
| R7.2 | **Foreground gating**: pause recording instantly on focus loss | P0 |
| R7.3 | **Server-side PII**: face blur + username-in-notification blur before upload finalization | P1 |
| R7.4 | **Transport**: TLS 1.3 only; certificate pinning to our upload domain | P0 |
| R7.5 | **Consent UX**: explicit consent dialog each session, not just first launch | P2 |
| R7.6 | **Sensitive-window blocklist**: password managers, banking, healthcare apps — never resume recording while these are foreground | P2 |
| R7.7 | **Anti-cheat compatibility**: per-game opt-out before any hook attempt; clear EAC/BattlEye/VAC compatibility list | P1 |
| R7.8 | **Data-deletion request**: user can request "delete my session X" with 30-day SLA | P1 |
| R7.9 | **No browsers / chat apps in frame**: pixel-region attestation server-side; reject clips with non-game window detection | P2 |

## R8. Performance Budgets

| # | Requirement | Priority |
|---|---|---|
| R8.1 | **Game FPS impact**: ≤5% delta vs. no-recorder baseline (1080p30 typical AAA game) | P0 |
| R8.2 | **CPU**: recorder process ≤15% of single core at 1080p30 (NVENC/AMF path) | P0 |
| R8.3 | **GPU**: ≤8% utilization of dedicated encoder die (NVENC chip) | P0 |
| R8.4 | **RAM**: recorder process resident ≤512 MB steady-state, ≤1 GB peak | P0 |
| R8.5 | **Disk I/O**: write rate ≤2 MB/s at 1080p30 (8 Mbps target) | P0 |
| R8.6 | **Storage**: 1 hour of gameplay ≤4 GB on disk before upload | P0 |
| R8.7 | **Upload bandwidth**: respects user-configured cap (default: 50% of measured upstream) | P1 |
| R8.8 | **Recorder startup**: from launch to "ready to record" in ≤3 s on cold start | P1 |

## R9. Distribution & Installation

| # | Requirement | Priority |
|---|---|---|
| R9.1 | **Installer format**: NSIS or MSI, ≤80 MB total bundle | P0 |
| R9.2 | **Code signing**: Authenticode EV cert on installer + main `.exe` | P1 |
| R9.3 | **Bundled dependencies**: no third-party unsigned binaries; if ffmpeg used, vendor a signed build with hash pinned | P0 |
| R9.4 | **Install location**: `%LocalAppData%\GameDataRecorder\` (per-user, no admin required) | P0 |
| R9.5 | **Uninstall**: clean removal via Settings → Apps; user-data preserved with explicit opt-out | P1 |
| R9.6 | **First-run UX**: terms-of-service + privacy-policy acceptance modal | P0 |
| R9.7 | **Telemetry opt-in**: anonymous error reports (Sentry) — user can disable | P1 |
| R9.8 | **Update channel**: signed manifest fetched from our domain; delta patches | P1 |

## R10. Code Quality (Engineering Standards)

| # | Requirement | Priority |
|---|---|---|
| R10.1 | **Language**: Rust (stable 1.75+); Python 3.11+ for backend; no Java/Kotlin/JVM in client | P0 |
| R10.2 | **Code style**: `cargo fmt` clean; `cargo clippy -- -D warnings` clean | P0 |
| R10.3 | **Test coverage**: ≥80% line coverage on `crates/*`; integration tests for capture pipeline | P0 |
| R10.4 | **CI**: every PR runs build + test + clippy on Windows + macOS targets | P0 |
| R10.5 | **Cross-platform tests**: `crates/*` must `cargo test --target aarch64-apple-darwin` green | P1 |
| R10.6 | **Schema pinning**: `EngineFrame` field names + array order locked by test fixture | P0 |
| R10.7 | **Commit convention**: Conventional Commits (`feat:` / `fix:` / `docs:` / `test:` / `chore:`) | P0 |
| R10.8 | **PR review**: ≥1 approving reviewer; CI green before merge | P0 |
| R10.9 | **Documentation**: every public API doc-commented; rustdoc builds clean | P1 |
| R10.10 | **No dead code**: `cargo machete` + `knip`-equivalent clean | P1 |

## R11. Operational Readiness (Gate C)

| # | Requirement | Priority |
|---|---|---|
| R11.1 | **Content attestation**: perceptual hash + server-side frame fingerprint for every clip | P2 |
| R11.2 | **Anti-fraud**: binary signature check, HID-replay detection, gameplay novelty test | P2 |
| R11.3 | **DLL hijack protection**: `SetDefaultDllDirectories` on startup | P2 |
| R11.4 | **API key protection**: stored via DPAPI, not plaintext on disk | P2 |
| R11.5 | **DPI awareness**: `PerMonitorV2` manifest entry, capture correctly on HiDPI displays | P1 |
| R11.6 | **Symlink guard**: `recording_location` cannot be a symlink to system / network paths | P2 |
| R11.7 | **Localization**: en-US + zh-CN at launch; UI strings externalized | P1 |

---

## Verification Matrix Template

When evaluating any candidate implementation, score each R{n}.{m} as:

- **✅ Match**: requirement fully met, evidence linked (PR / test / artifact URL)
- **⚠️ Partial**: spirit met but specifics drift (e.g., wrong format / suboptimal default)
- **❌ Miss**: not implemented, or implemented in a way that violates the requirement
- **🚫 Anti**: implementation actively breaks the requirement (e.g., does the opposite)

A candidate must score **≥90% Match on P0** to be considered for Gate A merge.
A candidate must score **≥70% Match on P1** to be considered for Gate B inclusion.

---

## Total Requirements Count

| Section | P0 | P1 | P2 | Total |
|---|---:|---:|---:|---:|
| R1. Product Identity | 3 | 2 | 0 | 5 |
| R2. Video Capture | 12 | 2 | 0 | 14 |
| R3. Input Stream | 8 | 1 | 0 | 9 |
| R4. Engine Telemetry | 3 | 4 | 2 | 9 |
| R5. Session Metadata | 5 | 1 | 0 | 6 |
| R6. UI Element Refusal | 5 | 1 | 0 | 6 |
| R7. Compliance | 4 | 3 | 4 | 11 |
| R8. Performance | 6 | 2 | 0 | 8 |
| R9. Distribution | 3 | 5 | 0 | 8 |
| R10. Code Quality | 7 | 3 | 0 | 10 |
| R11. Operational | 0 | 2 | 5 | 7 |
| **Total** | **56** | **26** | **11** | **93** |

P0 = 56 ship-blocking requirements.
