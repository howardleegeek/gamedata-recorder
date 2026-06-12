# depth-hook

Per-title DX12 depth-buffer capture for `gamedata-recorder`.

## Why this crate exists

The commodity tier of gameplay-data suppliers (Chinese network-café video
farms, UGC footage scrapers, generic screen-recorder resellers) produces raw
video + input logs. That is a race to the bottom on margin.

This crate is how `gamedata-recorder` produces the **enriched tier**:
ground-truth 3D data — depth buffer + camera matrices per frame, straight
from the GPU — which cannot be replicated without per-title DX12
reverse-engineering expertise.

Every new `DepthHookProfile` added to this crate is one more title where the
recorder ships the $300–800/hr enriched-tier data instead of the $30–80/hr
raw tier. Profiles compound: each one reuses the heuristic and hook
infrastructure built for the last.

**This is a product moat, not an infrastructure concern.** The code in this
crate is what the commodity suppliers cannot match without building an
equivalent DX12 reverse-engineering team from scratch.

## Architecture

```
depth-hook/
├── src/
│   ├── lib.rs              # Public API: DepthHookProfile, ProfileRegistry, CaptureSession
│   ├── types.rs            # Platform-agnostic DepthFrame / CameraMatrices / Matrix4
│   ├── capture.rs          # CaptureSession lifecycle (start / tick / stop)
│   ├── dx12/
│   │   ├── mod.rs          # Windows-only module, empty on other platforms
│   │   └── hook.rs         # DX12 command-queue hook (scaffold — Windows engineer fills)
│   └── profiles/
│       ├── mod.rs          # DepthHookProfile trait + ProfileRegistry
│       ├── common.rs       # Shared heuristic helpers (reverse-Z extractor, …)
│       └── cyberpunk2077.rs # REDengine 4 profile (first reference implementation)
└── tests/
    ├── test_profile_registry.rs
    └── test_heuristic_serialization.rs
```

- **types** is pure Rust. Compiles on every platform. Depends on `serde` only.
- **profiles** is pure Rust. All the per-title knowledge lives here. Unit
  testable on a Mac developer box.
- **dx12** is `#[cfg(windows)]`-only. On macOS / Linux the module is empty
  and `CaptureSession` becomes a no-op. That keeps CI fast on the Mac.
- **capture** is the thin glue: platform-independent public API, `cfg`-gated
  internals.

## Platform notes

| Platform | Builds? | Behaviour                                                        |
| -------- | ------- | ---------------------------------------------------------------- |
| Windows  | Yes     | Real DX12 hook (to be implemented). Produces `DepthFrame`s.      |
| macOS    | Yes     | Compiles to stubs. `CaptureSession::take_frames()` returns `[]`. |
| Linux    | Yes     | Compiles to stubs. Same as macOS.                                |

Why this matters: the main `gamedata-recorder` dev loop runs on Mac, and we
don't want adding a depth-capture profile to break `cargo check` for any dev
who isn't on Windows. Real DX12 hooking requires `windows-rs` and an
inline-hook engine (`retour` / `detours-rs`), both of which are pulled in
behind `cfg(windows)` in a follow-up commit by the engineer implementing
the hook.

## Public API

```rust
use depth_hook::{ProfileRegistry, CaptureSession};

// 1. Build the registry of known titles.
let registry = ProfileRegistry::with_builtin_profiles();

// 2. Ask: does a profile exist for the currently-foregrounded game?
//    The recorder's existing tokio_thread::get_foregrounded_game already
//    produces a lowercase exe stem, which is what find_for_exe_stem wants.
if let Some(profile) = registry.find_for_exe_stem("cyberpunk2077") {
    // 3. Install the hook for as long as the recording is running.
    let mut session = CaptureSession::start(profile)?;

    // 4. Each recorder tick, drain whatever depth frames the hook has
    //    captured. take_frames() is cheap and non-blocking.
    loop {
        let frames = session.take_frames();
        // pack alongside video + input log
    }

    // Drop session to uninstall the hook.
}
```

## Adding a new profile

Three steps. Example: adding Alan Wake 2 (Northlight engine, DX12).

### 1. Create `src/profiles/alan_wake_2.rs`

```rust
use crate::profiles::{DepthHookProfile, common};
use crate::types::{DepthFormat, DetectionHeuristic, Matrix4};

pub struct AlanWake2;

impl DepthHookProfile for AlanWake2 {
    fn name(&self) -> &'static str { "Alan Wake 2 (Northlight, DX12)" }

    fn game_exe_stems(&self) -> &[&str] { &["alanwake2"] }

    fn detection_heuristic(&self) -> DetectionHeuristic {
        // Northlight renders scene depth at output aspect, one clear per frame.
        DetectionHeuristic::WIDESCREEN_16_9
    }

    fn depth_format(&self) -> DepthFormat { DepthFormat::D32Float }

    fn reverse_z(&self) -> bool { true }

    fn near_far_from_matrix(&self, proj: &Matrix4) -> (f32, f32) {
        common::reverse_z_infinite_far_near(proj)
    }
}
```

### 2. Register it in `src/profiles/mod.rs`

Add to `ProfileRegistry::with_builtin_profiles`:

```rust
let profiles: Vec<Arc<dyn DepthHookProfile>> = vec![
    Arc::new(cyberpunk2077::Cyberpunk2077),
    Arc::new(alan_wake_2::AlanWake2),   // <-- here
];
```

### 3. Confirm the exe stem is whitelisted

Check that `alanwake2` appears in `crates/constants/src/lib.rs`'s
`GAME_WHITELIST`. If the recorder doesn't whitelist the title, the capture
session never starts and the new profile is dead code.

## Heuristic lineage

The "pick the right depth buffer out of the many DSVs a modern renderer
binds per frame" problem is not new. This crate's `DetectionHeuristic`
shape is lifted directly from the **ReShade Generic Depth addon**
(<https://reshade.me/forum/generic-depth-addon>, BSD-licensed). That addon
has been validated on hundreds of titles — including Cyberpunk 2077 — and
its heuristic distils down to four signals:

1. Aspect ratio matches the render target (16:9 typically).
2. Cleared exactly once per frame.
3. Format is typed depth (`D24_UNORM_S8_UINT`, `D32_FLOAT`,
   `D32_FLOAT_S8X24_UINT`).
4. Among remaining candidates, highest `Draw*` call count wins.

We inherit this shape in [`DetectionHeuristic`] and expose each knob so
profiles can override per title where necessary.

## Next steps (for the Windows engineer picking this up)

1. Add `windows` (with `Direct3D12` + `Dxgi` features) and `retour` under
   a `[target.'cfg(windows)'.dependencies]` block in `Cargo.toml`.
2. Fill in `src/dx12/hook.rs` following the plan in the module doc comment:
   - `D3D12CreateDevice` + command-queue vtable read at slot 10.
   - `retour::RawDetour` on `ExecuteCommandLists`.
   - `ClearDepthStencilView` observation + canonical-DSV picker.
   - Readback heap + `CopyResource` + fence wait.
   - Camera matrix read from the profile's declared CB slot (reverse-
     engineering TODO for Cyberpunk 2077).
3. Validate end-to-end on a real Windows 11 box running Cyberpunk 2077 v2.3
   with RenderDoc or PIX side-by-side for ground truth.
4. Once the hook works, add the remaining top-ten profiles: Alan Wake 2,
   Starfield, Black Myth: Wukong, Elden Ring (DX12 mode), Red Dead
   Redemption 2 (Vulkan — separate crate or feature flag).

## License

Same license as the top-level `gamedata-recorder` repo.
