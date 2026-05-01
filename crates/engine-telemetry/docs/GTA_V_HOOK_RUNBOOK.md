# GTA V Enhanced engine-telemetry hook — implementation runbook

Audience: puffydev (Windows engineer picking up the real implementation).

This file pairs with the `GtaVHook` scaffold in `crates/engine-telemetry/src/lib.rs`.
That scaffold defines the JSON contract (shared with `CyberpunkHook` —
it's the same `EngineFrame` struct), the `EngineHook` trait, and a
deterministic mock body for `GtaVHook`. Your job is to swap the mock
body for real ScriptHookV native invokes against a running GTA V
Enhanced process while keeping the cross-platform tests in
`tests/integration.rs` green on Mac / Linux CI.

This is the **second** per-title hook (after `CyberpunkHook`); refer to
`CYBERPUNK_HOOK_RUNBOOK.md` for the parts that overlap (Present-wrapper
timing budget, sidecar contract, integration-test discipline). This
runbook focuses on what's GTA-V-specific.

---

## 1. Attach surface — ScriptHookV vs RAGEPluginHook

| Option | Pros | Cons | Recommendation |
|--------|------|------|----------------|
| **ScriptHookV** (Alexander Blade) | Mature C++ library; stable native function table indexed by hash; thousands of community plugins as reference; lightweight `.asi` plugin model. | C++ only; offsets are by-hash so a hash mismatch silently no-ops a native. | **Use this.** Simpler, more docs, lower runtime overhead. |
| **RAGEPluginHook** | Higher-level C# API; easier for prototyping; richer entity wrappers. | Adds .NET runtime dependency this crate doesn't otherwise need; thinner native-table coverage; more abstraction layers between us and RAGE. | Skip unless you're significantly more comfortable in C# than C++. |

**Decision: ScriptHookV.** Drop the compiled `.asi` plugin into the
GTA V install root, expose a Rust FFI shim that calls into the native
table, gate the whole module behind `#[cfg(windows)]`. The plugin must
include a single C++ entrypoint that ScriptHookV's loader detects
(`ScriptMain` + `DLL_PROCESS_ATTACH` registration), then drives the
recorder's per-frame tick from the script callback.

Reference plugins to read before writing yours:
- ScriptHookV's own `NativeTrainer` example (ships in the SDK zip).
- The `ENTITY` and `CAM` native categories on
  [alloc8or.re/gta5/nativedb/](https://alloc8or.re/gta5/nativedb/) —
  this site is the canonical native hash + signature reference.

---

## 2. World coordinate system

RAGE uses a **right-handed** coordinate system:

- `X` axis → east
- `Y` axis → north
- `Z` axis → up

This matches OpenGL / Blender conventions and the `EngineFrame` wire
format expects right-handed XYZ. Do **not** apply a handedness flip
when copying a `Vector3` into `EngineFrame::player_position` — copy the
components straight through.

Yaw rotates around `+Z` (counterclockwise looking down). Heading values
returned by `GET_ENTITY_HEADING` are degrees in the range `[0, 360)`,
where `0` faces `+Y` (north). This means a heading of `90°` faces `-X`
(west) under RAGE's right-handed convention — verify this on the first
end-to-end capture by comparing `GET_ENTITY_HEADING` against the
in-game minimap arrow, which points along the avatar's `+Y` body axis.

---

## 3. Player position, heading, and rotation quaternion

```text
PLAYER::PLAYER_PED_ID()                    → Ped (handle)
  ├─> ENTITY::GET_ENTITY_COORDS(ped, alive=true) → Vector3 (meters, world-space)
  └─> ENTITY::GET_ENTITY_HEADING(ped)            → Float (degrees, world-space yaw)
```

- Pass `alive = true` to `GET_ENTITY_COORDS`. With `alive = false`,
  RAGE returns `(0, 0, 0)` for ragdoll'd peds — useless for our case.
- `GET_ENTITY_HEADING` is a single yaw scalar, not a quaternion. RAGE
  has no public native that returns a full quaternion for a ped.
  **Construct** the quaternion in Rust:

  ```rust
  let yaw_rad = heading_deg.to_radians();
  let half = yaw_rad * 0.5;
  let q = [0.0, 0.0, half.sin(), half.cos()]; // [x, y, z, w]
  ```

  This is correct because RAGE peds only rotate around `+Z` for normal
  locomotion; pitch and roll come from animation state and are not
  exposed through public natives. For our buyer pipeline, yaw-only is
  sufficient — the camera quaternion (next section) carries the pitch
  the trainer actually needs.

---

## 4. Camera position, rotation, and FOV

```text
CAM::GET_GAMEPLAY_CAM_COORD()               → Vector3 (meters, world-space)
CAM::GET_GAMEPLAY_CAM_ROT(rotation_order=2) → Vector3 (pitch, roll, yaw — degrees)
CAM::_GET_GAMEPLAY_CAM_FOV()                → Float (vertical FOV degrees)
```

- `GET_GAMEPLAY_CAM_ROT` takes a `rotation_order` argument. Pass `2`.
  This returns Euler angles in `(pitch, roll, yaw)` order, which is the
  only order ScriptHookV's docs guarantee stable across patches.
- Convert Euler to quaternion in Rust. The `glam` crate (already in the
  workspace if you pull it as a `[target.'cfg(windows)'.dependencies]`)
  has `Quat::from_euler(EulerRot::XYZ, pitch, roll, yaw)` — verify the
  rotation order on a non-zero pitch capture before trusting it.
- `_GET_GAMEPLAY_CAM_FOV` is an **unnamed native**. Its hash is
  `0x5F35F6732C3FBBA0` and has been stable across all GTA V Enhanced
  patches as of the 2024 Enhanced Edition release. The leading
  underscore is a community-naming convention indicating an unnamed
  native — ScriptHookV exposes it through `_GET_GAMEPLAY_CAM_FOV` if
  you build against a recent SDK.

  **Fallback:** if exposing the unnamed native is blocked (e.g. an
  older ScriptHookV SDK doesn't expose it), hard-code `50.0` degrees —
  this is RAGE's default gameplay FOV. Consumers will then see no FOV
  variation during sniper-zoom or first-person camera, but the rest of
  the telemetry remains valid. Log a one-time warning at hook-install
  time when running in fallback mode so the buyer knows.

---

## 5. Camera follow offset (derived, not native)

RAGE does not expose a public native for the third-person camera follow
offset. The follow camera is a `CCamera` C++ subclass and its tuning
fields live on internal members not surfaced through ScriptHookV.

**Derive it.** The buyer plugin actually wants the *relative pose* —
where the camera sits in the player's local frame — and that's
trivially computable from quantities you already have:

```text
world_offset = camera_position_world - player_position_world
camera_follow_offset = inverse(player_rotation_quat) * world_offset
```

Express the result as `[right, up, back]` per the wire format
(documented on `EngineFrame::camera_follow_offset`). For RAGE's
right-handed `X=east, Y=north, Z=up`:

- `right` is the avatar's `+X_local` after yaw rotation.
- `up` is `+Z_local` (RAGE world up; player roll/pitch are zero).
- `back` is the *negative* of `+Y_local` (the avatar faces `+Y`, so
  the camera-behind-the-avatar offset is `-Y` in local space, and
  "back" is the positive of that).

This is a Rust-side computation; no extra native invokes needed.

---

## 6. Metric scale = 1.0 (validate via 100m walk test)

Hard-code `metric_scale = 1.0`. RAGE world units are nominally meters,
confirmed empirically by Rockstar's own physics constants (gravity =
`9.81` units/s², matching real-world m/s²). The `GtaVHook::METRIC_SCALE`
constant locks this in.

**Validation procedure** (run once before merging the real hook):

1. In Story Mode, spawn at a known landmark with a measurable straight
   distance to a second landmark. The runway at Los Santos
   International (LSIA) is ideal — it's exactly **1000 m** long and
   straight along `-X` from the eastern threshold.
2. Stand at the eastern threshold, record `GET_ENTITY_COORDS()`.
3. Walk (or drive — the test is unit-agnostic) to a point 100 m along
   the runway by visually counting 10 runway centerline stripes (each
   is 30 m long with a 20 m gap, so 2 stripes + 2 gaps ≈ 100 m).
4. Record the new coordinates.
5. Compute `delta = sqrt(dx² + dy²)`. Assert `99.0 ≤ delta ≤ 101.0`.

If `delta` is significantly off (e.g. `~0.03`), RAGE is reporting in cm
or some other unit and `metric_scale` needs adjustment — but in two
decades of RAGE engine reverse-engineering nobody has reported this, so
treat a failure as "look harder for a measurement bug" before changing
the constant.

---

## 7. Online vs offline anti-cheat — Story Mode only

**GTA Online rejects ScriptHookV.** The BattlEye anti-cheat shipped
with GTA V Enhanced detects ScriptHookV's signature and will kick (and
historically ban) any session where it's loaded. **The recorder must
only attach in offline single-player mode (Story Mode).**

Implementation requirements:

1. **Pre-attach check.** At hook-install time, call
   `NETWORK::NETWORK_IS_GAME_IN_PROGRESS()`. If it returns `true`,
   abort the install:

   ```rust
   if network_is_game_in_progress() {
       return Err(HookError::NotAttached(
           "GTA V Online detected; recorder only supports Story Mode".into()
       ));
   }
   ```

2. **Per-frame check.** The user can transition Story Mode → Online
   without restarting the game. Re-check on every frame; if Online is
   detected mid-recording, gracefully tear down the hook and surface a
   user-facing error ("Online session detected — recording stopped").

3. **No bypass.** Do not attempt to spoof the BattlEye check, scrub
   ScriptHookV's signature, or otherwise evade detection. It's a TOS
   violation, a ban risk for the user, and outside the recorder's
   threat model.

The recorder's UI should also disable the "Record GTA V" toggle when
the user has Online launched, with a tooltip explaining the limitation.

---

## 8. DX12 swap-chain timing

GTA V Enhanced uses **DX12** (the Enhanced Edition's headline upgrade
over the legacy DX11 build). Same Present-wrapper integration as
`CyberpunkHook`:

- Sample one `EngineFrame` per `IDXGISwapChain::Present`.
- Piggyback on the `crates/depth-hook` Present wrapper if depth
  capture is enabled, to guarantee `frame_index` parity with the
  captured depth buffer.
- Per-frame budget: **under 100 µs**, same as Cyberpunk. ScriptHookV
  natives are hash-resolved at install time (cache the resolved
  function pointers in `GtaVHook` state), so the hot-path cost is one
  indirect call per native invoke.

**Caveat**: ScriptHookV runs the script callbacks on its own thread,
not on the render thread. You cannot call ScriptHookV natives from
inside the Present wrapper directly — instead, use the present wrapper
to **signal** (e.g. an `AtomicU64` frame counter) and have the
ScriptHookV script callback read coords + camera state into a
lock-free SPSC ring buffer that the recorder drains. This adds one
frame of latency to telemetry vs. depth, which is acceptable (the
buyer pipeline already tolerates ±1 frame of jitter).

---

## 9. Effort estimate by phase

| Phase | Description                                                                | Est. effort |
|-------|----------------------------------------------------------------------------|-------------|
| 1     | ScriptHookV `.asi` plugin scaffold + Rust FFI shim + smoke-print player coord | 0.5 day     |
| 2     | Wire `EngineHook::capture_frame` body, replace mock under `cfg(windows)`   | 0.5 day     |
| 3     | Camera follow-offset derivation + Euler→quaternion math + FOV native       | 0.5 day     |
| 4     | SPSC ring buffer between ScriptHookV thread and Present wrapper            | 0.5 day     |
| 5     | 100m walk test for `metric_scale` + Online-detection bail-out test         | 0.5 day     |
| 6     | Buyer-side ingestion smoke test (Decart Oasis sample pipeline) on a 5-min Story Mode capture | 0.5 day     |
| **Total** | **End-to-end live capture + verified sidecar**                         | **3 days**  |

This is **a half-day longer than `CyberpunkHook`'s 2.5-day estimate**.
The extra day is split across: (1) RAGE has fewer documented internals
than REDengine 4's RTTI registry (no by-name lookup; you're working
against community-maintained native hash tables), and (2) the
ScriptHookV-thread / render-thread bridge is non-trivial (Cyberpunk's
in-process RED4ext attach lets you read RTTI directly from the present
hook with no thread-handoff).

---

## 10. Verification checklist (for the post-implementation review)

- [ ] `cargo test -p engine-telemetry --target aarch64-apple-darwin` still
      green on the Mac developer box. The integration tests do not depend
      on Windows code, so the `cfg(windows)` swap must not regress them.
- [ ] `cargo test -p engine-telemetry --target x86_64-pc-windows-msvc`
      green with the real ScriptHookV bridge enabled (gate live tests
      behind `--ignored` / a `RECORDER_LIVE_TESTS=1` env var so they
      don't run in CI without a running game).
- [ ] 100m walk test on the LSIA runway returns `99.0 ≤ delta ≤ 101.0`.
- [ ] `NETWORK_IS_GAME_IN_PROGRESS` bail-out fires when launching with
      GTA Online active. Recorder UI disables the toggle and shows the
      tooltip.
- [ ] FOV values match the in-game UI during a sniper-zoom event (sniper
      rifle FOV in GTA V drops to ~10° from the ~50° default; this is a
      sharp visible step in the time series).
- [ ] No frame drops introduced — the 30 fps target from
      `docs/CAPTURE_PERFORMANCE_INVESTIGATION.md` holds with telemetry on.
- [ ] Quaternion norm-squared is within `[0.999, 1.001]` for every
      captured frame across a 10-min Story Mode session.
- [ ] Captured a 5-minute walk-around in Vinewood, fed
      `engine_telemetry.json` to the buyer's sample reader, verified the
      avatar trajectory plotted in 3D matches the in-game motion.
- [ ] Sidecar file is written via `crate::util::durable_write` (atomic
      rename) in production — same as Cyberpunk's path.

---

## 11. Where the scaffold ends and your work begins

Everything in `src/lib.rs`'s `GtaVHook` is touchable. Specifically,
replace the body of `GtaVHook::capture_frame` — keep the function
signature, keep the struct layout (extend it with a
`script_hook_v_handle` or similar field behind `#[cfg(windows)]` if
you need state). Do **not** modify:

- `EngineFrame` field names or array layouts (wire contract; shared
  with `CyberpunkHook`).
- `write_telemetry_sidecar` empty-array behaviour (buyer contract).
- `HookError` variants (the recorder pattern-matches on these).
- Any of the `tests/integration.rs` assertions.

If a test fails after your change, your change is wrong; the contract
does not move. Open an issue or escalate to Howard before relaxing any
assertion.
