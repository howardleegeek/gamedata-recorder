\# Bug Report: 录制黑屏问题



\*\*发现日期\*\*: 2026-04-19  

\*\*影响版本\*\*: v2.0.7, v2.1.1  

\*\*复现游戏\*\*: PlayGTAV.exe  

\*\*严重程度\*\*: 高（录制完全不可用）



\---



\## 现象



用户录制 GTA V 时，产出视频为纯黑屏。录制会在数秒内因 FPS 过低（15fps，低于最低要求 27fps）被自动标记为无效并删除。



\---



\## 根本原因



\### Bug 1：Window Capture 捕获了错误的窗口



Window Capture 的目标 executable 被设置成了录屏软件自身，而不是游戏进程。



\*\*日志证据（03:13:21）\*\*：

```

\[window-capture: 'owl\_window\_capture'] update settings:

&#x20;   executable: gamedata-recorder.exe   ← 应为 PlayGTAV.exe

&#x20;   title: GameData Recorder v2.1.1     ← 应为游戏窗口标题

&#x20;   method chosen: BitBlt

```



\*\*推测原因\*\*：在初始化 Window Capture source 时，传入的 HWND 或 executable 来自录屏软件自己的窗口，而不是从游戏进程扫描结果中取得。



\---



\### Bug 2：游戏分辨率读取错误



游戏窗口分辨率被错误读成 `(600, 840)`，导致 OBS base resolution 设置错误，输出画面变形。



\*\*日志证据（03:13:21）\*\*：

```

Game resolution: (600, 840)     ← GTA V 不可能是这个分辨率

base resolution: 600x840        ← OBS 使用了错误的分辨率

output resolution: 1920x1080    ← 强行拉伸到 1080P

```



`600x840` 是录屏软件自身 UI 窗口的尺寸，说明 `GetWindowRect` 也用了错误的 HWND。



\*\*两个 Bug 同源\*\*：Window Capture 目标错误和分辨率读取错误，都是因为在某处用了录屏软件自身的 HWND，而不是游戏的 HWND。



\---



\### Bug 3：黑屏而非软件自身画面



即便 Window Capture 目标是录屏软件自己，录出来也不是软件界面，而是纯黑。



\*\*原因\*\*：OBS 自动选择了 `BitBlt` 方法（因为目标不是游戏进程，不走 hook）。但录屏软件的 UI 使用 `egui + wgpu` 渲染，内容在 GPU 显存中，BitBlt 是 GDI 层操作，读不到 GPU 渲染内容，因此返回纯黑。



\*\*日志证据\*\*：

```

WARN egui\_wgpu::renderer: Detected a linear (sRGBA aware) framebuffer

```



\---



\## 完整黑屏链路



```

Game Capture hook 注入 GTA 失败（Rockstar 反作弊拦截）

&#x20;       ↓

Fallback 到 Window Capture

&#x20;       ↓

Window Capture 初始化时使用了错误的 HWND（指向录屏软件自身）

&#x20;       ↓

executable 设为 gamedata-recorder.exe，分辨率读成 (600, 840)

&#x20;       ↓

OBS 对录屏软件窗口使用 BitBlt 方法

&#x20;       ↓

录屏软件使用 egui/wgpu GPU 渲染，BitBlt 读不到内容

&#x20;       ↓

纯黑屏，FPS 约 15，低于 27fps 阈值，录制被自动删除

```



\---



\## 复现步骤



1\. 启动 GameData Recorder

2\. 启动 GTA V（PlayGTAV.exe）

3\. 将 GTA V 窗口移到非 foreground 屏幕（双显示器场景）

4\. 程序检测到游戏并自动开始录制

5\. 录制产出视频为纯黑屏，数秒后被自动删除



\---



\## Fix



\### Fix 1：Window Capture 使用游戏 HWND



在 `obs\_embedded\_recorder` 初始化 Window Capture source 时，确保传入的是游戏进程的 HWND 和 executable，而不是录屏软件自身。



```rust

// 错误：使用了自身窗口信息

let settings = WindowCaptureSettings {

&#x20;   executable: "gamedata-recorder.exe",  // ← 错误

&#x20;   ...

};



// 正确：使用从进程扫描获得的游戏信息

let settings = WindowCaptureSettings {

&#x20;   executable: \&game\_exe,   // 如 "PlayGTAV.exe"

&#x20;   hwnd: game\_hwnd,         // 从 process scan 获得的 HWND

&#x20;   ...

};

```



\### Fix 2：分辨率检测使用游戏 HWND



`recording.rs` 中读取 Game resolution 时，确保 `GetWindowRect` 传入的是游戏窗口句柄。



```rust

// 错误

let rect = GetWindowRect(GetForegroundWindow());  // 可能返回录屏软件自己



// 正确

let rect = GetWindowRect(game\_hwnd);  // 明确使用游戏 HWND

```



\### Fix 3（可选）：Window Capture 方法强制使用 WGC



对于现代游戏和现代 UI 框架，BitBlt 普遍无效。可以在 Window Capture source 设置中强制指定 Windows Graphics Capture (WGC) 方法：



```rust

obs\_data\_set\_int(settings, "method", 2);  // 2 = WGC，比 BitBlt 兼容性好得多

```



\---



\## 受影响的代码位置（待确认）



| 文件 | 问题 |

|------|------|

| `record/obs\_embedded\_recorder.rs` | Window Capture source 初始化时 executable/hwnd 来源错误 |

| `record/recording.rs` | `Game resolution` 读取时 HWND 来源错误 |



\---



\## 验证方法



修复后，日志中应出现：

```

\[window-capture: 'owl\_window\_capture'] update settings:

&#x20;   executable: PlayGTAV.exe     ← 游戏进程

&#x20;   method chosen: WGC           ← 或至少不是 BitBlt

```

以及：

```

Game resolution: (1920, 1080)    ← 正确的游戏分辨率

base resolution: 1920x1080

```

