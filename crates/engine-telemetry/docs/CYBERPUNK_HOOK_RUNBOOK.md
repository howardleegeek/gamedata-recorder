# Cyberpunk 2077 engine-telemetry hook — implementation runbook

Audience: puffydev (Windows engineer picking up the real implementation).

This file pairs with the scaffold in `crates/engine-telemetry/src/lib.rs`.
That scaffold defines the JSON contract, the `EngineHook` trait, and a
deterministic mock body for `CyberpunkHook`. Your job is to swap the mock
body for a real RTTI walk against a running Cyberpunk 2077 process while
keeping the cross-platform tests in `tests/integration.rs` green on Mac /
Linux CI.

The crate is split into two halves on purpose:

- Cross-platform half (`EngineFrame`, sidecar writer, hook trait, tests).
  Already done. Do **not** modify field names or array order — the buyer
  plugin parses these by string match, and breaking that silently produces
  zero training samples with no error.
- Windows-only half (real RTTI walker). Currently a mock. The notes below
  are everything you need to land it.

---

## 1. RTTI struct paths to read

REDengine 4 ships a typed RTTI runtime with classes and method tables that
you can address by name through the RED4ext registry — that is the path of
least maintenance, since it survives game patches that move offsets but keep
class names stable. All paths below are **string lookups**, not raw offsets.

### Player avatar

```
gameInstance (singleton)
  └─> GetPlayerSystem()                        → gamePlayerSystem
        └─> GetLocalPlayerControlledGameObject() → gamePuppetEntity
              ├─> GetWorldPosition()  → Vector4 { x, y, z, w }   (meters)
              └─> GetWorldOrientation() → Quaternion { i, j, k, r }
```

- `gamePuppetEntity::GetWorldPosition()` returns a `Vector4` where the
  `w` component is unused (REDengine reuses Vector4 for SoA reasons —
  ignore `w` and copy `[x, y, z]` into `EngineFrame::player_position`).
- `Quaternion { i, j, k, r }` maps to our wire format `[x, y, z, w]` as
  `[i, j, k, r]`. Yes, REDengine's `r` is at the end too — it's just
  named differently; do not swap.

### Camera

```
gameInstance
  └─> GetCameraSystem()                        → gameCameraSystem
        ├─> GetActiveCameraWorldTransform()    → WorldTransform
        │     ├─> Position    → Vector4   (meters)
        │     └─> Orientation → Quaternion
        └─> GetActiveCameraComponent()         → gameCameraComponent
              ├─> followOffset → Vector3 (meters; [right, up, -forward])
              └─> fov          → Float   (vertical FOV degrees, post-multiplier)
```

- `WorldTransform` is the fused position+orientation of the active camera
  (third-person follow, first-person, photo-mode, vehicle — all collapse
  through this single accessor, which is what you want).
- `gameCameraComponent::followOffset` is the **local-space** offset from
  the avatar pivot to the camera. REDengine reports `z` pointing forward
  along the avatar's facing direction; the buyer wire format wants
  `[right, up, back]`, where "back" is the *negative* of REDengine's z.
  The scaffold's docs flag this explicitly: do not double-negate. Copy
  `[x, y, -z]`.
- The `fov` field is the **post-multiplier effective** FOV. Cyberpunk
  also exposes `fovMultiplier` for cinematics; you do not need to read
  it separately because `gameCameraComponent::fov` already reflects it
  on the active camera. If you read the base `fov` instead, you'll get
  garbage values during scripted scenes (sniper-zoom, vehicle entry
  animations, photo-mode).

### Frame index + timestamp

The recorder owns the global `frame_index` (it's the same counter
`frames.jsonl` uses, and it is the buyer plugin's join key). Pass it in
from the recorder rather than reading the engine's frame counter — the
two will diverge after a single dropped frame. The scaffold
`CyberpunkHook` currently increments its own counter; replace this with
a constructor parameter or a setter the recorder calls each tick.

`timestamp_ms` is the wall-clock time since recording start. Same — read
it from the recorder's clock, not from `gameTimeSystem::GetSimTime()`,
because sim-time pauses during menus / loading screens and would no
longer align with depth/video frames.

### Metric scale

Hard-code `1.0`. REDengine units **are** meters. `EngineFrame::metric_scale`
is a per-frame field only because future profiles (UE5 in cm, idTech 7 in
inches) will diverge. For Cyberpunk specifically, `1.0` is correct and
reading anything else off the engine is a code smell.

---

## 2. DX12 swap-chain timing

The recorder calls `EngineHook::capture_frame` exactly once per
`IDXGISwapChain::Present`. This is non-negotiable: any other cadence
desyncs the telemetry from the depth buffer captured by `crates/depth-
hook` (which already runs on `ID3D12CommandQueue::ExecuteCommandLists`).

Two ways to wire this up:

1. **Piggyback on the depth-hook present-wrapper.** Cleanest. The depth
   hook already owns a `Present` interceptor; add an
   `Option<Box<dyn EngineHook>>` to its session struct, call
   `capture_frame` from inside the wrapper before forwarding to the real
   `Present`, and queue the resulting `EngineFrame` next to the
   `DepthFrame`. Both end up with the same `frame_index` and zero drift.
2. **Standalone Present hook.** Use this only if depth capture is
   disabled for the current title. Implementation is the same shape
   (MinHook / retour on `IDXGISwapChain::Present`), but you'll need to
   coordinate the frame counter independently.

Whichever path you pick, the call into `capture_frame` must complete in
**under 100 µs** (budget set by the 30 fps cadence and the depth-hook's
own per-frame budget — see `docs/CAPTURE_PERFORMANCE_INVESTIGATION.md`).
Cache RTTI offsets at install time; do not name-resolve per frame.

---

## 3. Anti-cheat compatibility

Cyberpunk 2077 has **no online anti-cheat** (the multiplayer roadmap is
indefinitely shelved as of CDPR Q4-2025 earnings). This means:

- **In-process attach (RED4ext / CET) is safe.** No risk of bans or
  module-integrity flags. Recommended path; estimated effort 1.5–2
  days.
- **Out-of-process via `ReadProcessMemory`** is also safe, but heavier
  to maintain (offsets break on every patch). Estimated effort 3–4
  days, plus ~half a day per major game patch to re-AOB.
- **REDmod does not interfere** — it's a content mod loader, separate
  from RED4ext, no overlap with the present hook surface.

Stay aware: this calculus is Cyberpunk-specific. When this scaffold
ports to other titles (Wukong, Alan Wake 2), check `docs/MULTI_GAME_
ROADMAP.md` for per-title anti-cheat status before picking an attach
surface.

---

## 4. Effort estimate by phase

| Phase | Description                                                                 | Est. effort |
|-------|-----------------------------------------------------------------------------|-------------|
| 1     | RED4ext plugin scaffold + RTTI registry attach + smoke-print player pos     | 0.5 day     |
| 2     | Wire `EngineHook::capture_frame` body, replace mock under `cfg(windows)`    | 0.5 day     |
| 3     | Hook into `crates/depth-hook` Present wrapper, share frame counter          | 0.5 day     |
| 4     | Validate quaternion / FOV / Follow Offset on a 10-min recording session     | 0.5 day     |
| 5     | Buyer-side ingestion smoke test (Decart Oasis sample pipeline)              | 0.5 day     |
| **Total** | **End-to-end live capture + verified sidecar**                          | **2.5 days**|

Pad another half-day for the first patch-bump after release; subsequent
patches should be near-zero-touch as long as you stay on the RTTI
name-lookup path.

---

## 5. Verification checklist (for the post-implementation review)

- [ ] `cargo test -p engine-telemetry --target aarch64-apple-darwin` still
      green on the Mac developer box. The integration tests do not depend
      on Windows code, so the `cfg(windows)` swap must not regress them.
- [ ] `cargo test -p engine-telemetry --target x86_64-pc-windows-msvc`
      green with the real RTTI walker enabled (you'll need a running
      Cyberpunk 2077 process — gate the live tests behind
      `--ignored` / a `RECORDER_LIVE_TESTS=1` env var so they don't run
      in CI).
- [ ] Captured a 10-minute walk-around in Night City, fed
      `engine_telemetry.json` to the buyer's sample reader, verified the
      avatar trajectory plotted in 3D matches the in-game motion.
- [ ] FOV values match the in-game UI value during a sniper-zoom event
      (sniper rifle FOV in Cyberpunk drops to ~12° from the ~80° default;
      this is a sharp visible step in the time series).
- [ ] No frame drops introduced — `docs/CAPTURE_PERFORMANCE_
      INVESTIGATION.md`'s 30 fps target holds with telemetry on.
- [ ] Quaternion norm-squared is within `[0.999, 1.001]` for every
      captured frame across the 10-min session (the scaffold's
      `InvariantViolation` branch shouldn't trip in practice).
- [ ] Sidecar file is written via `crate::util::durable_write` (atomic
      rename), not direct `File::create`. `write_telemetry_sidecar` in
      the scaffold is the in-process serialiser; the recorder's
      production path wraps it.

---

## 6. Where the scaffold ends and your work begins

Everything in `src/lib.rs` is touchable. Specifically, replace the body
of `CyberpunkHook::capture_frame` — keep the function signature, keep
the struct layout (extend it with a `red4ext_handle` or similar field
behind `#[cfg(windows)]` if you need state). Do **not** modify:

- `EngineFrame` field names or array layouts (wire contract).
- `write_telemetry_sidecar` empty-array behaviour (buyer contract).
- `HookError` variants (the recorder pattern-matches on these).
- Any of the `tests/integration.rs` assertions.

If a test fails after your change, your change is wrong; the contract
does not move. Open an issue or escalate to Howard before relaxing any
assertion.
