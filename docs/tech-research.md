# GameData Labs — PC/Mac/Mobile Recorder 技术选型报告

> 核心原则：**对用户来说要够简单** — 下载 → 安装 → 玩游戏 → 自动录制 → 自动上传 → 自动到账
> 语言：Rust | 日期：2026-04-07

---

## 一、总体推荐方案

| 平台 | 优先级 | 录屏方案 | 输入捕获 | 编码 | 状态 |
|------|--------|---------|---------|------|------|
| **Windows** | P0 | windows-capture (WGC + DXGI DD) | Raw Input API | ffmpeg-next (hevc_nvenc/amf/qsv) | 先做 |
| **macOS** | P1 | ScreenCaptureKit (screencapturekit crate) | CGEventTap (core-graphics crate) | VideoToolbox (objc2-video-toolbox) | 第二 |
| **Android** | P2 | MediaProjection + VirtualDisplay | 透明 Overlay + MotionEvent | MediaCodec H.265 | 第三 |
| **iOS** | P3 | ReplayKit Broadcast Extension | ⚠️ 无系统级 API (见 Edge Case) | VideoToolbox | 最后 |

---

## 二、Windows 方案 (P0)

### 2.1 录屏：windows-capture crate

**选择理由：**
- 同时支持 WGC (Windows Graphics Capture) 和 DXGI Desktop Duplication 双后端
- GPU 纹理直出 (ID3D11Texture2D)，零 CPU 拷贝
- v2.0 已内置硬件加速编码
- MIT 协议，467 stars，活跃维护
- 覆盖 90%+ 游戏 (borderless/windowed)

**方案对比：**

| 方案 | CPU 开销 | 全屏独占 | 反作弊 | Rust 生态 | 推荐度 |
|------|---------|---------|--------|----------|-------|
| WGC (windows-capture) | ~0% | ❌ 不支持 | ✅ 安全 | ✅ 成熟 | ⭐⭐⭐⭐⭐ |
| DXGI Desktop Dup (windows-capture) | ~0% | ❌ 不支持 | ✅ 安全 | ✅ 成熟 | ⭐⭐⭐⭐ |
| OBS Hook 注入 (obs-rs) | ~0% | ✅ 支持 | ⚠️ 可能被拦 | ⚠️ 停更 | ⭐⭐⭐ |
| FFmpeg ddagrab | ~0% | ❌ 不支持 | ✅ 安全 | ✅ ffmpeg-next | ⭐⭐⭐ |
| NVFBC | ~0% | ✅ 支持 | ✅ 安全 | ❌ 消费级 GPU 锁定 | ❌ 不可用 |

**结论：** windows-capture (WGC 主 + DXGI DD 备)。放弃全屏独占支持——现代游戏默认都是 borderless fullscreen。

### 2.2 输入捕获：Raw Input API

**选择理由：**
- 亚毫秒延迟，比 Low-level hooks 更快
- `RIDEV_INPUTSINK` 标志允许后台接收所有输入
- **不触发任何反作弊系统** (BattlEye, EAC, Vanguard 都不拦)
- 原始未加速数据 (unaccelerated delta counts)

**方案对比：**

| 方案 | 延迟 | 全屏工作 | 反作弊 | 推荐度 |
|------|------|---------|--------|-------|
| Raw Input (windows crate) | <1ms | ✅ | ✅ 安全 | ⭐⭐⭐⭐⭐ |
| SetWindowsHookEx | ~1ms | ✅ | ❌ 被检测 | ❌ 禁用 |
| rdev crate | ~1ms | ✅ | ❌ 底层用 hooks | ❌ 禁用 |

**手柄支持：**
- Xbox 手柄: `rusty-xinput` (直接 XInput polling, 1ms)
- 其他手柄: `gilrs` (DirectInput/HID, 关闭 wgi feature)

**鼠标 DPI：**
- Windows 没有 API 查硬件 DPI，只能让用户在 App 设置里填
- 记录 Raw Input 的原始 delta counts，后处理时对齐

### 2.3 编码：ffmpeg-next (多 GPU 厂商)

| 方案 | NVIDIA | AMD | Intel | 推荐度 |
|------|--------|-----|-------|-------|
| ffmpeg-next (hevc_nvenc/amf/qsv) | ✅ | ✅ | ✅ | ⭐⭐⭐⭐⭐ |
| nvidia-video-codec-sdk | ✅ | ❌ | ❌ | ⭐⭐⭐ |
| rsmpeg (ByteDance) | ✅ | ✅ | ✅ | ⭐⭐⭐⭐ |

**选择 ffmpeg-next** — 自动检测用户 GPU，选最优编码器。rsmpeg 作为备选 (MIT 协议更清晰)。

### 2.4 Windows Cargo.toml

```toml
[dependencies]
windows-capture = "1.5"           # WGC + DXGI DD 录屏
windows = { version = "0.58", features = [
    "Win32_UI_Input",
    "Win32_UI_WindowsAndMessaging",
    "Win32_System_Performance",
    "Win32_Devices_HumanInterfaceDevice",
]}
rusty-xinput = "1.3"              # Xbox 手柄
gilrs = { version = "0.11", default-features = false, features = ["xinput"] }
ffmpeg-next = "7"                 # H.265 硬件编码 (NVENC/AMF/QSV)
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

---

## 三、macOS 方案 (P1)

### 3.1 录屏：ScreenCaptureKit

- `screencapturekit` crate (v1.5, MIT, 活跃)
- GPU 加速，IOSurface 零拷贝直出
- macOS 12.3+ 要求 (覆盖所有还在用的 Mac)
- Apple Silicon 上编码走 Media Engine 专用硬件，不占 GPU/CPU

### 3.2 输入捕获：CGEventTap

- `core-graphics` crate (v0.25) 内置完整支持
- 系统级键鼠事件拦截
- 需要 Accessibility 权限 (系统设置 → 隐私 → 辅助功能)
- 手柄: `gilrs` (GCController 后端, 支持 Xbox/PS/MFi)

### 3.3 编码：VideoToolbox

- `objc2-video-toolbox` (v0.3)
- Apple Silicon Media Engine: 1080p30 HEVC 仅用 ~10% 编码能力
- IOSurface → CVPixelBuffer → VTCompressionSession，全程零 CPU 拷贝
- 备选: ffmpeg-next 的 `hevc_videotoolbox` 编码器

### 3.4 分发

- **不用 App Sandbox** (CGEventTap 不兼容沙盒)
- **必须 Notarization** (避免 Gatekeeper 警告)
- 以 .dmg 或直接下载 .app 分发

### 3.5 macOS Cargo.toml

```toml
[dependencies]
screencapturekit = "1.5"          # ScreenCaptureKit 录屏
core-graphics = "0.25"            # CGEventTap 键鼠
objc2-video-toolbox = "0.3"       # VideoToolbox H.265
objc2-core-media = "0.3"
objc2-core-video = "0.3"
objc2 = "0.6"
gilrs = "0.11"                    # 手柄
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

---

## 四、Android 方案 (P2)

### 4.1 录屏：MediaProjection API

- 标准 Android 录屏 API (5.0+)，所有主流录屏 App 都用这个
- MediaProjection → VirtualDisplay → Surface → MediaCodec，GPU 零拷贝
- 必须前台 Service + 用户每次授权

### 4.2 输入捕获：透明 Overlay + MotionEvent

- `TYPE_APPLICATION_OVERLAY` + `SYSTEM_ALERT_WINDOW` 权限
- `dispatchTouchEvent(MotionEvent)` 拿精确坐标 + 压力 + 多点触控 + ms 时间戳
- 手柄: Android InputDevice API (`onKeyDown` / `onGenericMotionEvent`)

### 4.3 编码：MediaCodec H.265

- 2016+ 旗舰 / 2018+ 中端机均支持硬件 H.265
- 运行时 `MediaCodecList` 检测，不支持则回退 H.264
- 1080p30 H.265: ~3-5% 额外耗电

### 4.4 Rust 集成

- `cargo-ndk` 交叉编译 → `.so` → JNI 桥接
- 推荐 `uniffi` 自动生成 Kotlin 绑定
- Rust 负责: 数据序列化/压缩/上传管道。Kotlin 负责: UI/Service/权限

---

## 五、iOS 方案 (P3)

### 5.1 录屏：ReplayKit Broadcast Extension

- 系统级全屏录制
- 用户必须手动从控制中心启动
- Extension 运行在独立进程，**50MB 内存限制**
- 可能被 iOS 随时杀掉

### 5.2 输入捕获：⚠️ 严重受限

**iOS 没有系统级触控捕获 API。** 三个替代方案:

| 方案 | 精度 | 可行性 | 采用难度 |
|------|------|--------|---------|
| SDK 集成 (游戏内嵌 SDK) | 完美 | 需要游戏开发者配合 | 高 |
| 视觉触控指示器 + CV 后处理 | ~10-20px 误差 | 通用但不精确 | 中 |
| 不采集 iOS 触控数据 | N/A | 只卖视频数据 | 低 |

**推荐：Phase 1 先不做 iOS 触控，只做视频录制。**

### 5.3 编码 & Rust 集成

- VideoToolbox (所有 iPhone 6S+ 支持 H.265)
- Rust: `cargo-lipo` + `uniffi` 生成 Swift 绑定
- 注意 50MB Extension 内存限制，Rust binary 约 2-5MB

---

## 六、开源参考项目

| 项目 | Stars | 语言 | 用途 | 协议 |
|------|-------|------|------|------|
| [Cap](https://github.com/CapSoftware/Cap) | 17.9k | Rust/TS | 完整录屏 App (Tauri) | AGPL |
| [scap](https://github.com/CapSoftware/scap) | 595 | Rust | 跨平台录屏库 (最佳抽象) | MIT ✅ |
| [windows-capture](https://github.com/NiiightmareXD/windows-capture) | 467 | Rust | Windows WGC+DXGI 录屏 | MIT ✅ |
| [obs-rs](https://github.com/not-matthias/obs-rs) | 167 | Rust | OBS Hook 游戏捕获 | MIT ✅ |
| [RustDesk](https://github.com/rustdesk/rustdesk) | 111k | Rust | 远程桌面 (含 NVENC) | AGPL |
| [OBS Studio](https://github.com/obsproject/obs-studio) | ~65k | C | 架构参考标杆 | GPL-2.0 |
| [gpu-screen-recorder](https://git.dec05eba.com/gpu-screen-recorder/) | - | C | 零拷贝 GPU 管道参考 | GPL-3.0 |
| [ffmpeg-next](https://github.com/zmwangx/rust-ffmpeg) | 1.9k | Rust | FFmpeg 绑定 (编码) | WTFPL |
| [rsmpeg](https://github.com/larksuite/rsmpeg) | 863 | Rust | FFmpeg 绑定 (ByteDance) | MIT ✅ |
| [hylarana](https://github.com/mycrl/hylarana) | 71 | Rust | 跨平台投屏 SDK | MIT ✅ |

**可直接复用 (MIT):** scap, windows-capture, obs-rs, rsmpeg, hylarana
**仅作参考 (GPL/AGPL):** OBS, RustDesk, Cap, gpu-screen-recorder

---

## 七、Edge Cases 完整清单

### 7.1 录屏相关

| Edge Case | 平台 | 影响 | 解决方案 |
|-----------|------|------|---------|
| 全屏独占模式 (Exclusive Fullscreen) | Windows | WGC/DXGI 无法捕获 | 现代游戏默认 borderless，不支持则提示用户切换 |
| DWM 被绕过 | Windows | 捕获失败 | 检测后自动切 DXGI DD 后端 |
| 黄色边框 (WGC Win10) | Windows | 录制时屏幕有黄框 | Win11 22H2+ 可通过 `IsBorderRequired=false` 消除；Win10 无解但不影响录制内容 |
| 多显示器 | Windows/Mac | 录错屏幕 | 自动检测游戏窗口所在显示器 |
| HDR 内容 | Windows | 色彩空间不匹配 | WGC 支持 HDR (需开 HAGS)；编码时转 SDR 或保留 HDR10 |
| macOS 权限弹窗 | macOS | Sequoia 定期弹窗要求重新授权 | 无法规避，引导用户授权 + 检测权限状态 |
| macOS CGEventTap 静默失败 | macOS | Accessibility 未授权时回调不触发，无报错 | 启动时 `AXIsProcessTrusted()` 检测，未授权则引导 |
| macOS App Sandbox 不兼容 | macOS | CGEventTap 在沙盒内无法工作 | 不使用沙盒，走 notarization + .dmg 分发 |
| IOSurface 像素格式不匹配 | macOS | 编码器报错 | ScreenCaptureKit 默认 NV12，VideoToolbox 原生支持，不要转 BGRA |
| Android FLAG_SECURE | Android | 游戏窗口录制为黑屏 | 极少数游戏用这个，检测后告知用户 |
| Android 14+ 单次令牌 | Android | MediaProjection 不能跨 session 复用 | 每次录制重新请求授权 |
| Android 15 自动隐藏敏感内容 | Android | 通知/密码被自动遮挡 | 这是好事，不需要额外 PII 处理 |
| ReplayKit 50MB 内存限制 | iOS | Extension 超限被杀 | Rust binary 控制在 5MB 内，最小化内存分配 |
| ReplayKit 后台被杀 | iOS | 录制中断 | 检测中断事件，通知用户重新开始 |
| 游戏检测 Overlay 拒绝运行 | Android | 部分竞技游戏 (Fortnite, PUBG) | 提供 "被动模式" (关闭 overlay, 仅录视频) |

### 7.2 输入捕获相关

| Edge Case | 平台 | 影响 | 解决方案 |
|-----------|------|------|---------|
| 反作弊检测输入 hook | Windows | 游戏崩溃/封号 | 用 Raw Input (不被检测)，绝不用 SetWindowsHookEx |
| 鼠标 DPI 未知 | Windows | 无法还原真实移动距离 | App 设置页让用户填 DPI，或自动检测常见品牌软件 |
| 输入时间戳与视频帧对齐 | 全平台 | 输入事件和视频帧时间不同步 | 统一用 QPC (Win) / mach_absolute_time (Mac) 打时间戳，后处理对齐到最近帧 |
| 手柄热插拔 | 全平台 | 录制中手柄断连/重连 | 监听设备变更事件，断连时暂停手柄录制，重连时恢复 |
| 多设备同时输入 | Windows | 键盘+鼠标+手柄同时操作 | Raw Input 天然支持多设备分流 |
| iOS 无系统级触控 API | iOS | 完全无法采集触控数据 | Phase 1 不做 iOS 触控，仅视频 |
| Android Overlay 被安全策略拦截 | Android | 部分 ROM 限制 overlay 权限 | 运行时检测 + 引导用户开启 |
| 虚拟键盘输入 | 移动端 | 软键盘事件捕获不一致 | 标记为 virtual_keyboard 事件类型 |

### 7.3 编码相关

| Edge Case | 平台 | 影响 | 解决方案 |
|-----------|------|------|---------|
| 无独显 (集成显卡) | Windows | 没有 NVENC/AMF | ffmpeg-next 自动回退 Intel QSV 或 CPU 软编码 |
| 老旧 GPU 不支持 H.265 | Windows | 编码失败 | 运行时检测，回退 H.264 |
| 编码器饱和 (同时录制+游戏) | 全平台 | 帧率下降 | 限制录制码率 (8Mbps CBR)，优先保游戏帧率 |
| 游戏帧率低于 30fps | 全平台 | 录制帧数不足 | 以游戏实际帧率录制，metadata 标记真实帧率 |
| VFR (可变帧率) | 全平台 | 后处理对齐困难 | 强制 CFR (恒定帧率)，丢帧/补帧到 30fps |
| Apple Silicon vs Intel Mac | macOS | 编码器能力差异大 | 运行时检测芯片类型，Intel 降低编码参数 |

### 7.4 存储 & 上传相关

| Edge Case | 平台 | 影响 | 解决方案 |
|-----------|------|------|---------|
| 磁盘空间不足 | 全平台 | 录制中断 | 启动前检测可用空间 (最少 2GB)，录制中实时监控 |
| 上传中断 | 全平台 | 数据丢失 | 分块上传 + SHA-256 校验 + 断点续传 |
| WiFi → 移动网络切换 | 移动端 | 上传中断 | 默认仅 WiFi 上传，网络切换时暂停 |
| 低电量 | 移动端 | 系统杀后台 | <15% 自动停止录制，提示用户 |
| 大文件 (2小时 = ~5GB) | 全平台 | 上传慢/失败 | 录制时分段 (每 5 分钟一个文件)，并行上传已完成段 |
| 上传后本地清理 | 全平台 | 占满磁盘 | 服务端确认收到后 48h 自动删除本地文件 |
| 来电打断录制 | 移动端 | 录制中断 | 检测电话状态，自动暂停/恢复 |

### 7.5 用户体验相关 (核心原则: 要够简单)

| Edge Case | 影响 | 解决方案 |
|-----------|------|---------|
| 用户不懂分辨率/帧率设置 | 配置错误 | **全自动检测**：检测游戏分辨率，自动设为 1080p30 |
| 用户不知道何时该录 | 不录制 | **自动检测游戏启动**，后台开始录制 |
| 用户忘记停止录制 | 录了菜单/桌面 | **自动检测游戏退出**，停止录制 |
| 游戏内死亡/Loading 画面 | 垃圾数据 | **帧相似度检测** (>95% 相似 = 静态画面)，自动裁剪 |
| 用户不想被看到通知/密码 | 隐私泄露 | PII 检测 + 模糊处理 (后端 pipeline) |
| 安装路径有中文/特殊字符 | 程序崩溃 | Rust 用 `PathBuf`/`OsString` 处理路径 |
| 首次启动权限引导 | 用户被多个弹窗搞晕 | **一次性引导页**: 分步骤解释每个权限的作用 |
| 录制性能影响游戏帧率 | 用户卸载 | 硬件加速编码，目标 < 3% FPS 影响 |
| 上传占网速 | 游戏延迟 | **默认 50% 带宽限制**，游戏中暂停上传 |
| 用户想知道赚了多少钱 | 留存 | 系统托盘通知: "本次录制 45 分钟，预计收入 $X.XX" |

### 7.6 安全 & 合规

| Edge Case | 影响 | 解决方案 |
|-----------|------|---------|
| 录到敏感信息 (银行/密码) | 法律风险 | 后端 PII 扫描 + 面部/用户名模糊 |
| 录到第三方版权内容 (cutscene) | 版权风险 | 用户协议声明 + 后端内容过滤 |
| 录制未成年人内容 | COPPA | 注册时年龄验证，<13 岁禁止使用 |
| 数据传输安全 | 中间人攻击 | TLS 1.3 + 分块 SHA-256 校验 |
| 本地文件被篡改 | 数据质量 | 录制时生成文件哈希，上传时验证 |

---

## 八、推荐开发路线

```
Phase 1 (Month 1-2): Windows MVP
├── windows-capture (WGC) 录屏
├── Raw Input 键鼠捕获
├── ffmpeg-next H.265 编码
├── 本地保存: video.mp4 + input.json + meta.json
├── 自动游戏检测 (进程名匹配)
└── 系统托盘 App (最小 UI)

Phase 2 (Month 2-3): 上传 + 后端
├── S3 分块上传 + 断点续传
├── 后端 ingestion pipeline (Lambda + Step Functions)
├── 质量评分系统
└── 用户 Dashboard (录制历史 + 收入)

Phase 3 (Month 3-4): macOS 支持
├── ScreenCaptureKit + CGEventTap
├── VideoToolbox H.265
├── 跨平台 Rust core (uniffi)
└── Notarization + .dmg 分发

Phase 4 (Month 4-6): Android
├── MediaProjection + MediaCodec
├── Overlay 触控捕获
├── Kotlin UI + Rust core (cargo-ndk + uniffi)
└── Play Store 上架

Phase 5 (Month 6+): iOS + 引擎插件
├── ReplayKit Broadcast Extension
├── Godot/Unity 引擎插件 (Premium 数据)
└── 手柄输入完善
```

---

## 九、关键架构决策总结

| 决策 | 选择 | 理由 |
|------|------|------|
| 录屏 API | WGC 主 + DXGI DD 备 | 覆盖最广，零反作弊风险 |
| 放弃全屏独占 | 是 | 现代游戏默认 borderless，工程复杂度不值得 |
| 输入捕获 | Raw Input (非 hooks) | 反作弊安全，延迟更低 |
| 编码库 | ffmpeg-next | 多 GPU 厂商自动适配 |
| 跨平台策略 | Rust core + 平台 UI | uniffi 生成绑定，一次写核心逻辑 |
| iOS 触控 | Phase 1 不做 | 无系统级 API，ROI 太低 |
| 帧率 | 强制 CFR 30fps | 后处理一致性 > 原始帧率 |
| 分发 | 非 App Store (Mac) | CGEventTap 不兼容沙盒 |
| Android 先于 iOS | 是 | 触控捕获方案完整，且 Android 游戏用户基数更大 |

---

## 十、游戏自动检测 (零配置，用户无感)

### 10.1 多层信号架构

```
Layer 1: 进程监控 (sysinfo crate, 2秒轮询, 黑名单过滤)
    ↓
Layer 2: 启动器集成 (Steam/Epic/GOG 本地清单解析)
    ↓
Layer 3: 前台窗口 + 全屏检测 (GetForegroundWindow)
    ↓
Layer 4: 图形 API 检测 (d3d11.dll/vulkan-1.dll 加载检测)
    ↓
Layer 5: GPU 占用 (nvml-wrapper, >40% 持续 5s)
    ↓
  置信度评分 → 超过 0.6 → 自动开始录制
```

### 10.2 置信度评分

| 信号 | 分值 |
|------|------|
| Steam/Epic/GOG 清单匹配 | +0.45 |
| 本地游戏 DB 匹配 (exe 名) | +0.35 |
| 加载了 DX11/DX12 | +0.20 |
| 加载了 Vulkan | +0.25 |
| 全屏/无边框 | +0.15 |
| GPU 占用 >40% | +0.15 |
| 黑名单进程 (steam.exe 等) | =0.00 |

### 10.3 启动器清单解析

| 启动器 | 文件 | Rust Crate |
|--------|------|-----------|
| Steam | `appmanifest_*.acf` | `steamlocate` + `vdf-serde` |
| Epic | `Manifests/*.item` (JSON) | `serde_json` |
| GOG | `galaxy-2.0.db` (SQLite) | `rusqlite` |

### 10.4 游戏数据库

- **IGDB API**: 200K+ 游戏, Twitch OAuth2, 可按 Steam AppID 交叉查询
- **本地 SQLite**: 预装 exe→游戏映射, 随 App 更新推送

### 10.5 Edge Cases

| 场景 | 方案 |
|------|------|
| 非商店游戏 | 启发式: 全屏 + DX/Vulkan + GPU 高 |
| 模拟器 | 进程名硬匹配 + 窗口标题解析 ROM 名 |
| 启动器→游戏 | 父子进程追踪, 子进程全屏时开录 |
| Alt+Tab | 10 秒宽限, GPU 降零则暂停 |
| 多游戏同时开 | 录前台 + GPU 占用最高的 |

---

## 十一、后端 Pipeline

> 完整文档: `backend-architecture.md`

### 11.1 架构

```
客户端 → S3 Presigned URL 上传 → Lambda 校验 → SQS
  ↓
Step Functions 8 步:
  1. FFmpeg 转码 (Fargate Spot ARM)    ← 最贵 $36K/mo @Phase2
  2. 游戏检测 (MobileNetV2 CNN)
  3. 质量评分 (6维加权)
  4. 内容过滤 (裁剪菜单/死亡/Loading)
  5. PII 扫描 (人脸/用户名模糊)        ← 第二贵 $18K/mo
  6. 输入日志对齐
  7. 引擎元数据合并
  8. 写入数据目录
  ↓
存储: S3 收 → R2 发 (零出口费) → ClickHouse 分析 → PostgreSQL 元数据
  ↓
Buyer API: REST + Stripe 计量 + R2 Presigned URL 下载
```

### 11.2 成本

| 阶段 | 用户 | 月总成本 | 单位成本 |
|------|------|---------|---------|
| Phase 1 | 1K | $5.1K | $0.27/hr |
| Phase 2 | 10K | $81K | $0.27/hr |
| Phase 3 | 100K | $620K | $0.21/hr (规模效应) |

### 11.3 质量评分

| 维度 | 权重 |
|------|------|
| Action Density (输入频率+摄像机移动) | 30% |
| Content Uniqueness (帧间差异) | 20% |
| Session Length (5min 起, 30min 满分) | 15% |
| Input Richness (输入类型多样性) | 15% |
| FPS Stability (标准差 <2fps 满分) | 10% |
| Resolution (1080p 满分) | 10% |

---

## 十二、定价 & 单位经济

> **Note:** Unit economics documentation pending

### 12.1 卖给 AI 公司

| 层级 | 价格/小时 | 包含 |
|------|----------|------|
| Tier 1: 视频+输入 | $4-8 | MP4 + JSON 键鼠事件 |
| Tier 2: +引擎元数据 | $15-30 | + Camera pose, objects, physics |
| Custom Bounty | $12-50 | 买家指定游戏/场景 |
| **均价** | **$7.80** | |

### 12.2 付给玩家

| 层级 | $/可用小时 |
|------|-----------|
| Tier 1 (基础) | $0.50 |
| Tier 2 (引擎) | $1.00 |
| Bounty | $2-8 |

### 12.3 单位经济 (Phase 2)

```
卖出价:    $7.80/hr
- 玩家付费: $0.50
- 处理成本: $0.22
- 存储传输: $0.08
- 推荐佣金: $0.05
= 毛利:     $6.95/hr (72.7%)
```

### 12.4 收入预测

| 阶段 | 月收入 | 月利润 | ARR |
|------|--------|--------|-----|
| Phase 1 (1K) | $19.6K | -$35K | - |
| Phase 2 (10K) | $524K | $141K | $6.3M |
| Phase 3 (100K) | $5.1M | $3.3M | $61M |

**关键发现**: 目前不存在商业化游戏数据市场。NVIDIA Cosmos 用了 2000 万小时视频训练。

---

## 十三、引擎插件 (Premium 数据, 3-5x 溢价)

> 完整文档: `engine-plugin-research.md`

### 13.1 方案

| 引擎 | 工具 | 优先级 |
|------|------|--------|
| Unity (Mono) | BepInEx + HarmonyX | P0 (覆盖最多) |
| Unity (IL2CPP) | MelonLoader | P1 |
| Godot 4 | godot-rust `gdext` GDExtension | P2 |
| Unreal 5 | C++ UGameInstanceSubsystem | P3 (最后) |

### 13.2 每帧提取数据

Camera (pos/rot/FOV) + Player (pos/vel/anim) + Objects×N (type/pos/state) + Environment (light/weather) + Events (Door_Open, Weapon_Fire...)

- 每帧 ~8-12KB (50 个对象)
- 每小时 ~300-500MB (压缩后)
- 序列化: **FlatBuffers** (零拷贝, ML 友好)

### 13.3 反作弊限制

EAC/BattlEye/Vanguard 拦截所有 modloader → 引擎插件**仅适用于单机/离线**。在线游戏走屏幕录制。

---

## 十四、"简单"设计哲学

> Howard 核心要求: 用户端要简单 + 我们标注要简单

### 14.1 用户端简单 (玩家)

**原则: 用户做的事情 = 0 配置**

| 用户动作 | 系统自动处理 |
|---------|------------|
| 下载安装 | 一键安装, 无依赖 (静态链接 Rust binary) |
| 打开 App | 自动启动到系统托盘, 开机自启 |
| 玩游戏 | 自动检测游戏启动 → 自动录制 |
| 退出游戏 | 自动停止录制 → 自动裁剪垃圾帧 |
| 什么都不做 | 自动 WiFi 上传 → 自动质量审核 → 自动到账 |
| 查看收入 | 系统托盘气泡: "今日录制 2.3 小时, 预计 $1.15" |

**用户永远不需要**:
- 选择分辨率/帧率/编码器
- 手动开始/停止录制
- 选择录哪个游戏
- 管理本地文件
- 手动上传
- 理解任何技术概念

### 14.2 标注端简单 (数据处理)

**原则: 机器做 95%，人做 5% 例外审核**

| 传统标注 (Scale AI 模式) | GameData Labs 模式 |
|------------------------|-------------------|
| 人工看视频打标签 | 机器自动: 游戏检测 (CNN) + 质量评分 (算法) |
| 人工标记动作边界 | 自动: 输入日志天然就是动作标签 (按键=动作) |
| 人工检查 PII | 自动: 人脸/文字检测 + 模糊 |
| 人工分类场景 | 自动: 引擎元数据有场景分类 |
| 人工清洗垃圾数据 | 自动: 帧相似度 >95% 裁剪, idle >10% 标记 |
| **标注成本: $2-5/hr** | **标注成本: $0.05/hr (纯算力)** |

**自动标注的关键洞察**:
1. **输入日志 = 免费的动作标签** — 每个键盘/鼠标事件天然标记了"玩家在这一帧做了什么"。无需人工标注 action。
2. **引擎元数据 = 免费的场景标签** — Camera pose, object transforms 直接描述了世界状态。无需人工标注 scene。
3. **质量评分 = 自动过滤器** — 低分数据直接丢弃，无需人工审核。
4. **人工只处理边界 case** — CNN 置信度 <0.7 的游戏识别、质量评分在阈值附近的数据。

### 14.3 买家端简单 (AI 公司)

| 买家动作 | 系统处理 |
|---------|---------|
| 搜索数据 | Data Portal: 按游戏/类型/场景/引擎筛选 |
| 下载数据 | 选格式 (视频/帧序列/state-action pairs) → 一键下载 |
| 指定需求 | Bounty Board: 描述需要什么 → 系统自动匹配 + 定价 |
| 付费 | Stripe 按量计费, 月结 |

### 14.4 数据格式自动转换 (买家不需要自己处理)

```
原始数据 (video.mp4 + input.json + meta.json + engine.json)
    ↓ 按买家需求自动转换
├── Frame Sequences: PNG/JPG @配置 FPS + annotation JSON
├── State-Action Pairs: {state, action, outcome} JSON tuples
├── Video Clips: 按场景/动作类型分段
├── Bulk: tar.gz via R2 Transfer Acceleration
└── Streaming API: 实时数据流 (未来)
```

---

---

## 十五、买家 Spec 对照 & 新增需求

> 来源: 真实买家需求文档 (World Model 公司)

### 15.1 买家核心要求

**Phase I**: 单人游戏环境导航 (无复杂 UI 交互)
**Phase II**: 多人游戏 agent-to-agent 交互
**Engine Data**: Camera pose + object transforms + asset metadata
**关键词**: "clean" visual data — 干净比数量重要

### 15.2 必须实现的新需求

#### A. Motion Blur 自动关闭

**问题**: 买家要求 Motion Blur OFF，但这是每个游戏的独立设置

**方案**:
1. **配置文件注入** — 很多游戏的设置存在 `.ini`/`.cfg`/`registry` 中
   - Unreal Engine: `Engine.ini` → `r.MotionBlurQuality=0`
   - Unity: 通常在 `PlayerPrefs` (registry) 或 `quality_settings.json`
   - Source/Source 2: `autoexec.cfg` → `mat_motion_blur_enabled 0`
2. **首次启动引导** — 检测到游戏后，弹出一次性提示: "请在游戏设置中关闭动态模糊"
3. **帧间运动检测** — 后处理时检测 motion blur 特征 (光流场方差过大)，标记受影响帧

**推荐**: 先做 #2 (引导), 后做 #1 (自动注入常见游戏)

#### B. 死亡画面实时检测 + 自动停录

**问题**: 角色死亡/不可控时必须立刻停录

**方案**:
1. **输入中断检测** — 如果玩家输入突然停止 (无键鼠事件 >3 秒) 且游戏画面变化剧烈 (fade to black/red)，标记为可能死亡
2. **帧特征检测** — 训练 CNN 识别常见死亡画面特征:
   - "You Died" / "Game Over" 文字 (OCR)
   - 全屏变暗/变红
   - 固定视角 (相机停止移动)
3. **引擎事件** — 有引擎插件时，直接 hook `OnPlayerDeath` / `OnGameOver`
4. **处理策略**: 检测到死亡 → 暂停录制 → 等待玩家重生 → 恢复。后处理时裁剪死亡帧。

#### C. 实时 FPS 日志 (逐秒)

**问题**: 买家要求每秒的真实 FPS 数据

**方案**: 在录屏回调中计数每秒实际收到的帧数，写入 `fps_log.json`:
```json
[
  {"second": 0, "fps": 30, "frame_time_ms_avg": 33.3, "frame_time_ms_max": 35.1},
  {"second": 1, "fps": 30, "fps_drops": 0},
  ...
]
```
这个很简单，在 windows-capture 的 frame callback 中加计数器即可。

#### D. Mouse DPI 自动测试

**问题**: 买家要求 DPI 测试值，但 Windows 无 API 查硬件 DPI

**方案**:
1. **自动 DPI 测试流程**:
   - 首次安装时要求用户"在屏幕上匀速拖动鼠标从左到右"
   - 测量 Raw Input delta counts vs 屏幕像素距离
   - 计算 DPI = delta_counts / (pixel_distance / screen_dpi × 25.4)
2. **品牌软件 API 检测**:
   - Logitech G Hub: 读 `%APPDATA%\LGHUB\settings.db` (SQLite)
   - Razer Synapse: 读 `%APPDATA%\Razer\Synapse3\` 配置文件
3. **默认值提示**: "确保你的鼠标 DPI 为默认值 (通常 800 或 1600)"

#### E. Scene Classification (场景分类体系)

**问题**: 买家要基于 taxonomy 分类场景 (urban/nature/sci-fi/indoor/outdoor)

**方案**:
1. **一级分类** (环境):
   - Indoor: residential, commercial, industrial, underground, spaceship
   - Outdoor: urban, suburban, rural, wilderness, desert, ocean, space
2. **二级分类** (风格):
   - Realistic, Stylized, Sci-fi, Fantasy, Horror, Post-apocalyptic
3. **实现**: 每 10 秒采样一帧 → ResNet/EfficientNet 分类器 → 多标签输出
4. **训练数据**: 从 IGDB 游戏截图 + Steam 商店图片标注

#### F. HUD/UI 检测与标记

**问题**: 买家要求 HUD 最小化或关闭，但无法自动控制

**方案**:
1. **引导用户** — 首次录制时提示: "为获得更高收益，请在游戏中关闭 HUD (通常在设置 → 界面)"
2. **HUD 区域检测** — 训练模型识别常见 HUD 元素 (血条、小地图、十字准星)
3. **元数据标记** — `"hud_coverage": 0.12` (HUD 占画面比例)，买家自行决定是否使用
4. **定价差异** — HUD <5% 的录制付更高价，激励用户关闭 HUD

### 15.3 Interaction Category 映射

买家定义了 3 级交互层次，我们的 Input Log + Engine Event 天然覆盖:

| 买家类别 | 检测方式 | 数据来源 |
|---------|---------|---------|
| Tier 0: 纯导航 (无交互) | 只有移动输入 (WASD/鼠标) | Input Log |
| Tier 1: 环境交互 (物理/触发器) | 交互键触发 (E/F/Click) + 引擎事件 | Input Log + Engine Event |
| Tier 2: 特殊行为 (跳跃/360旋转等) | 特征输入模式 (Space/连续鼠标旋转) | Input Log + Camera pose delta |

### 15.4 Exclusion 自动过滤清单

| 排除条件 | 检测方法 |
|---------|---------|
| 角色静止不动 | Input idle >10% of session |
| 非实时游戏/文字冒险 | 游戏 DB genre 过滤 |
| 不可跳过的过场 | 帧相似度 + 输入中断检测 |
| 嵌套交互 >2层 | UI 状态检测 (通过引擎插件或帧分析) |
| 需要陀螺仪/语音的游戏 | 游戏 DB 过滤 |
| NPC 驱动动作 | 引擎事件 + 无玩家输入但画面变化 |
| 视觉瑕疵 (镜头抖动/过曝) | 帧质量检测 (亮度直方图 + 光流稳定性) |

### 15.5 交付格式 (完全符合买家 Spec)

```
session_{id}/
├── video.mp4           # MP4, H.265, 1080p, 30fps 锁定, Motion Blur OFF
├── input_log.json      # 帧对齐键鼠/手柄事件, ms 时间戳
├── meta.json           # 游戏标题, 类型, 分辨率, DPI, 宽高比, 系统配置
├── fps_log.json        # 逐秒 FPS + 帧时间统计
├── engine/             # (Premium, 仅引擎插件游戏)
│   ├── camera.fb       # FlatBuffers: 逐帧 Camera pose
│   ├── objects.fb      # FlatBuffers: 逐帧 Object transforms
│   ├── events.fb       # FlatBuffers: 离散事件 (Door_Open 等)
│   └── world_state.fb  # FlatBuffers: 资产状态变化
└── quality/
    ├── score.json      # 质量评分 (6 维度)
    ├── scene_tags.json # 场景分类标签
    └── hud_report.json # HUD 覆盖率分析
```

---

## 十六、完整文件索引

| 文件 | 内容 |
|------|------|
| `GameData_Labs_Tech_Research.md` | 本文: 技术选型总报告 |
| `backend-architecture.md` | 后端 Pipeline 详细架构 |
| *(pending)* | 定价 & 单位经济详细模型 |
| `engine-plugin-research.md` | 引擎插件详细研究 |
