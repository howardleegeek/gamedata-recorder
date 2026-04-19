# GameData Recorder — 预案手册 (Contingency Playbook)

> 12 scenarios × (Trigger → Detection → Immediate Action → Fallback → Comms)
> Target: collapse response time from 10–30 min to < 2 min for any known failure mode.

---

## TIER 1 — 录制客户端故障 (Recorder Client Failures)

### 预案 1: 录制黑屏 (Black screen recording)

| Field | Plan |
|---|---|
| **Trigger** | MP4 output is all-black or desktop-only, not game content |
| **Detection** | User report OR `scripts/verify.py` detects > 95% black pixels in first 10 frames |
| **Immediate action** | (A) Check `metadata.json.capture_mode` — if `window_capture`, force-switch to `monitor_capture` via `config.json`. (B) Ensure game is on primary monitor. (C) Restart recorder. |
| **Fallback** | If monitor capture also black → manually set `force_encoder = x264` in config (software encode can capture anything the driver composites). Last resort: run `desktop_duplication` mode (DDA API). |
| **Comms** | "Detected capture mode mismatch. Switching to monitor capture. Please restart the game." |

### 预案 2: 游戏未检测 (Game not detected / 未进入游戏就开始录了)

| Field | Plan |
|---|---|
| **Trigger** | Recorder starts recording while still on launcher/menu/desktop |
| **Detection** | `metadata.active_game` = `launcher.exe` / `playgtav.exe` / empty |
| **Immediate action** | Add offending process to `SELF_AND_SYSTEM_BLACKLIST` in `crates/constants/src/lib.rs`. Hotfix + rebuild + redeploy. |
| **Fallback** | Ship with `auto_record = false`, user must press F9 manually. Ugly but safe. |
| **Comms** | "Identified launcher process being mistaken for game. Patch incoming in 10 min." |

### 预案 3: Raw Input 注册失败 (0x80070057)

| Field | Plan |
|---|---|
| **Trigger** | `RegisterRawInputDevices` fails at `kbm_capture.rs:123` |
| **Detection** | `crashes/*.json` has fingerprint matching this panic; OR stderr contains `"failed to register raw input devices"` |
| **Immediate action** | Gate `RegisterRawInputDevices` behind `catch_unwind` + fallback to `SetWindowsHookEx` (legacy WH_KEYBOARD_LL / WH_MOUSE_LL). |
| **Fallback** | Disable keyboard/mouse capture entirely, keep video + XInput. Log warning to user: "Input capture unavailable, video will still record." |
| **Comms** | "Raw Input unavailable on this session. Video recording continues; keyboard/mouse input will be recorded via fallback API." |

---

## TIER 2 — 基础设施故障 (Infrastructure Failures)

### 预案 4: Nucbox 掉线 / IP 变化 (Nucbox offline / IP changed)

| Field | Plan |
|---|---|
| **Trigger** | SSH to nucbox times out OR IP changed (Tailscale re-assignment) |
| **Detection** | `ping $NUCBOX_IP` 100% packet loss |
| **Immediate action** | `/opt/homebrew/bin/tailscale --socket=/opt/homebrew/var/run/tailscaled.sock status \| grep nucbox-m6ultra` to get fresh IP. Update `~/.ssh/config` `nucbox` entry. |
| **Fallback** | If completely offline → switch red-team + test loop to `mac2` node (dispatch to 100.91.32.29). If mac2 also down → pause red-team, focus on client-visible bugs only. |
| **Comms** | Auto-notification via `scripts/cluster-health.sh` hourly cron; Howard gets text if offline > 30 min. |

### 预案 5: CI build 卡住 / 失败 (CI stuck or failing)

| Field | Plan |
|---|---|
| **Trigger** | GitHub Actions "Build and Release" red OR running > 10 min |
| **Detection** | `gh run list --workflow="Build and Release" --limit 3` shows failure/hung |
| **Immediate action** | (A) `cargo fmt` + `cargo clippy` locally, push fix. (B) If GitHub Actions infra down, build locally: `cd gamedata-recorder && cargo build --release` on Mac Studio (ARM64 cross-compile) or push to `nucbox` for native build. |
| **Fallback** | Pre-built v2.5.1 zip in `/tmp/v2.5.1/` on Howard's Mac. Deploy directly via scp without CI. |
| **Comms** | Don't commit CI-breaking changes right before demo. Freeze main branch 2 hrs before scheduled demo. |

### 预案 6: OBS 依赖构建失败 (OBS libraries fail to build)

| Field | Plan |
|---|---|
| **Trigger** | `cargo obs-build build` fails; `obs-ffmpeg-mux.exe` missing |
| **Detection** | Build output contains `Unable to start the recording helper process` |
| **Immediate action** | Clear `target/` + `~/.cargo-target-shared/` caches. Re-run `cargo obs-build build --out-dir target\x86_64-pc-windows-msvc\release`. |
| **Fallback** | Pin OBS to a known-working commit (last successful: `libobs-wrapper = "0.4.x"`). Revert any Cargo.lock changes to that commit. |
| **Comms** | Internal only — don't mention OBS dependency issues to client. |

---

## TIER 3 — 数据管道故障 (Integration / Data Failures)

### 预案 7: 上传失败 / 后端 500 (Upload fails / backend 500)

| Field | Plan |
|---|---|
| **Trigger** | Client sees "Upload failed" OR backend returns 5xx |
| **Detection** | Frontend toast; backend logs in `/var/log/fastapi/app.log` |
| **Immediate action** | (A) Check backend via `curl $API_BASE_URL/health`. (B) Restart FastAPI container: `docker restart oyster-api`. (C) Verify S3 creds: `aws s3 ls s3://oyster-gamedata-recordings/`. |
| **Fallback** | Client always retains local copy in `recordings/`. Disable auto-upload via `config.upload_enabled = false`; user can manually upload later via `scripts/manual-upload.py`. |
| **Comms** | "Upload queued — will retry automatically. Recording is saved locally at `%APPDATA%\gamedata-recorder\recordings\`." |

### 预案 8: JSONL 格式不兼容 (JSONL format mismatch)

| Field | Plan |
|---|---|
| **Trigger** | Data team rejects uploaded recording for format violation |
| **Detection** | Backend validation 400; OR post-hoc review flags records |
| **Immediate action** | Run `scripts/verify-jsonl.py <path>` to identify field. Most common: missing `timestamp` precision (ns vs ms), wrong `event_type` enum case. Patch `src/output_types.rs`. |
| **Fallback** | Post-process old recordings via one-shot script (`scripts/migrate-jsonl.py`). Client doesn't re-record. |
| **Comms** | "Format migration complete — backfilled X recordings. No user action required." |

### 预案 9: 时间戳漂移 (Timestamp drift > 10ms)

| Field | Plan |
|---|---|
| **Trigger** | `drift_ns` in timestamps.jsonl > 10,000,000 |
| **Detection** | Validation warning OR video PTS vs wallclock diverges |
| **Immediate action** | Switch `session_manager.now_ns()` from `SystemTime::now()` to `Instant::now() + start_wallclock`. Avoid NTP jumps. |
| **Fallback** | Record both monotonic and wallclock; downstream can pick. |
| **Comms** | Internal only — data quality issue, not user-facing. |

---

## TIER 4 — 客户关系故障 (Client Relationship Failures)

### 预案 10: Demo 日当场崩溃 (Crash during live demo)

| Field | Plan |
|---|---|
| **Trigger** | Recorder crashes mid-demo with client watching |
| **Detection** | The silence. |
| **Immediate action** | (A) Don't apologize excessively, don't diagnose live. (B) Say: "Let me restart — that's a known intermittent we have a fix for, but let me show you the output from last night's run." (C) Switch to pre-recorded demo video (`demo/golden-run.mp4`) showing successful GTA V capture. |
| **Fallback** | Show BUGS.md — demonstrate engineering rigor: "Here are the 37 issues we've already caught and fixed, with reproduction steps for each." |
| **Comms** | **Pre-brief client before demo**: "Live software sometimes hiccups — I have a backup recording of the 10-minute GTA V session from last night if anything goes sideways." Makes a crash a non-event. |

### 预案 11: 客户发现新 bug (Client finds new bug)

| Field | Plan |
|---|---|
| **Trigger** | Client's test user reports issue not in BUGS.md |
| **Detection** | Email / Slack / phone |
| **Immediate action** | (A) Acknowledge within 1 hour ("Got it, investigating, will reply by EOD with reproduction steps"). (B) Request `%APPDATA%\gamedata-recorder\crashes\*.json` + last recording session. (C) Add to BUGS.md with `Status: investigating`. |
| **Fallback** | If not reproducible within 4 hrs → ship instrumented build (extra `tracing::info!` lines) to collect more data next run. |
| **Comms** | Never say "that's not a bug" or "works on my machine". Always: "Thanks — I can see it in the logs, fix coming in version X.Y.Z." |

### 预案 12: 客户要求功能未实现 (Client wants feature not yet built)

| Field | Plan |
|---|---|
| **Trigger** | Client: "can it also capture X / export format Y / run on Linux?" |
| **Detection** | Meeting question, feature request |
| **Immediate action** | (A) Buy time: "Let me check the pipeline." (B) Triage internally: can we ship in < 1 week? → commit to date. Can't? → "Not in scope for v1, let's add to roadmap for v2 in 4 weeks." |
| **Fallback** | If feature is deal-breaker → scope negotiate: "We can do a simpler version of X (Y% of the functionality) by Friday, full version in month 2. Acceptable?" |
| **Comms** | Always return with a date, not a vague promise. Written confirmation in email/Slack, not just verbal. |

---

## 附录 A — 关键命令速查 (Cheat Sheet)

```bash
# Nucbox IP discovery
/opt/homebrew/bin/tailscale --socket=/opt/homebrew/var/run/tailscaled.sock status | grep nucbox

# SSH with persistent master
ssh -o ControlMaster=yes -o ControlPath="~/.ssh/sockets/%r@%h-%p" \
    -o ControlPersist=10m -fN howard@$NUCBOX_IP

# Kill + relaunch recorder on nucbox
ssh howard@$NUCBOX_IP "taskkill /F /IM gamedata-recorder.exe /T; \
    cd C:\\Users\\Howard\\Downloads\\gamedata-recorder && \
    start gamedata-recorder.exe"

# Fetch latest crash from nucbox
scp howard@$NUCBOX_IP:"C:/Users/Howard/AppData/Roaming/gamedata-recorder/crashes/*.json" \
    ~/Downloads/crashes/

# Emergency rollback to last-known-good
git revert HEAD && git push
gh release download v2.4.1  # last release before current trouble

# Backend health
curl -s $API_BASE_URL/health | jq

# Force CI rerun
gh run rerun $(gh run list --workflow="Build and Release" --limit 1 --json databaseId -q '.[0].databaseId')
```

---

## 附录 B — 升级路径 (Escalation Path)

| Severity | Response time | Action |
|---|---|---|
| **P0** (demo breaking, client waiting) | < 5 min | Skip analysis, execute plan's "immediate action" blindly. Diagnose later. |
| **P1** (production blocking, no demo) | < 1 hour | Full plan execution including comms. |
| **P2** (annoying, workaround exists) | Same day | Add to BUGS.md, schedule fix. |
| **P3** (quality of life) | Next sprint | Queue in autoresearch red-team loop. |

---

## 附录 C — 不要做的事 (Anti-patterns)

1. ❌ **不要在 demo 前 2 小时合并任何 PR** — feature freeze 2 hrs before any client touch-point.
2. ❌ **不要依赖 nucbox 是唯一测试环境** — always have a mac2 fallback.
3. ❌ **不要向客户暴露内部文件路径** — sanitize all error messages before they leave the machine.
4. ❌ **不要在 live demo 里尝试 bug 诊断** — pivot to backup content, diagnose async.
5. ❌ **不要 silently fail** — every crash must produce a report. No "ghosts in the machine."
6. ❌ **不要 commit 然后 push 又 force push** — client / teammates may have pulled already.
7. ❌ **不要在 release 里带 debug/trace logs** — client shouldn't see our internal breadcrumbs.

---

*Last updated: v2.5.1 (2026-04-19). Review before each demo.*
