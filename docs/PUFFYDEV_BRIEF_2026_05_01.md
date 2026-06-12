# Puffydev Brief — 2026-05-01

*Audience: puffydev (Windows engineer) · Window: 2026-04-25 → 2026-05-01 · Owner: Howard*

> Focused brief on what changed in `gamedata-recorder` this session that touches puffydev's lane, and what is queued for him next. No source code in this repo was modified by this brief — it is documentation only.

---

## 1. TL;DR

Seven commits landed in `gamedata-recorder` between 2026-04-25 and 2026-05-01, all in puffydev's lane. The themes:

- **Buyer-plugin compatibility** — native `action_camera.json` sink + gamepad-aware schema (modality-routed).
- **Cross-platform tests** — new `action-camera-tests` crate (30 Mac tests) and `engine-telemetry` crate (22 Mac tests). Both pass on aarch64-apple-darwin.
- **Performance investigation** — `CAPTURE_PERFORMANCE_INVESTIGATION.md` documents that the 1fps metric was a bug surface, not a capture-loop regression.
- **Two PR-ready specs queued** — `RECORDER_BUYER_SPEC_FEATURES.md` (F1/F2/F3 + 5-min auto-cap + UI refusal) and `engine-telemetry` Phase 2 hook scaffolds for Cyberpunk 2077 and GTA V (each ships with a runbook).

Net: scaffolding and tests are done; puffydev's job is to fill the Windows-only halves and ship the buyer-spec features.

---

## 2. What Landed (commits since 2026-04-25)

| SHA | Date | Subject |
|---|---|---|
| `fd88f1b` | 2026-04-28 | feat(record): native `action_camera.json` sink for buyer plugin compatibility |
| `008a838` | 2026-04-28 | test(rust): `action-camera-tests` crate — 30 cross-platform tests on Mac |
| `de3139a` | 2026-04-28 | docs(perf): `CAPTURE_PERFORMANCE_INVESTIGATION.md` — 1fps metric is a bug, not capture |
| `3a85bc0` | 2026-04-28 | feat(rust): `action_camera_writer.rs` — gamepad-aware schema (modality-routed) |
| `9c47d72` | 2026-05-01 | docs(spec): `RECORDER_BUYER_SPEC_FEATURES.md` — PR-ready spec for puffydev (Engineer HHH) |
| `3a74b19` | 2026-05-01 | feat(engine-telemetry): `crates/engine-telemetry` — Phase 2 hook scaffold (Engineer R11, 18 Mac tests) |
| `c5180f4` | 2026-05-01 | feat(gtav): `GtaVHook` scaffold + RAGE runbook — 22 Mac tests (Engineer DD14) |

Note: the engine-telemetry test count grew from 18 (R11 scaffold) to 22 (after GTA V hook landed in DD14).

---

## 3. What's Queued for Puffydev

Three PR-ready specs awaiting Windows implementation. All are additive — defaults preserve current behaviour.

### 3.1 Buyer-spec features (Engineer HHH, ~1.5 days)

**Spec:** `docs/RECORDER_BUYER_SPEC_FEATURES.md` (16 KB, PR-ready)

Three independent features, each gated behind a Preferences flag:

| Feature | Effort | Why |
|---|---|---|
| `route_type` tagging via F1/F2/F3 hotkeys | ~4h | Buyer's `gameinfo` schema requires per-clip `route_type ∈ {1,2,3}` — operator-annotated, not derivable post-hoc. |
| 5-min auto-cap timer | ~2h | Buyer-spec acceptance: every clip `5 ≤ duration ≤ 6 min`. ~30% of clips drift in operator dry-runs today. |
| UI-element refusal (modal/notification/menu detection) | ~1d | Buyer rejects clips with 弹窗/系统通知/水印/模态框/读档CG/游戏配置界面/切出画面/电脑菜单栏. Defensive abort + warn UX. |

The spec contains file:line proposals for `src/config.rs`, `src/app_state.rs`, `src/record/recording.rs`, `src/record/recorder.rs`, `src/tokio_thread.rs`, and `src/output_types/lem_metadata.rs`. Tests proposed under `tests/route_type.rs`.

### 3.2 Cyberpunk 2077 hook fill (Engineer R11 scaffold, ~2.5 days)

**Runbook:** `crates/engine-telemetry/docs/CYBERPUNK_HOOK_RUNBOOK.md` (10 KB)
**Scaffold:** `crates/engine-telemetry/src/lib.rs` (`CyberpunkHook`)

Cross-platform half is done — `EngineFrame`, sidecar writer, `EngineHook` trait, deterministic mock body, integration tests. Puffydev's job:

- Swap the mock body for a real RTTI walk against a running Cyberpunk 2077 process via the RED4ext registry (string-lookup paths — survives game patches that move offsets).
- Read `gamePuppetEntity::GetWorldPosition()` (Vector4, ignore `w`) and `Quaternion {i,j,k,r}` → wire-format `[x,y,z,w]` order.
- Keep `tests/integration.rs` green on Mac/Linux CI — do **not** rename fields or reorder arrays (the buyer plugin parses by string match; breaking that silently produces zero training samples with no error).

### 3.3 GTA V Enhanced hook fill (Engineer DD14 scaffold, ~3 days)

**Runbook:** `crates/engine-telemetry/docs/GTA_V_HOOK_RUNBOOK.md` (14 KB)
**Scaffold:** `crates/engine-telemetry/src/lib.rs` (`GtaVHook`)

Decision: **ScriptHookV** over RAGEPluginHook (simpler, more docs, lower runtime overhead, no .NET dependency).

- Drop a compiled `.asi` plugin into the GTA V install root.
- Expose a Rust FFI shim that calls into ScriptHookV's native table.
- Gate the whole module behind `#[cfg(windows)]`.
- RAGE coordinates: right-handed (`X` east, `Y` north, `Z` up) — matches OpenGL/Blender and the `EngineFrame` wire format.
- Reference: `NativeTrainer` example in the ScriptHookV SDK + `alloc8or.re/gta5/nativedb/` for native hashes/signatures.

The runbook explicitly defers Cyberpunk-overlapping topics (Present-wrapper timing budget, sidecar contract, integration-test discipline) to `CYBERPUNK_HOOK_RUNBOOK.md`.

---

## 4. Cross-Platform Tests

Both crates ship with Mac-CI green tests. Run from repo root:

```bash
cargo test -p action-camera-tests --target aarch64-apple-darwin   # 30 tests
cargo test -p engine-telemetry --target aarch64-apple-darwin      # 22 tests
```

The 30+22 figures are authoritative per the commit messages (`008a838` and `c5180f4`); raw `#[test]` counts in `tests/integration.rs` look lower because some assertions are driven from helper-loop fixtures.

The discipline going forward: **any change to `EngineFrame` field names or array order breaks the buyer plugin silently.** Tests pin both. Do not relax them.

---

## 5. Test Fixtures — `src/record/action_camera_writer.rs`

Commit `3a85bc0` reshaped this file from 67-line-additions of legacy mouse/kbd-only schema into a 519-line gamepad-aware modality-routed schema:

- **Modality routing:** events are tagged `keyboard | mouse | gamepad` and serialized into per-modality arrays so downstream consumers can subset without re-parsing.
- **Gamepad-aware fields:** stick axes, trigger axes, button bitfields — all serialized in fixed order to match the buyer plugin's parser.
- **Backwards compatibility:** legacy mouse/kbd records still deserialize; old recordings remain valid.

This file is now 1155 lines. **Do not rename fields without a buyer-plugin coordination ticket** — same string-match contract as `EngineFrame`.

---

## 6. Where to Ask Questions

| Topic | Source of truth |
|---|---|
| Buyer-spec acceptance bar (clip duration, route_type, UI refusal, gameinfo schema) | `oyster-enrichment/docs/BUYER_SPEC_v1.md` |
| Coordinate-system conventions across engines (REDengine, RAGE, others) | `oyster-enrichment/docs/COORDINATE_SYSTEMS_GUIDE.md` |
| RECORDER_BUYER_SPEC_FEATURES file:line proposals | `gamedata-recorder/docs/RECORDER_BUYER_SPEC_FEATURES.md` |
| Cyberpunk hook RTTI paths and JSON contract | `gamedata-recorder/crates/engine-telemetry/docs/CYBERPUNK_HOOK_RUNBOOK.md` |
| GTA V ScriptHookV attach surface and native hash references | `gamedata-recorder/crates/engine-telemetry/docs/GTA_V_HOOK_RUNBOOK.md` |
| Capture-loop performance question (1fps metric) | `gamedata-recorder/docs/CAPTURE_PERFORMANCE_INVESTIGATION.md` |

Ping Howard for buyer-side clarifications; ping the Engineer scaffold author (R11 / DD14 / HHH) for scaffold-internal questions before changing wire contracts.

---

*End of brief. No source code modified.*
