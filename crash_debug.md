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

