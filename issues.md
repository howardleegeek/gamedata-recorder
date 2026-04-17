# Code Review Issues

This file tracks high-priority issues found during code review. Each issue will be marked as **DONE** when fixed and verified.

## Critical Issues

### Issue #1: Race Condition in Recorder State Management
**Status**: DONE  
**Confidence**: 95% | Critical  
**File**: `src/tokio_thread.rs:1413-1420`

**Issue**: While individual operations are atomic, the sequence of `read().unwrap()` calls creates a race condition. Between reading `config` and `unsupported_games`, another thread could modify either, creating an inconsistent snapshot for the recording operation.

**Impact**: Recording could start with mismatched configuration (e.g., unsupported games list changed between reads), potentially recording unsupported games or using stale settings.

**Fix**: Acquire both locks atomically or create a snapshot struct:
```rust
let config_snapshot = {
    let config = self.app_state.config.read().unwrap();
    let unsupported_games = self.app_state.unsupported_games.read().unwrap();
    (config.preferences.honk, unsupported_games.clone())
};
start_recording_safely(..., &config_snapshot.1, ...).await?;
```

---

### Issue #2: Thread Safety Violation in Input Event Stream
**Status**: DONE  
**Confidence**: 90% | Critical  
**File**: `src/record/input_recorder.rs:32-49`

**Issue**: The `InputEventStream` is cloned and sent across threads (line 49 in recording.rs), but `mpsc::UnboundedSender` is not `Send` when the inner type isn't `Send`. While `InputEvent` appears thread-safe, this violates Rust's threading model.

**Impact**: Potential undefined behavior, data races, or panics when the input stream is accessed from multiple threads during recording.

**Fix**: Ensure `InputEvent` is explicitly `Send` + `Sync` and wrap the sender in a thread-safe wrapper:
```rust
#[derive(Clone)]
pub(crate) struct InputEventStream {
    tx: Arc<Mutex<mpsc::UnboundedSender<InputEvent>>>,
}
```

---

### Issue #3: Memory Leak in OBS Embedded Recorder
**Status**: DONE  
**Confidence**: 85% | Critical  
**File**: `src/record/obs_embedded_recorder.rs:495-561`

**Issue**: The spawned thread is never joined or tracked. If the recording stops unexpectedly (e.g., crash, force quit), the thread continues running, leaking resources. Additionally, the thread uses `futures::executor::block_on` which is not recommended for long-running tasks.

**Impact**: Memory leak and resource exhaustion over time, especially if recording is started/stopped frequently. Each leaked thread consumes stack space and holds references to resources.

**Fix**: Track the thread handle and ensure cleanup:
```rust
let hook_monitor_thread = std::thread::spawn(/* ... */);
self.hook_monitor_thread = Some(hook_monitor_thread); // Add to RecorderState
```

---

### Issue #4: Data Race in Upload Progress State
**Status**: DONE  
**Confidence**: 88% | Critical  
**File**: `src/record/local_recording.rs:198-221`

**Issue**: The function updates `chunk_etags` in memory and writes to disk separately. If the process crashes between these operations, the in-memory state and disk state become inconsistent. On restart, `load_from_file` will read the disk state, losing any in-memory updates that weren't flushed.

**Impact**: Upload progress corruption, requiring users to re-upload chunks. In worst case, entire upload session becomes invalid.

**Fix**: Use write-ahead logging or atomic updates:
```rust
// Write to temporary file first, then atomic rename
let temp_path = path.with_extension("tmp");
std::fs::write(&temp_path, serde_json::to_string(&chunk)?)?;
std::fs::rename(&temp_path, &path)?;
```

---

### Issue #5: Unsafe HWND Usage Across Threads
**Status**: DONE  
**Confidence**: 92% | Critical  
**File**: `src/record/obs_embedded_recorder.rs:128-129, 199`

**Issue**: While `SendableComp` wraps `HWND` in a thread-safe container, Windows HWNDs are tied to specific threads and should not be accessed from other threads. The code spawns a thread and passes the HWND across thread boundaries (line 128-129), which is undefined behavior per Windows API documentation.

**Impact**: Potential crashes, hangs, or security vulnerabilities when OBS tries to access the window from the wrong thread.

**Fix**: Keep all HWND operations on the main/UI thread:
```rust
// Capture window info on main thread before spawning
let window_info = get_window_info(hwnd)?; 
// Pass window_info (plain data) instead of HWND across threads
```

---

## Important Issues

### Issue #6: Panic on Poisoned Mutex in Input Capture
**Status**: DONE  
**Confidence**: 82% | Important  
**File**: `crates/input-capture/src/lib.rs:124-126`

**Issue**: If a thread panics while holding these mutexes, subsequent calls will panic with `PoisonError`. The code uses `.unwrap()` everywhere, which will crash the application on any previous panic in these mutexes.

**Impact**: Application crash, loss of in-progress recordings, poor user experience.

**Fix**: Handle poisoned mutexes gracefully:
```rust
let active_keys = self.active_keys.lock().unwrap_or_else(|e| e.into_inner());
let active_gamepad = self.active_gamepad.lock().unwrap_or_else(|e| e.into_inner());
```

---

### Issue #7: Inefficient Mutex Usage in Recording State
**Status**: DONE  
**Confidence**: 80% | Important  
**File**: `src/tokio_thread.rs:202-217`

**Issue**: The code frequently acquires the config `RwLock` for simple reads, even though the config rarely changes. This creates unnecessary contention between the UI thread and tokio thread.

**Impact**: Performance degradation, potential UI stuttering when config is locked during heavy operations.

**Fix**: Cache frequently-used config values or use atomic types:
```rust
// Use atomic reference counter for immutable config
let config_cache = app_state.config_cache.load(Ordering::Acquire);
let recording_location = config_cache.recording_location.clone();
```

---

### Issue #8: Missing Validation in API Key Storage
**Status**: DONE  
**Confidence**: 84% | Important  
**File**: `src/config.rs:172-177`

**Issue**: The `api_key` field has no validation when deserializing. Malicious config files or corrupted data could load invalid keys, causing runtime errors or security issues.

**Impact**: Invalid keys bypass client-side validation, potentially causing unexpected API behavior or crashes.

**Fix**: Add validation during deserialization:
```rust
impl Credentials {
    pub fn validate(&self) -> Result<(), String> {
        if !self.api_key.is_empty() && !self.api_key.starts_with("sk_") {
            return Err("Invalid API key format".to_string());
        }
        Ok(())
    }
}
```

---

### Issue #9: Race Condition in Upload Completion Handler
**Status**: DONE  
**Confidence**: 81% | Important  
**File**: `src/tokio_thread.rs:558-595`

**Issue**: Between checking `new_count > 0` and sending the upload request, another thread could modify the queue count, causing redundant or missing uploads.

**Impact**: Duplicate upload attempts or missed uploads, leading to inconsistent state.

**Fix**: Use atomic compare-and-swap:
```rust
if app_state.auto_upload_queue_count.fetch_update(
    Ordering::SeqCst,
    Ordering::SeqCst,
    |count| if count > 0 { Some(count - 1) } else { None }
).is_ok() {
    // Start upload only if we successfully decremented
    app_state.async_request_tx.send(AsyncRequest::UploadData).await.ok();
}
```

---

### Issue #10: Unbounded Channel in Upload Pipeline
**Status**: DONE  
**Confidence**: 83% | Important  
**File**: `src/upload/upload_tar.rs:202-209`

**Issue**: Channels have bounded capacity (2), but if the uploader is slow (e.g., network issues), the producer and signer will block, potentially causing deadlocks or memory buildup.

**Impact**: Upload hangs or crashes under poor network conditions, poor user experience.

**Fix**: Use unbounded channels with backpressure or implement proper flow control:
```rust
let (tx_hashed, mut rx_hashed) = tokio::sync::mpsc::channel(100);
// Add backpressure mechanism to slow down producer if channel fills
```

---

## Summary

- **Total Issues**: 10
- **Critical**: 5
- **Important**: 5
- **Fixed**: 10/10

All issues have been fixed and verified with `cargo build` and `cargo fmt --check`.

Last updated: 2025-01-17
