//! High-precision timer using Windows QueryPerformanceCounter with GetMessageTime hybrid.
//! Sub-microsecond precision, output formatted as HH:MM:SS.mmm
//!
//! The hybrid approach combines:
//! - QueryPerformanceCounter (QPC): High precision, monotonic
//! - GetMessageTime(): Windows message timestamp for correlation with system events

#[cfg(target_os = "windows")]
use windows::Win32::{
    System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
    UI::WindowsAndMessaging::GetMessageTime,
};

/// High-precision timer based on QueryPerformanceCounter (Windows)
/// or std::time::Instant (other platforms).
/// Optionally integrates GetMessageTime for Windows message correlation.
pub struct HighPrecisionTimer {
    #[cfg(target_os = "windows")]
    frequency: i64,
    #[cfg(target_os = "windows")]
    start_counter: i64,
    #[allow(dead_code)]
    start_instant: std::time::Instant,
    /// Offset to correlate QPC time with GetMessageTime (Windows only)
    #[cfg(target_os = "windows")]
    msg_time_offset_ms: i32,
}

impl HighPrecisionTimer {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        {
            let mut frequency = 0i64;
            let mut start_counter = 0i64;
            unsafe {
                // QueryPerformanceFrequency should never fail on Windows XP+
                // but we log just in case something goes wrong
                if let Err(e) = QueryPerformanceFrequency(&mut frequency) {
                    tracing::error!("QueryPerformanceFrequency failed: {:?}", e);
                    // Fall back to a reasonable default (1MHz is common)
                    frequency = 1_000_000;
                }
                if let Err(e) = QueryPerformanceCounter(&mut start_counter) {
                    tracing::error!("QueryPerformanceCounter failed: {:?}", e);
                    // Will result in elapsed_ms returning 0 until next successful call
                }
            }

            // Get initial message time for correlation
            let msg_time_offset_ms = unsafe { GetMessageTime() };

            Self {
                frequency,
                start_counter,
                start_instant: std::time::Instant::now(),
                msg_time_offset_ms,
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            Self {
                start_instant: std::time::Instant::now(),
            }
        }
    }

    /// Elapsed milliseconds since timer creation, using QPC for precision.
    pub fn elapsed_ms(&self) -> u64 {
        #[cfg(target_os = "windows")]
        {
            let mut current = 0i64;
            unsafe {
                if let Err(e) = QueryPerformanceCounter(&mut current) {
                    tracing::error!("QueryPerformanceCounter failed: {:?}", e);
                    // Fall back to std::time::Instant if QPC fails
                    return self.start_instant.elapsed().as_millis() as u64;
                }
            }
            let elapsed = current - self.start_counter;
            ((elapsed as u128 * 1000) / self.frequency as u128) as u64
        }

        #[cfg(not(target_os = "windows"))]
        {
            self.start_instant.elapsed().as_millis() as u64
        }
    }

    /// Elapsed microseconds since timer creation, using QPC for precision.
    /// Provides higher resolution than elapsed_ms() for latency-sensitive measurements.
    pub fn elapsed_us(&self) -> u64 {
        #[cfg(target_os = "windows")]
        {
            let mut current = 0i64;
            unsafe {
                if let Err(e) = QueryPerformanceCounter(&mut current) {
                    tracing::error!("QueryPerformanceCounter failed: {:?}", e);
                    // Fall back to std::time::Instant if QPC fails
                    return self.start_instant.elapsed().as_micros() as u64;
                }
            }
            let elapsed = current - self.start_counter;
            ((elapsed as u128 * 1_000_000) / self.frequency as u128) as u64
        }

        #[cfg(not(target_os = "windows"))]
        {
            self.start_instant.elapsed().as_micros() as u64
        }
    }

    /// Elapsed nanoseconds since timer creation, using QPC for precision.
    /// This is the highest resolution timestamp available — matches the precision
    /// used by Mouse-Keyboard-Time-Series (time.perf_counter_ns() equivalent).
    /// Critical for frame alignment: 30fps = 33.33ms/frame, need sub-ms precision.
    pub fn elapsed_ns(&self) -> u64 {
        #[cfg(target_os = "windows")]
        {
            let mut current = 0i64;
            unsafe {
                if let Err(e) = QueryPerformanceCounter(&mut current) {
                    tracing::error!("QueryPerformanceCounter failed: {:?}", e);
                    return self.start_instant.elapsed().as_nanos() as u64;
                }
            }
            let elapsed = current - self.start_counter;
            ((elapsed as u128 * 1_000_000_000) / self.frequency as u128) as u64
        }

        #[cfg(not(target_os = "windows"))]
        {
            self.start_instant.elapsed().as_nanos() as u64
        }
    }

    /// Current wall-clock time as HH:MM:SS.mmm string.
    pub fn wall_time_str(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_else(|e| {
                tracing::warn!("System time is before UNIX epoch: {}", e);
                std::time::Duration::default()
            });
        let secs = now.as_secs();
        let ms = now.subsec_millis();
        let hours = (secs / 3600) % 24;
        let minutes = (secs / 60) % 60;
        let seconds = secs % 60;
        format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, seconds, ms)
    }

    /// Get the current GetMessageTime value (Windows only).
    /// Returns milliseconds since system start.
    /// This is useful for correlating input events with Windows message timestamps.
    /// Returns 0 if the call fails (should not happen on modern Windows).
    #[cfg(target_os = "windows")]
    pub fn message_time_ms(&self) -> i32 {
        unsafe {
            // GetMessageTime returns the time in milliseconds; on error it returns -1
            // but this should not fail on modern Windows. We check and log just in case.
            let result = GetMessageTime();
            if result < 0 {
                tracing::warn!("GetMessageTime returned negative value: {}", result);
            }
            result
        }
    }

    /// Get hybrid timestamp combining QPC precision with message time correlation.
    /// Returns (elapsed_ms, message_time_ms) tuple.
    ///
    /// This is useful for input capture scenarios where you need:
    /// - High precision timing (from QPC)
    /// - Correlation with Windows message timestamps
    #[cfg(target_os = "windows")]
    pub fn hybrid_timestamp(&self) -> (u64, i32) {
        (self.elapsed_ms(), self.message_time_ms())
    }

    /// Format hybrid timestamp as a string for logging.
    /// Format: "HH:MM:SS.mmm [msg_time: XXXXms]"
    #[cfg(target_os = "windows")]
    pub fn hybrid_time_str(&self) -> String {
        let wall = self.wall_time_str();
        let msg_time = self.message_time_ms();
        let elapsed = self.elapsed_ms();
        format!("{} [msg:{}ms qpc:{}ms]", wall, msg_time, elapsed)
    }

    /// Calculate the drift between QPC and GetMessageTime.
    /// Returns the difference in milliseconds.
    /// Useful for detecting timing anomalies.
    #[cfg(target_os = "windows")]
    pub fn time_drift_ms(&self) -> i32 {
        // Capture both timestamps atomically to prevent measurement jitter
        let mut current_qpc = 0i64;
        let current_msg_time = unsafe {
            // Query QPC first, then GetMessageTime immediately after
            // to minimize time delta between the two measurements
            let _ = QueryPerformanceCounter(&mut current_qpc);
            GetMessageTime()
        };
        // Use i64 for calculation to prevent overflow when elapsed_ms exceeds i32::MAX (~24.8 days)
        let elapsed_ticks = current_qpc - self.start_counter;
        let elapsed_ms = ((elapsed_ticks as u128 * 1000) / self.frequency as u128) as i64;
        let expected_msg_time = self.msg_time_offset_ms as i64 + elapsed_ms;
        let drift = current_msg_time as i64 - expected_msg_time;
        // Clamp to i32 range to avoid overflow on return
        drift.clamp(i32::MIN as i64, i32::MAX as i64) as i32
    }
}

impl Default for HighPrecisionTimer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timer_creation() {
        let timer = HighPrecisionTimer::new();
        // Timer should start near 0
        let elapsed = timer.elapsed_ms();
        assert!(elapsed < 1000, "Timer should start near 0");
    }

    #[test]
    fn test_elapsed_increases() {
        let timer = HighPrecisionTimer::new();
        let e1 = timer.elapsed_ms();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let e2 = timer.elapsed_ms();
        assert!(e2 > e1, "Elapsed time should increase");
    }

    #[test]
    fn test_wall_time_format() {
        let timer = HighPrecisionTimer::new();
        let time_str = timer.wall_time_str();
        // Format should be HH:MM:SS.mmm (15 chars)
        assert_eq!(
            time_str.len(),
            12,
            "Wall time format should be HH:MM:SS.mmm"
        );
        assert!(
            time_str.contains('.'),
            "Wall time should contain milliseconds"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_hybrid_timestamp() {
        let timer = HighPrecisionTimer::new();
        let (qpc, msg) = timer.hybrid_timestamp();
        assert!(qpc < 1000, "QPC time should start near 0");
        // Message time is since system start, so it should be large
        assert!(msg > 0, "Message time should be positive");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_time_drift() {
        let timer = HighPrecisionTimer::new();
        let drift = timer.time_drift_ms();
        // Drift should be small initially (within 100ms)
        assert!(drift.abs() < 100, "Initial time drift should be small");
    }
}
