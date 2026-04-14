# GameData Recorder — Architecture Decision Record

> All decisions backed by research in [tech-research.md](tech-research.md)

## 1. Core Architecture

```
┌─────────────────────────────────────────────────────────┐
│ GameData Recorder (Windows native, Rust)                │
├─────────────────────────────────────────────────────────┤
│ Game Detection  → Auto-detect game launch (multi-signal)│
│ Screen Capture  → OBS embedded (GPU hardware encoding)  │
│ Input Capture   → Raw Input API (anti-cheat safe)       │
│ Controller      → XInput + gilrs                        │
│ Post-Process    → 3-layer data (Raw→Trajectory→Action)  │
│ Upload          → S3 multipart (auto, WiFi-only)        │
│ UI              → egui tray + dashboard                 │
└─────────────────────────────────────────────────────────┘
```

## 2. Key Technical Decisions

### Screen Capture: OBS Embedded (not raw DXGI/WGC)
**Why:** OBS handles game capture hooks, encoder negotiation, and edge cases. Writing our own capture pipeline would take months. libobs-rs wraps it safely in Rust.
**Trade-off:** Larger binary (~30MB with OBS DLLs), but much more reliable.

### Input Capture: Raw Input API with RIDEV_INPUTSINK
**Why:** Anti-cheat safe (Vanguard/EAC/BattlEye don't flag it). Sub-millisecond latency. Works in background while game has focus.
**NOT SetWindowsHookEx:** Hooks are flagged by anti-cheat as keylogger signatures. Would get users banned.
**NOT pynput:** Python-level, too slow for frame-accurate timestamps.

### Timestamps: QPC Nanosecond Precision
**Why:** Need sub-frame accuracy for input-to-frame alignment. 30fps = 33.33ms/frame. QPC gives ~100ns resolution.
**Hybrid approach:** QPC for precision + GetMessageTime() for system event correlation.
**Three precision levels:** `elapsed_ns()`, `elapsed_us()`, `elapsed_ms()`

### Encoding: H.265/HEVC (not H.264)
**Why:** Buyer spec requires HEVC. 40-50% smaller files at same quality. All modern GPUs support hardware HEVC encoding.
**Fallback chain:** NVENC HEVC → AMF HEVC → QSV HEVC → x265 (CPU) → x264 (last resort)
**Bitrate:** 10 Mbps CBR (buyer spec: 8-12 Mbps for 1080p@30fps)

### Data Format: 3-Layer Architecture
**Why:** Raw events alone are insufficient for ML training. World model companies need structured trajectories and action labels.
- **Layer 1 (Raw):** JSON Lines events with nanosecond timestamps — `events.jsonl`
- **Layer 2 (Trajectory):** Mouse movements grouped into strokes by click/key/pause — `trajectories.jsonl`
- **Layer 3 (Action Scaffold):** Discrete actions with VLM annotation placeholders — `actions.jsonl`

Inspired by [github.com/Hunterbacon111/Mouse-Keyboard-Time-Series](https://github.com/Hunterbacon111/Mouse-Keyboard-Time-Series)

### Game Detection: Multi-Signal Confidence Scoring
**Signals (additive):**
- Steam/Epic/GOG manifest match: +0.45
- Local game DB (exe name): +0.35
- DX11/DX12 DLL loaded: +0.20
- Vulkan DLL loaded: +0.25
- Fullscreen/borderless: +0.15
- GPU utilization >40%: +0.15
**Threshold:** 0.5 triggers auto-record

### Compression: zstd Level 1
**Why:** 40% better ratio than LZ4, 600 MB/s throughput (far exceeds write rate). 2hr session: 576MB raw → 137MB compressed.
**Streaming mode:** Flush every 1-5 seconds for crash recovery.

### Auto-Record Mode: Zero Manual Operation
**Why:** Target user is ordinary gamer who wants passive income. Any manual step = user drop-off.
**Flow:** Game detected → auto start → game exits → auto stop → auto validate → auto upload → auto earn

## 3. Platform Priority

| Platform | Priority | Why |
|----------|----------|-----|
| Windows | P0 | 75%+ of PC gamers |
| macOS | P1 | ScreenCaptureKit + CGEventTap |
| Android | P2 | Touch capture works perfectly |
| iOS | P3 | No system-level touch API — limited value |

## 4. Backend Architecture

FastAPI + PostgreSQL + S3/R2 hybrid storage.
- Upload: S3 presigned URL multipart
- Storage: S3 ingest → R2 serve (zero egress)
- Processing: Step Functions 8-step pipeline
- Cost: $0.27/hr fully loaded at 10K users

## 5. Unit Economics

- Sell to AI companies: $7.80/hr average
- Pay gamers: $0.50/hr (Tier 1), $1.00/hr (Tier 2 with engine data)
- Gross margin: 72.7%
- Break-even sell-through: 23%

## 6. Buyer Spec Compliance

| Requirement | Status |
|-------------|--------|
| 1080p, 30fps locked CFR | ✅ OBS config |
| H.265/HEVC | ✅ Encoder mapping |
| Motion Blur OFF | ⚠️ User prompt (can't auto-disable) |
| Frame-aligned input logs | ✅ QPC nanosecond timestamps |
| JSON format | ✅ JSON Lines |
| Per-second FPS log | ✅ fps_logger.rs |
| Mouse DPI metadata | ⚠️ User self-report |
| Engine metadata (camera/objects) | 🔜 Phase 3 (BepInEx/godot-rust) |
| Scene classification | 🔜 Backend pipeline (MobileNetV2) |
| Death/menu auto-trim | 🔜 Backend pipeline |
| PII blur | 🔜 Backend pipeline |

## 7. Open Source Foundation

Forked from OWL Control (MIT) by Overworld AI.
- Kept: OBS integration, Raw Input capture, gamepad, upload pipeline, validation
- Added: H.265, JSON Lines, 3-layer data, auto-record, FPS logging, nanosecond timestamps, zstd compression
- Changed: API endpoint, branding, auto-upload behavior
