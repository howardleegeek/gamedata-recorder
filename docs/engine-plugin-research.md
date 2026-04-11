# GameData Labs - Engine Metadata Extraction Plugin Research

## Executive Summary

This document covers technical approaches for building per-frame engine metadata extraction plugins for Godot 4.x, Unity, and Unreal Engine 5. The goal: capture synchronized camera, player, object, environment, and event data alongside gameplay video recordings.

**Recommended primary stack:**
- Godot: GDExtension via godot-rust (`gdext` crate) for performance-critical extraction, with GDScript autoload fallback for rapid prototyping
- Unity: BepInEx + HarmonyX for Mono games; MelonLoader for IL2CPP games; native Rust plugin via `csbindgen` for first-party integrations
- Unreal: C++ plugin using UnrealCV patterns + custom Subsystem; future consideration only
- Serialization: FlatBuffers primary (zero-copy reads), MessagePack fallback (simpler tooling)

---

## 1. GODOT 4.x PLUGIN

### 1.1 GDExtension via godot-rust (gdext) - RECOMMENDED

**Crate:** `godot` (on crates.io), source at [github.com/godot-rust/gdext](https://github.com/godot-rust/gdext)

**What it is:** Rust bindings for Godot 4 using the GDExtension C API. Compiles to a native shared library (.so/.dylib/.dll) that Godot loads at runtime. No engine recompilation needed.

**Engine internals accessible:**
- Full Node tree traversal (`get_tree()`, `get_children()`, recursive scene walking)
- `Camera3D`: `global_transform` (position + rotation as Transform3D), `fov`, `near`, `far`, projection matrix
- `RigidBody3D` / `CharacterBody3D`: `global_position`, `global_rotation`, `linear_velocity`, `angular_velocity`
- `AnimationPlayer`: `current_animation`, `current_animation_position`
- `MeshInstance3D`: `get_aabb()` for bounding boxes, `mesh` resource for geometry
- `DirectionalLight3D` / `OmniLight3D` / `SpotLight3D`: `light_color`, `light_energy`, position, direction
- `WorldEnvironment`: `environment` resource (fog, tonemap, sky, ambient light)
- Physics: `PhysicsDirectSpaceState3D` for raycasts, collision queries
- Signals: connect to any signal for event capture (area_entered, body_entered, etc.)

**Per-frame hook architecture:**

```rust
use godot::prelude::*;
use godot::classes::{Node, Camera3D, RigidBody3D, INode};

#[derive(GodotClass)]
#[class(init, base=Node)]
struct GameDataExtractor {
    base: Base<Node>,
    frame_count: u64,
    // writer handle for output stream
}

#[godot_api]
impl INode for GameDataExtractor {
    fn process(&mut self, _delta: f64) {
        let tree = self.base().get_tree().unwrap();
        let root = tree.get_root().unwrap();

        // Extract camera data
        // Walk scene tree for objects
        // Capture physics state
        // Write frame record

        self.frame_count += 1;
    }
}
```

**Key gdext types for extraction:**
- `Transform3D` -> position (Vector3), rotation (Basis -> Quaternion)
- `Aabb` -> bounding box (position + size)
- `Gd<T>` -> smart pointer to any Godot object
- `SceneTree::get_nodes_in_group()` -> tagged object queries

**Performance considerations:**
- `_process()` runs every render frame; `_physics_process()` runs at fixed physics tick (default 60Hz)
- For 60fps capture: scene tree walk of ~500 objects adds roughly 0.5-1ms per frame in Rust (native speed)
- Major cost is serialization, not data gathering. FlatBuffers write is ~10-50us per frame for typical data
- Use object groups/tags to limit extraction scope (don't walk entire tree)
- Batch writes: buffer N frames in memory, flush to disk on background thread via Rust's `std::thread`
- The gdext Gd<T> cell no longer uses a mutex internally (recent optimization), so object access is fast

**Cargo.toml setup:**
```toml
[dependencies]
godot = { git = "https://github.com/godot-rust/gdext", branch = "master" }
flatbuffers = "24.3"
serde = { version = "1", features = ["derive"] }
# For MessagePack fallback:
rmp-serde = "1"
```

**Crate: `gdext-gen`** - Auto-generates the .gdextension manifest file.

### 1.2 GDScript Autoload - SIMPLE APPROACH

For rapid prototyping or games where you control the source:

```gdscript
# addons/gamedata_extractor/extractor.gd
extends Node

var output_file: FileAccess
var frame_num: int = 0

func _ready():
    output_file = FileAccess.open("user://gamedata.jsonl", FileAccess.WRITE)

func _process(delta):
    var data = {}
    data["frame"] = frame_num
    data["timestamp"] = Time.get_ticks_msec()

    # Camera
    var cam = get_viewport().get_camera_3d()
    if cam:
        data["camera"] = {
            "position": var_to_str(cam.global_position),
            "rotation": var_to_str(cam.global_rotation),
            "fov": cam.fov,
            "near": cam.near,
            "far": cam.far
        }

    # All RigidBody3D objects
    var bodies = get_tree().get_nodes_in_group("tracked_objects")
    var objects = []
    for body in bodies:
        objects.append({
            "name": body.name,
            "position": var_to_str(body.global_position),
            "rotation": var_to_str(body.global_rotation),
            "type": body.get_meta("object_type", "unknown")
        })
    data["objects"] = objects

    output_file.store_line(JSON.stringify(data))
    frame_num += 1
```

**Limitations vs GDExtension:**
- GDScript is ~10-100x slower than Rust for data processing
- Cannot access low-level engine structures (rendering internals, custom physics data)
- JSON serialization is expensive per frame
- No background thread writing (GDScript is single-threaded)
- Suitable for: <100 objects, 30fps capture, prototyping

### 1.3 Godot Mod Distribution

**Godot Mod Loader** ([github.com/GodotModding/godot-mod-loader](https://github.com/GodotModding/godot-mod-loader)):
- General purpose mod loader for GDScript-based Godot games (3.x and 4.x)
- Mods distributed as .zip files containing .pck (Godot resource packs)
- The game developer must integrate the mod loader into their project
- Allows overriding/extending existing scripts and scenes

**Injecting GDExtension into compiled games:**
- **Possible but game-dependent.** A compiled Godot game includes the engine runtime. GDExtensions are loaded from the project's `addons/` or `bin/` directory via `.gdextension` manifest files.
- If the game exports with `res://` filesystem accessible (not encrypted .pck), you can drop a GDExtension library + manifest into the right directory
- If the .pck is encrypted or the game strips GDExtension loading, injection is much harder
- **No standard universal injection mechanism** like BepInEx for Unity. Each game requires analysis.
- Best approach: ship GameData Labs extractor as a Godot Mod Loader compatible mod for games that support it, and as a standalone GDExtension with instructions for games that don't.

---

## 2. UNITY PLUGIN

### 2.1 BepInEx + HarmonyX - PRIMARY APPROACH (Mono games)

**Repository:** [github.com/BepInEx/BepInEx](https://github.com/BepInEx/BepInEx)
**Harmony:** [github.com/BepInEx/HarmonyX](https://github.com/BepInEx/HarmonyX) (BepInEx's fork of Harmony)

**How BepInEx works:**
1. Drop BepInEx files into the game's root directory
2. On game launch, BepInEx's doorstop (Unity Doorstop) hooks into the Mono runtime before the game's assemblies load
3. BepInEx loads its plugin chain, which includes HarmonyX for method patching
4. Plugins (your GameData extractor) run as managed C# code inside the game process

**Accessible Unity APIs for extraction:**

```csharp
// Camera data
Camera cam = Camera.main;
cam.transform.position;        // Vector3
cam.transform.rotation;        // Quaternion
cam.fieldOfView;               // float
cam.nearClipPlane;             // float
cam.farClipPlane;              // float
cam.projectionMatrix;          // Matrix4x4

// Player / agents
GameObject player = GameObject.Find("Player");
player.transform.position;
player.transform.rotation;
Rigidbody rb = player.GetComponent<Rigidbody>();
rb.velocity;                   // Vector3
rb.angularVelocity;           // Vector3
Animator anim = player.GetComponent<Animator>();
anim.GetCurrentAnimatorStateInfo(0); // animation state

// Scene objects (expensive - cache results)
var allRenderers = Object.FindObjectsOfType<Renderer>();
foreach (var r in allRenderers) {
    r.bounds;                  // Bounds (AABB)
    r.gameObject.name;
    r.gameObject.layer;
    r.transform.position;
    r.transform.rotation;
}

// Physics events via Harmony patches
[HarmonyPatch(typeof(Collision))]
// or hook OnCollisionEnter via MonoBehaviour patches

// Lighting
var lights = Object.FindObjectsOfType<Light>();
foreach (var l in lights) {
    l.type;                    // LightType (Directional, Point, Spot)
    l.transform.position;
    l.intensity;
    l.color;
    l.range;
}

// Raycasting for visibility
Physics.Raycast(origin, direction, out hit, maxDistance);
```

**Harmony patch types for event capture:**

```csharp
// Postfix: runs AFTER the original method
[HarmonyPatch(typeof(DoorController), "Open")]
[HarmonyPostfix]
static void OnDoorOpen(DoorController __instance) {
    GameDataLogger.LogEvent("Door_Open", __instance.transform.position);
}

// Prefix: runs BEFORE, can skip original
[HarmonyPatch(typeof(WeaponSystem), "Fire")]
[HarmonyPrefix]
static void OnWeaponFire(WeaponSystem __instance) {
    GameDataLogger.LogEvent("Weapon_Fire", __instance.transform.position);
}

// Transpiler: modify IL code directly (advanced, for injecting extraction calls)
```

**BepInEx plugin skeleton:**

```csharp
[BepInPlugin("com.gamedatalabs.extractor", "GameData Extractor", "1.0.0")]
public class GameDataPlugin : BaseUnityPlugin {
    void Awake() {
        // Harmony patches auto-applied
        Harmony harmony = new Harmony("com.gamedatalabs.extractor");
        harmony.PatchAll();
    }

    void Update() { // runs every frame
        CaptureFrameData();
    }

    void CaptureFrameData() {
        var frame = new FrameData {
            FrameNumber = Time.frameCount,
            Timestamp = Time.realtimeSinceStartup,
            Camera = CaptureCameraData(),
            Objects = CaptureVisibleObjects(),
            // ...
        };
        DataWriter.Write(frame);
    }
}
```

**Minimizing GC pressure:**
- Pre-allocate arrays for object lists; reuse between frames
- Use `struct` instead of `class` for frame data records
- Pool `StringBuilder` instances for any string operations
- Avoid LINQ in hot paths
- Use `FindObjectsOfType<T>()` sparingly - cache and refresh every N frames
- Consider `Physics.OverlapSphereNonAlloc()` instead of allocating variants
- For serialization: MessagePack-CSharp ([github.com/MessagePack-CSharp/MessagePack-CSharp](https://github.com/MessagePack-CSharp/MessagePack-CSharp)) is extremely fast and low-allocation in Unity

### 2.2 MelonLoader - FOR IL2CPP GAMES

**Repository:** [github.com/LavaGang/MelonLoader](https://github.com/lavagang/melonloader)

**How it differs from BepInEx:**
- MelonLoader was designed from day one for IL2CPP support
- Uses Il2CppInterop (formerly Il2CppAssemblyUnhollower) to generate proxy C# assemblies from IL2CPP metadata
- These proxy assemblies let you call IL2CPP game methods as if they were normal C# calls
- BepInEx also supports IL2CPP now (via BepInEx 6.x "Bleeding Edge"), but MelonLoader's IL2CPP support is more mature for many games

**IL2CPP considerations:**
- IL2CPP compiles all C# to C++, then to native code. There is no Mono runtime.
- Game method signatures must be recovered from `global-metadata.dat` + IL2CPP binary
- Tools: `Il2CppDumper` extracts type/method info; MelonLoader automates this
- Reflection-heavy code won't work directly; must use Il2CppInterop proxy types
- String handling differs (Il2CppSystem.String vs System.String)
- Generic types require special handling

**MelonLoader mod structure:**
```csharp
public class GameDataMod : MelonMod {
    public override void OnInitializeMelon() {
        // Setup
    }

    public override void OnUpdate() {
        // Per-frame extraction (runs every frame)
        CaptureFrameData();
    }

    public override void OnSceneWasLoaded(int buildIndex, string sceneName) {
        // Scene transition handling
        LogEvent("Scene_Load", sceneName);
        RefreshObjectCache();
    }
}
```

**BepInEx vs MelonLoader decision matrix:**

| Factor | BepInEx | MelonLoader |
|--------|---------|-------------|
| Mono games | Excellent | Good |
| IL2CPP games | Good (6.x BE) | Excellent |
| Community/mods | Larger ecosystem | Smaller but dedicated |
| Harmony support | Built-in HarmonyX | Built-in HarmonyX |
| Setup complexity | Drop files | Installer or drop files |
| Game compatibility | Very wide | Very wide |
| Documentation | Better | Adequate |

**Recommendation:** Support both. Ship a BepInEx plugin and a MelonLoader mod from the same core extraction library.

### 2.3 Unity Native Plugin (Rust)

**For first-party game integrations where the developer cooperates:**

**Unity Native Plugin Interface** allows loading C-compatible shared libraries that can:
- Access Unity's low-level rendering pipeline (IUnityGraphics)
- Hook into rendering events (GL.IssuePluginEvent)
- Run on a separate thread for zero main-thread impact

**Rust integration via csbindgen:**
- Crate: `csbindgen` ([github.com/Cysharp/csbindgen](https://github.com/Cysharp/csbindgen))
- Auto-generates C# `[DllImport]` bindings from Rust `extern "C"` functions
- Your Rust library handles: serialization (FlatBuffers), buffering, disk I/O, compression
- C# side handles: Unity API calls (Camera, Transform, etc.) and passes data to Rust

**Architecture:**
```
Unity C# (data gathering) -> [DllImport] -> Rust dylib (serialization + I/O)
                                              |
                                              v
                                     FlatBuffers file on disk
```

**Rust side:**
```rust
#[no_mangle]
pub extern "C" fn write_frame_data(
    frame_num: u64,
    cam_pos_x: f32, cam_pos_y: f32, cam_pos_z: f32,
    cam_rot_x: f32, cam_rot_y: f32, cam_rot_z: f32, cam_rot_w: f32,
    cam_fov: f32,
    // ... more fields
) {
    // Build FlatBuffer, write to buffered output
}
```

**Crate: `testdouble/rust-ffi-example`** ([github.com/testdouble/rust-ffi-example](https://github.com/testdouble/rust-ffi-example)) - Reference implementation for Rust<->Unity FFI.

### 2.4 UnityExplorer - DEBUGGING / REVERSE ENGINEERING

**Repository:** [github.com/sinai-dev/UnityExplorer](https://github.com/sinai-dev/UnityExplorer)

Essential tool for developing extraction plugins:
- In-game UI for browsing the full Unity object hierarchy at runtime
- Works with both BepInEx and MelonLoader
- Supports Mono and IL2CPP
- Use it to discover: object names, component types, field values, hierarchy structure
- Critical for reverse-engineering what data is available in any given game

### 2.5 AssetRipper / UnityPy - OFFLINE ANALYSIS

**AssetRipper** ([github.com/AssetRipper/AssetRipper](https://github.com/AssetRipper/AssetRipper)):
- Extracts and reconstructs Unity projects from compiled builds
- Supports Unity 3.5.0 through 6000.5.x
- Useful for: understanding scene structure, identifying object types, mapping class hierarchies before writing extraction code
- Does NOT help with runtime data extraction, but invaluable for planning what to extract

---

## 3. UNREAL ENGINE 5 (Future Consideration)

### 3.1 C++ Plugin / Subsystem Approach

UE5 plugins are C++ modules that compile against the engine. For data extraction:

**Game Instance Subsystem** (recommended pattern):
```cpp
UCLASS()
class UGameDataSubsystem : public UGameInstanceSubsystem, public FTickableGameObject {
    GENERATED_BODY()
public:
    virtual void Tick(float DeltaTime) override {
        CaptureFrameData();
    }

    void CaptureFrameData() {
        // Camera
        APlayerCameraManager* CamMgr = GetWorld()->GetFirstPlayerController()->PlayerCameraManager;
        FVector CamPos = CamMgr->GetCameraLocation();
        FRotator CamRot = CamMgr->GetCameraRotation();
        float FOV = CamMgr->GetFOVAngle();

        // All actors
        for (TActorIterator<AActor> It(GetWorld()); It; ++It) {
            AActor* Actor = *It;
            FVector Pos = Actor->GetActorLocation();
            FRotator Rot = Actor->GetActorRotation();
            FBox Bounds = Actor->GetComponentsBoundingBox();
            // ...
        }
    }
};
```

**Accessible data:**
- `APlayerCameraManager`: position, rotation, FOV, aspect ratio
- `AActor` / `APawn` / `ACharacter`: transforms, velocity, physics state
- `USkeletalMeshComponent`: animation state, bone transforms
- `ULightComponent` subclasses: all lighting parameters
- `UAbilitySystemComponent` (GAS): active abilities, gameplay effects, attribute values
- `FPhysScene`: collision events, physics simulation state
- Level streaming: `ULevelStreaming::GetLoadedLevel()` for tracking loaded sublevels

**Gameplay Ability System (GAS) hooks:**
- `UAbilitySystemComponent::OnAbilityActivated` delegate
- `UAbilitySystemComponent::OnGameplayEffectApplied` delegate
- Gameplay Tags provide natural event classification

### 3.2 UnrealCV - RESEARCH REFERENCE

**Repository:** [github.com/unrealcv/unrealcv](https://github.com/unrealcv/unrealcv)

UnrealCV provides a Python/MATLAB API to UE for computer vision research:
- Captures: RGB, depth, surface normals, object segmentation masks
- Commands: `vget /camera/0/location`, `vget /camera/0/rotation`, `vget /camera/0/depth`
- TCP socket interface (port 9000) for external tools
- Supports UE5 (5.2+ tested, 5.6 recommended)
- **Directly relevant architecture pattern** for GameData Labs: UnrealCV's approach of registering a UE plugin that exposes engine data via a communication channel is exactly the pattern to follow

### 3.3 Modding UE5 Games (External)

UE5 modding is harder than Unity:
- No universal mod loader like BepInEx
- Some games support mods via UGC (User Generated Content) or custom mod loading
- DLL injection is possible but game-specific and often blocked by anti-cheat
- **Recommendation:** For UE5, focus on first-party SDK integration (cooperative with game developers) rather than universal injection

---

## 4. SERIALIZATION FORMAT COMPARISON

### Per-Frame Data Size Estimate

Typical frame: 1 camera + 1 player + 50 visible objects + 5 lights + 3 events
Approximate raw data: ~8-12 KB per frame

At 60fps: ~480-720 KB/s = ~28-43 MB/min = ~1.7-2.6 GB/hour

### Format Benchmarks

| Format | Serialize Speed | Deserialize Speed | Size (vs JSON) | Schema Required | Rust Crate | C# Library |
|--------|----------------|-------------------|-----------------|-----------------|------------|-------------|
| **FlatBuffers** | Medium | Fastest (zero-copy) | ~40% of JSON | Yes (.fbs) | `flatbuffers` | `Google.FlatBuffers` |
| **MessagePack** | Fastest | Fast | ~50% of JSON | No | `rmp-serde` | `MessagePack-CSharp` |
| **Protobuf** | Fast | Fast | ~35% of JSON | Yes (.proto) | `prost` | `Google.Protobuf` |
| **Cap'n Proto** | Fastest (zero-copy) | Fastest (zero-copy) | ~45% of JSON | Yes (.capnp) | `capnp` | Limited support |
| **JSON** | Slow | Slow | Baseline (100%) | No | `serde_json` | `Newtonsoft.Json` |
| **Custom Binary** | Fastest | Fastest | Smallest | Custom | Manual | Manual |

### Recommendation: FlatBuffers Primary, MessagePack Secondary

**FlatBuffers for production:**
- Zero-copy deserialization is critical for downstream ML pipelines reading terabytes of frame data
- Schema provides strong typing and versioning (add fields without breaking readers)
- Excellent Rust (`flatbuffers` crate) and C# (`Google.FlatBuffers` NuGet) support
- Google designed it specifically for game development (used in Android game SDK)

**MessagePack for development/debugging:**
- No schema needed - faster iteration during development
- `MessagePack-CSharp` is the fastest serializer for Unity (by Cysharp, same team as `csbindgen`)
- Human-inspectable with `msgpack2json` tools
- Good fallback for games where FlatBuffers overhead isn't justified

**Schema example (FlatBuffers .fbs):**
```fbs
namespace GameDataLabs;

struct Vec3 {
  x: float;
  y: float;
  z: float;
}

struct Quat {
  x: float;
  y: float;
  z: float;
  w: float;
}

table CameraData {
  position: Vec3;
  rotation: Quat;
  fov: float;
  near_clip: float;
  far_clip: float;
}

table ObjectData {
  id: uint32;
  name: string;
  object_type: string;
  position: Vec3;
  rotation: Quat;
  bbox_min: Vec3;
  bbox_max: Vec3;
  velocity: Vec3;
  state_flags: uint32;
}

table LightData {
  light_type: uint8; // 0=directional, 1=point, 2=spot
  position: Vec3;
  direction: Vec3;
  color: Vec3;
  intensity: float;
  range: float;
}

table EventData {
  event_type: string;
  position: Vec3;
  actor_id: uint32;
  target_id: uint32;
  metadata: string; // JSON for flexible event data
}

table FrameRecord {
  frame_number: uint64;
  timestamp_ms: float64;
  camera: CameraData;
  player_position: Vec3;
  player_rotation: Quat;
  player_velocity: Vec3;
  player_animation_state: string;
  objects: [ObjectData];
  lights: [LightData];
  events: [EventData];
  scene_name: string;
}

table RecordingSession {
  version: uint16;
  game_name: string;
  engine: string; // "godot4", "unity", "ue5"
  start_time_utc: string;
  target_fps: uint16;
  frames: [FrameRecord];
}

root_type RecordingSession;
```

### File Format Strategy

**During recording:** Write individual `FrameRecord` FlatBuffers as length-prefixed messages to a streaming binary file (`.gdf` - GameData Frames):

```
[4 bytes: frame size][FlatBuffer bytes][4 bytes: frame size][FlatBuffer bytes]...
```

**Post-recording:** Optionally compress with LZ4 (fast) or Zstandard (better ratio) for archival. LZ4 crate: `lz4_flex`. Zstd crate: `zstd`.

**Sidecar metadata file** (JSON): recording session info, game name, resolution, target fps, total frames, timestamps.

---

## 5. EDGE CASES AND CHALLENGES

### 5.1 Anti-Cheat Compatibility

| Anti-Cheat | BepInEx | MelonLoader | Impact |
|-----------|---------|-------------|--------|
| **EasyAntiCheat (EAC)** | Blocked in online modes | Blocked in online modes | Kernel-mode driver prevents DLL injection |
| **BattlEye** | Blocked | Blocked | Similar kernel-mode protection |
| **Denuvo Anti-Tamper** | Usually works (DRM, not anti-cheat) | Usually works | Protects executable, not runtime |
| **Vanguard (Riot)** | Blocked | Blocked | Ring-0 always-on driver |
| **No anti-cheat** | Works | Works | Most indie/single-player games |

**Mitigation strategies:**
- Target offline/single-player modes (most games disable anti-cheat for offline play)
- Work with game developers for whitelisted integrations
- Use replay files where available (many competitive games export replays)
- For online games: capture only via screen recording + computer vision (no injection)

### 5.2 IL2CPP Obfuscation

- IL2CPP itself acts as mild obfuscation (no C# assemblies to decompile)
- Additional obfuscators (Beebyte, .NET Reactor) can strip metadata
- `Il2CppDumper` recovers most type info from `global-metadata.dat`
- Heavily obfuscated games: use UnityExplorer at runtime to discover types manually
- Some games strip `global-metadata.dat` entirely - these are effectively unmoddable without significant RE effort

### 5.3 Thread Safety

**Godot:**
- Scene tree is NOT thread-safe. All node access must happen on the main thread.
- Use `_process()` for extraction (runs on main thread), offload I/O to background thread via channels

**Unity:**
- Unity API is main-thread only (most calls). `Transform`, `Camera`, `GameObject` must be read on main thread.
- Pattern: read all data in `Update()`, serialize and write on a worker thread
- Use `NativeArray<T>` for lock-free data passing between threads

**Unreal:**
- Game thread owns all UObject access. Similar to Unity.
- Use `AsyncTask` or `FRunnable` for background I/O
- `FTickableGameObject::Tick()` runs on game thread

### 5.4 Scene Transitions / Loading Screens

- **Unity:** Hook `SceneManager.sceneLoaded` event + BepInEx `OnSceneWasLoaded`
- **Godot:** Connect to `SceneTree.tree_changed` signal
- **All engines:** Insert a `Scene_Transition` event into the data stream; downstream consumers use this as a segment boundary
- During loading: frame data may be empty or partial. Mark these frames with a `loading: true` flag
- Object caches must be invalidated on scene change

### 5.5 Performance Budget

Target: <2ms per frame overhead (3.3% of 60fps frame budget)

| Operation | Typical Cost |
|-----------|-------------|
| Scene tree walk (100 objects) | 0.1-0.3ms |
| Transform reads (100 objects) | 0.05-0.1ms |
| Bounding box computation | 0.1-0.2ms |
| FlatBuffer serialization | 0.01-0.05ms |
| File write (buffered) | 0.01-0.02ms |
| **Total** | **~0.3-0.7ms** |

For 500+ objects, use spatial hashing or frustum culling to limit extraction scope. Only extract objects visible to the camera or within a configurable radius.

### 5.6 Custom Engines

Games not built on Godot/Unity/Unreal have no universal injection path. Options:
- Screen capture + computer vision (engine-agnostic but loses precision)
- Memory scanning (fragile, game-version-specific)
- If the game has modding support, use its native API
- If the game has replay files, parse those instead

---

## 6. EXISTING REFERENCES AND PRIOR ART

### 6.1 OpenAI Video PreTraining (VPT)
- Captured Minecraft gameplay from contractors: video + keyboard/mouse inputs
- Trained an Inverse Dynamics Model (IDM) to predict actions from video
- Used IDM to label 70,000 hours of public Minecraft videos
- **Key insight:** They used contractor-recorded paired data (video + actions) to bootstrap labeling of unlabeled video. GameData Labs' engine-level extraction is far richer than what VPT captured.

### 6.2 NVIDIA Isaac Sim Replicator
- Synthetic data generation for robotics training
- Exports: RGB, depth, 2D/3D bounding boxes, segmentation masks, surface normals
- Uses annotators + writers architecture (pluggable output formats)
- `BasicWriter` outputs to disk; custom writers can stream to any destination
- `CosmosWriter` captures synchronized multi-modal data
- **Architecture pattern to adopt:** Annotator (what to capture) + Writer (how to serialize) separation

### 6.3 Minecraft ReplayMod
- Records packets + player state per tick
- ReplayFPS addon captures client-side camera position for first-person replay
- Distributed as a Fabric/Forge mod (Java)
- Demonstrates the per-frame recording + playback model

### 6.4 pyLoL (League of Legends)
- Extracts spatiotemporal player positions from game videos using computer vision
- Fallback approach when engine-level access isn't possible

### 6.5 UnrealCV
- Python/MATLAB client -> TCP -> UE5 plugin server
- Captures: camera params, depth, segmentation, normals, object masks
- Used in dozens of computer vision research papers
- **Direct architectural reference** for the GameData Labs UE5 plugin

---

## 7. RECOMMENDED ARCHITECTURE

### Plugin Core (Rust library, shared across engines)

```
gamedata-core/          # Rust crate
  src/
    lib.rs              # FFI exports + shared logic
    schema.fbs          # FlatBuffers schema
    writer.rs           # Buffered frame writer (FlatBuffers + LZ4)
    session.rs          # Recording session management
    events.rs           # Event classification/encoding
```

### Engine-Specific Adapters

```
gamedata-godot/         # GDExtension (Rust, depends on gdext + gamedata-core)
  src/
    extractor.rs        # Node that hooks _process(), reads scene tree
    mod.rs              # GDExtension entry point

gamedata-unity/         # C# (BepInEx plugin + MelonLoader mod)
  src/
    Plugin.cs           # BepInEx entry point
    MelonMod.cs         # MelonLoader entry point
    FrameCapture.cs     # Unity API calls for data extraction
    NativeBinding.cs    # [DllImport] to gamedata-core Rust lib
    HarmonyPatches.cs   # Event hooks (door open, weapon fire, etc.)

gamedata-unreal/        # C++ (UE5 plugin, future)
  Source/
    GameDataSubsystem.h/cpp
    FrameCapture.h/cpp
```

### Data Flow

```
Engine Frame Tick
  -> Engine Adapter (read transforms, states, events)
  -> gamedata-core Rust lib (serialize to FlatBuffers, buffer, compress)
  -> Disk: .gdf file (streaming binary)
  -> Sidecar: .json metadata

Simultaneously:
  Video Recorder (OBS / engine built-in)
  -> .mp4 file

Post-recording:
  Synchronize .gdf + .mp4 by timestamp alignment
```

### Key Crates Summary

| Purpose | Crate | Notes |
|---------|-------|-------|
| Godot bindings | `godot` (gdext) | github.com/godot-rust/gdext |
| GDExtension manifest gen | `gdext-gen` | crates.io/crates/gdext-gen |
| C# FFI generation | `csbindgen` | github.com/Cysharp/csbindgen |
| FlatBuffers | `flatbuffers` | Primary serialization |
| MessagePack | `rmp-serde` | Secondary / debug serialization |
| LZ4 compression | `lz4_flex` | Fast compression for streaming |
| Zstandard compression | `zstd` | Better ratio for archival |
| Async I/O | `tokio` or `crossbeam-channel` | Background write thread |

| C# Library | NuGet Package | Notes |
|------------|---------------|-------|
| MessagePack | `MessagePack-CSharp` | Fastest C# serializer, Unity-compatible |
| FlatBuffers | `Google.FlatBuffers` | Official Google package |
| Unity modding | `BepInEx` | Drop-in mod framework |
| IL2CPP modding | `MelonLoader` | Best IL2CPP support |
| Method patching | `HarmonyX` | Included in both BepInEx and MelonLoader |
| Runtime exploration | `UnityExplorer` | Essential development tool |

---

## 8. IMPLEMENTATION PRIORITY

1. **Unity BepInEx plugin** - largest addressable market (most games are Unity)
2. **Unity MelonLoader mod** - same core logic, IL2CPP coverage
3. **Godot GDExtension** - growing market, cleaner architecture
4. **gamedata-core Rust lib** - shared serialization/IO layer
5. **Unreal C++ plugin** - highest effort, evaluate after Unity+Godot ship
