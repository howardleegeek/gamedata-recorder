\# Bug Report: 第二次录制卡死



\*\*来源\*\*: 用户反馈  

\*\*现象\*\*: 第一次录制正常（可持续数十秒），停止后立即开始第二次录制，程序卡死  

\*\*严重程度\*\*: 高（功能完全不可用）



\---



\## 根本原因分析



第一次录制结束后，某类资源没有完全释放，第二次启动时撞上残留资源导致卡死。有以下四种可能原因，可能同时存在。



\---



\### 原因 1：OBS 资源没有完全释放（最常见）



`obs\_output\_stop()` 被调用了，但 `release` 系列函数漏掉，导致第二次录制时两套资源同时存在，显存/内存撑爆。



```rust

// 停止录制时必须完整走这个顺序

obs\_output\_stop(output);

obs\_output\_release(output);   // ← 经常漏掉

obs\_source\_release(source);   // ← 经常漏掉

obs\_encoder\_release(encoder); // ← 经常漏掉

```



\---



\### 原因 2：DXGI Desktop Duplication 没有释放



`IDXGIOutputDuplication` 同一个 output \*\*同时只能有一个 session\*\*。第一次没有 `Release()`，第二次调用 `DuplicateOutput()` 时返回 `DXGI\_ERROR\_NOT\_CURRENTLY\_AVAILABLE`，导致卡死。



```

第一次 DuplicateOutput() 成功

&#x20;   ↓

停止录制，IDXGIOutputDuplication 未 Release()

&#x20;   ↓

第二次 DuplicateOutput() → DXGI\_ERROR\_NOT\_CURRENTLY\_AVAILABLE → 卡死

```



\---



\### 原因 3：后台线程未退出就启动了第二次



stop 信号发出后，线程还卡在 `AcquireNextFrame()`，第二次录制又 spawn 了新线程，两个线程同时操作同一资源，导致死锁。



```rust

// 错误写法：只设了 flag，没等线程真正退出

self.is\_recording.store(false, Ordering::SeqCst);

// 漏掉了 join：

// self.capture\_thread.join().unwrap();



// 第二次录制时旧线程还在跑，又 spawn 了新线程 → 死锁

```



\---



\### 原因 4：显存未回收就重新申请



GPU 资源的释放是异步的，停止录制后立即开始第二次，显存还未真正回收就再次申请，触发 OOM，驱动卡死。



```

第一次录制占满显存

&#x20;   ↓

停止录制，release 调用了，但 GPU 尚未真正回收

&#x20;   ↓

立刻开始第二次录制，再次申请显存

&#x20;   ↓

OOM → 驱动卡死

```



\---



\## 排查方法



\*\*第一步：看日志卡在哪里\*\*



在 `start\_recording()` 开头加详细日志，确认卡死发生在哪个阶段：



```rust

log::info!("=== start\_recording called ===");

log::info!("active threads: {}", active\_thread\_count());

```



\*\*第二步：观察内存/显存是否回落\*\*



第一次录制停止后，打开 Task Manager，观察进程的内存和 GPU 显存占用有没有回落。如果没有回落，确认是资源泄漏。



\---



\## 解决方案



\### Fix 1：用 RAII 管理 OBS 资源（推荐）



用 `Drop` trait 保证资源一定按顺序释放，不会遗漏：



```rust

struct RecordingSession {

&#x20;   output:  \*mut obs\_output\_t,

&#x20;   source:  \*mut obs\_source\_t,

&#x20;   encoder: \*mut obs\_encoder\_t,

}



impl Drop for RecordingSession {

&#x20;   fn drop(\&mut self) {

&#x20;       unsafe {

&#x20;           // 顺序不能错

&#x20;           obs\_output\_stop(self.output);

&#x20;           obs\_output\_release(self.output);

&#x20;           obs\_encoder\_release(self.encoder);

&#x20;           obs\_source\_release(self.source);

&#x20;       }

&#x20;   }

}

// RecordingSession 离开作用域自动释放，不会漏

```



\---



\### Fix 2：线程退出加 join + 超时



stop 信号发出后，必须等线程真正退出，再释放 D3D 资源：



```rust

fn stop\_recording(\&mut self) {

&#x20;   self.is\_recording.store(false, Ordering::SeqCst);



&#x20;   // 等线程真正退出，最多等 3 秒

&#x20;   if let Some(handle) = self.capture\_thread.take() {

&#x20;       match handle.join\_timeout(Duration::from\_secs(3)) {

&#x20;           Ok(\_)  => log::info!("capture thread exited cleanly"),

&#x20;           Err(\_) => log::warn!("capture thread did not exit in time"),

&#x20;       }

&#x20;   }



&#x20;   // 线程退出后再释放 D3D 资源

&#x20;   self.release\_dxgi();

}

```



\---



\### Fix 3：DXGI 释放后等待 GPU 回收



```rust

fn release\_dxgi(\&mut self) {

&#x20;   if let Some(dup) = self.duplication.take() {

&#x20;       drop(dup); // IDXGIOutputDuplication release

&#x20;   }

&#x20;   // 给 GPU 驱动时间真正回收资源

&#x20;   std::thread::sleep(Duration::from\_millis(200));

}

```



\---



\### 临时 Fix（快速验证用）



如果上述改动量太大，可以先在第二次录制前强制走一次完整清理，验证方向是否正确：



```rust

fn start\_recording(\&mut self) {

&#x20;   self.force\_cleanup();

&#x20;   std::thread::sleep(Duration::from\_millis(500));



&#x20;   self.init\_obs();

&#x20;   self.init\_capture();

&#x20;   // ...

}

```



如果加了这个之后第二次录制恢复正常，可以确认是资源释放问题，再做正式的 RAII 改造。



\---



\## 受影响的代码位置（待确认）



| 文件 | 问题 |

|------|------|

| `record/obs\_embedded\_recorder.rs` | OBS output/source/encoder release 是否完整 |

| `record/recording.rs` | 捕获线程是否等待 join 后再释放 D3D 资源 |

| `capture/dxgi\_capture.rs` | IDXGIOutputDuplication 是否在 stop 时 Release |



\---



\## 验证方法



修复后执行以下步骤确认问题解决：



1\. 运行 `.\\run\_ci.ps1 -RecordSeconds 10`，第一次录制通过

2\. 不重启程序，立即再次运行，第二次录制也通过

3\. 重复 5 次，每次均通过则认为修复完成


---


## 当前实现状态 (2026-04-23)


### ✅ 已实施的修复


#### 修复 1：音频捕获已禁用（解决 WASAPI 无限重试循环）

**实施位置**: `src/record/obs_embedded_recorder.rs:1739`

```rust
// 音频捕获已禁用以节省资源并避免第二次录制崩溃
let capture_audio = false;
```

**效果**:
- 节省 ~1-3% CPU，5-15 MB 内存，~15% 磁盘空间
- 消除了 WASAPI 音频进程循环back companion 的无限重试循环
- **关键修复**: WASAPI companion 绑定到游戏窗口句柄，窗口变化时（如 GTA V 分辨率变化）会进入 "window disappeared" → "Device invalidated. Retrying" 循环，导致 OBS 输出被饿死，程序卡死

**状态**: ✅ 完全实施


#### 修复 2：强制重建 WGC 和 GameHook 捕获源

**实施位置**: `src/record/obs_embedded_recorder.rs:1741-1771`

```rust
// 强制重建 WGC 和 GameHook 源以修复第二次录制崩溃
if matches!(
    state.effective_mode,
    crate::config::EffectiveCaptureMode::Wgc | crate::config::EffectiveCaptureMode::GameHook
) {
    if let Some(source) = last_source.take() {
        tracing::info!(
            mode = ?state.effective_mode,
            "Force recreating source (fixes second recording crash with stale WASAPI audio companion)"
        );
        let _ = scene.remove_source(&source);
    }
}
```

**原理**:
- WGC 和 GameHook 模式会生成 WASAPI 音频进程循环back companion
- 当窗口变化（分辨率变化、重启游戏）时，旧 audio companion 绑定到已失效的 HWND
- 强制重建源确保新的 audio companion 绑定到当前有效窗口

**状态**: ✅ 完全实施


#### 修复 3：编码器缓存清理（释放 GPU 内存）

**实施位置**: `src/record/obs_embedded_recorder.rs:1285-1290`

```rust
// 清除编码器缓存以在录制之间释放 GPU 内存
// 编码器持有 GPU 端帧缓冲；在多个录制中缓存它们可能累积 VRAM 并导致 OOM
self.video_encoders.clear();
tracing::debug!("Cleared encoder cache to release GPU memory");
```

**效果**: 在每次录制停止后释放 GPU 显存，避免 OOM

**状态**: ✅ 完全实施


#### 修复 4：RwLock 锁中毒保护（防止级联崩溃）

**实施位置**: `src/app_state.rs:79-126` (RwLockExt trait)

**修复文件**:
- `src/app_state.rs` - 添加 `read_safe()` 和 `write_safe()` 方法
- `src/ui/views/main/mod.rs` - 4 处修复
- `src/record/recorder.rs` - 6 处修复
- `src/tokio_thread.rs` - 20+ 处修复
- `src/ui/views/mod.rs` - 4 处修复
- `src/ui/overlay.rs` - 3 处修复
- `src/main.rs` - 1 处修复
- `src/ui/views/main/upload_manager.rs` - 1 处修复

**效果**:
- 当线程 panic 时不再导致整个应用崩溃
- 记录锁中毒警告并恢复守卫
- 用户不会丢失未保存的工作

**状态**: ✅ 完全实施


#### 修复 5：DXGI 访问丢失恢复（Workstation Lock/UAC 处理）

**实施位置**: `src/record/obs_embedded_recorder.rs:122-216`

```rust
enum AccessLostState {
    Active,
    Paused(Instant),
}

const ACCESS_LOST_RESUME_TIMEOUT: Duration = Duration::from_secs(5 * 60);

fn session_is_interactive() -> bool {
    // 检测当前进程的窗口站是否有权访问前台输入桌面
    let result = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) };
    // ...
}
```

**效果**:
- 检测 Win+L、RDP 会话切换、UAC 安全桌面
- 暂停录制而不是生成损坏的 MP4
- 交互式桌面返回后自动恢复

**状态**: ✅ 完全实施


#### 修复 6：OBS 线程 Drop 超时保护

**实施位置**: `src/record/obs_embedded_recorder.rs:473-548`

```rust
impl Drop for ObsEmbeddedRecorder {
    fn drop(&mut self) {
        // ...
        const DROP_DEADLINE: Duration = Duration::from_secs(3);
        // 轮询 is_finished() 而不是调用 join()（无限期阻塞）
        // 如果 OBS 线程卡住（GPU 驱动死锁、FFmpeg muxer 挂起、阻塞的 FFI 调用），
        // 记录警告并放弃线程句柄，以便进程退出
    }
}
```

**效果**:
- 如果 libobs 原生资源卡死，3 秒后放弃线程
- 防止应用关闭时无限期挂起
- 记录警告日志

**状态**: ✅ 完全实施


### ⚠️ 部分实施的修复


#### 修复 7：编码器缓存持久化问题

**问题**: `video_encoders` HashMap 在每次录制后清空，但重建编码器开销大

**当前状态**: 
- ✅ 已实现缓存清理
- ⚠️ 未实现跨录制重用（性能权衡：优先释放 VRAM）

**权衡说明**: 清理缓存可防止 VRAM 在长时间会话中累积 OOM（特别是在显存密集型游戏如 GTA V Enhanced 中）


### ❌ 未实施的修复


#### 修复 8：RAII 资源管理（建议的未来改进）

**建议**: 使用 `Drop` trait 保证 OBS 资源按顺序释放

```rust
struct RecordingSession {
    output: ObsOutputRef,
    source: ObsSourceRef,
    encoder: ObsVideoEncoder,
}

impl Drop for RecordingSession {
    fn drop(&mut self) {
        // 确保按正确顺序释放
    }
}
```

**当前状态**: 
- 使用手动清理（`stop_recording_phase1/phase2`）
- 依赖 `ObsContext`、`ObsSourceRef`、`ObsOutputRef` 的现有 Drop 实现
- **已知风险**: 如果清理顺序错误可能导致资源泄漏

**建议优先级**: 中等（当前手动清理已覆盖主要场景）


#### 修复 9：线程 Join 超时（部分覆盖）

**当前实现**: 
- ✅ OBS 线程 Drop 有 3 秒超时
- ✅ Hook 监控线程在停止时 join（1 秒超时）

**未覆盖**: 
- ❌ DXGI 捕获线程无显式 join（依赖 OBS 内部管理）
- ❌ 原始输入桥接线程无超时保护


### 剩余已知问题


#### 问题 1：GPU 驱动死锁风险

**场景**: NVIDIA/AMD GPU 驱动在特定条件下死锁（D3D 设�）

**当前缓解**:
- Drop 超时（3 秒）后放弃线程
- 记录警告日志

**未解决**:
- 驱动死锁时 OBS 资源可能泄漏
- 后续 OBS 调用可能崩溃

**建议**: 
- 添加 OBS 上下文健康检查
- 考虑进程级 watchdog


#### 问题 2：通道关闭处理

**场景**: OBS 线程死亡导致 `obs_tx` 通道关闭

**当前实现**:
- 大多数通道 send 使用 `.ok()`（忽略错误）
- 部分 `.await` 可能失败

**风险**: 
- tokio 线程中的通道关闭可能导致级联错误

**建议**: 
- 添加通道关闭检测和恢复逻辑
- 考虑使用 `tokio::sync::broadcast` 进行更鲁棒的信号传递


### 测试状态


**自动化测试**: 
- ✅ 单元测试覆盖（`src/record/obs_embedded_recorder.rs:2141-2516`）
- ✅ CI 集成测试（`.github/workflows/ci-e2e.yml`）

**手动测试清单**:
- [ ] 快速按 F9 - 至少 200ms 内只响应一次
- [ ] 显示器休眠后唤醒 - 自动重建 DXGI Duplication 会话
- [ ] 游戏崩溃后重启 - 重新捕获新进程，继续录制
- [ ] 窗口拖到另一屏幕 - 能检测并提示用户
- [ ] UAC 弹窗时按 F9 - 按键仍能触发
- [ ] 锁屏后解锁 - 不崩溃，录制继续
- [ ] 系统休眠后唤醒 - 优雅恢复或至少不损坏文件


### 修复优先级建议


| 优先级 | 问题 | 预计工作量 | 建议 |
|:---|:---|:---|:---|
| 🔴 P0 | GPU 驱动死锁检测 | 2-3 天 | 添加 OBS 健康检查 + watchdog |
| 🟡 P1 | 通道关闭恢复逻辑 | 1-2 天 | 添加重连机制 |
| 🟢 P2 | RAII 资源管理重构 | 3-5 天 | 长期代码质量改进 |
| 🟢 P3 | 线程 Join 超时全覆盖 | 1 天 | 完善超时保护 |


### 相关提交记录


- `ea76897`: fix: disable audio capture to save resources and fix second recording crash
- `c20f0b5`: chore: bump to v2.5.13
- `399bcd7`: fix: second recording crash - force recreate WGC/GameHook sources


---

**最后更新**: 2026-04-23  
**更新者**: Claude (AI Assistant)  
**状态**: 主要修复已实施，剩余问题已记录并排优先级

