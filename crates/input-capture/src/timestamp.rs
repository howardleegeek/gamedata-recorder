//! High-precision timer using Windows QueryPerformanceCounter.
//! Sub-microsecond precision, output formatted as HH:MM:SS.mmm

#[cfg(target_os = "windows")]
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// High-precision timer based on QueryPerformanceCounter (Windows)
/// or std::time::Instant (other platforms).
pub struct HighPrecisionTimer {
    #[cfg(target_os = "windows")]
    frequency: i64,
    #[cfg(target_os = "windows")]
    start_counter: i64,
    #[allow(dead_code)]
    start_instant: std::time::Instant,
}

impl HighPrecisionTimer {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        {
            let mut frequency = 0i64;
            let mut start_counter = 0i64;
            unsafe {
                QueryPerformanceFrequency(&mut frequency).ok();
                QueryPerformanceCounter(&mut start_counter).ok();
            }
            Self {
                frequency,
                start_counter,
                start_instant: std::time::Instant::now(),
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
                QueryPerformanceCounter(&mut current).ok();
            }
            let elapsed = current - self.start_counter;
            ((elapsed as u128 * 1000) / self.frequency as u128) as u64
        }

        #[cfg(not(target_os = "windows"))]
        {
            self.start_instant.elapsed().as_millis() as u64
        }
    }

    /// Current wall-clock time as HH:MM:SS.mmm string.
    pub fn wall_time_str(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs();
        let ms = now.subsec_millis();
        let hours = (secs / 3600) % 24;
        let minutes = (secs / 60) % 60;
        let seconds = secs % 60;
        format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, seconds, ms)
    }
}

impl Default for HighPrecisionTimer {
    fn default() -> Self {
        Self::new()
    }
}
