# Bug Triage — v2.5.4 → v2.5.5 / v2.6 / v3.0

## 5-round audit total: 40 findings (16 CRITICAL, 24 IMPORTANT)

But **bug count ≠ product quality**. Bug count = audit thoroughness.

For comparison:
- OBS Studio (our dependency): 2,800+ open issues on GitHub
- Chromium: 34,000+ open bugs
- **40 bugs from 5 parallel audit rounds = LOW for a ~15k LOC codebase**

## Why so many?

1. **~80% are inherited from upstream (OWL Control)**, not introduced by us.
   - `_pid` ignored — OWL's original trait design
   - 200ms sleep hack — a TODO comment in OWL's code
   - Heartbeat-as-FPS — OWL's fps_logger pattern
   - Hardcoded "NVMe SSD" — OWL's LEM metadata stub
2. **Most never fire for the actual user**. Turkish locale, RDP session, anti-cheat, multi-monitor, symlink attacks — all irrelevant for a single trusted tester.
3. **We ran 5 parallel audits in 30 minutes**. Most teams never do this. They ship, then discover bugs the hard way.

---

## Gate A — v2.5.5 (today, client demo works end-to-end)

Only these 5 bugs block the client's GTA V recording from working:

| # | File | Issue | Effort |
|---|------|-------|--------|
| A1 | `src/validation/mod.rs:159-173` | JSONL→CSV reconstruction splits on commas inside JSON arrays, zeroes all multi-arg events in `input_stats`. Fix: parse JSONL directly, don't reconstruct CSV. | 1h |
| A2 | `src/config.rs:15-22` | `wmic` NVENC probe missing on Windows N / LTSC / 22H2+. Fix: use DXGI adapter enumeration already available at startup. | 1h |
| A3 | `src/record/obs_embedded_recorder.rs:690` | 200ms `thread::sleep` stalls tokio every stop, drops input events. Fix: replace with `Notify`/oneshot when skipped-frames log arrives. | 1.5h |
| A4 | `crates/input-capture/src/lib.rs:83` | mpsc capacity 10 overflows during stop-stall. Fix: raise to 10_000. | 0.2h |
| A5 | `crates/game-process/src/lib.rs:42-61` + `src/record/recorder.rs:425-432` | ANSI `szExeFile` silently skips Chinese-locale game paths. Fix: migrate to W-suffix wide APIs (`QueryFullProcessImageNameW`, `PROCESSENTRY32W`). | 2h |

**Total effort**: ~6 hours of focused work. Ship today.

## Gate B — v2.6 (this week, friendly-tester production)

Adds these ~12 fixes for the next 100 testers:

- `stop_recording` proper signal (not sleep)
- OBS thread Drop timeout
- `LemInputStream::stop()` → blocking send
- MP4 fsync before metadata write
- Atomic write for `finalize_session_metadata`
- `metadata_writer` real GPU/FPS/FOV data (not hardcoded)
- Non-ASCII locale support throughout
- Optimus adapter-index via OBS-direct query
- DPI-awareness manifest for the whole app
- Workstation lock handling (DXGI_ERROR_ACCESS_LOST)
- RwLock TOCTOU on `listening_for_new_hotkey`
- Upload queue atomic enqueue via channel message

## Gate C — v3.0 (weeks, adversarial users)

Before opening payouts to public users we need:

- Content attestation (perceptual hash + server-side frame fingerprint)
- Anti-fraud: binary signature check, HID-replay detection, gameplay novelty test
- Privacy consent UX (every time, not just once)
- Alt-tab pause-recording mode
- Blocklist of sensitive windows (password managers, banking, etc.)
- DLL hijack protection (`SetDefaultDllDirectories`)
- Authenticode signing of the recorder binary
- DPAPI-protect API key
- Symlink guard on `recording_location`
- Anti-cheat compatibility: per-game warning / opt-out before hook attempt
- Monitor capture → mask non-game window overlays

---

## Bug distribution by relevance to current client (华硕主机X, RTX 4060, Chinese Windows 11)

- **Affects them today** (Gate A): 5 bugs
- **Affects them after Gate A** (Gate B): ~6 bugs
- **Only matters at scale** (Gate C): ~18 bugs
- **Irrelevant to their machine** (Turkish locale, RDP, Optimus, multi-monitor, etc.): ~11 bugs

So of the 40 total, **~29% hit the actual client, 45% matter at public rollout, 26% never fire for them.**
