use std::path::Path;

use color_eyre::Result;
use serde::Serialize;

/// Per-second FPS statistics entry (buyer spec requirement).
#[derive(Debug, Serialize)]
pub struct FpsLogEntry {
    /// Second index from recording start (0-based)
    pub second: u64,
    /// Number of frames captured in this second
    pub fps: u32,
    /// Average frame time in milliseconds
    pub frame_time_avg_ms: f64,
    /// Maximum frame time in milliseconds (worst frame)
    pub frame_time_max_ms: f64,
}

/// Maximum FPS log entries to keep in memory (10 minutes at 1 entry/second = 600).
/// Older entries are dropped to prevent unbounded growth during long sessions.
const MAX_FPS_ENTRIES: usize = 600;

/// Accumulates frame timing data and produces per-second FPS statistics.
pub struct FpsLogger {
    /// All completed per-second entries (capped at MAX_FPS_ENTRIES)
    entries: Vec<FpsLogEntry>,
    /// Frame times (ms) accumulated within the current second
    current_second_frame_times: Vec<f64>,
    /// Which second we're currently accumulating (0-based)
    current_second: u64,
    /// Timestamp (Instant) of the last frame arrival
    last_frame_time: Option<std::time::Instant>,
    /// When the recording started
    start_instant: std::time::Instant,
}

impl FpsLogger {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            current_second_frame_times: Vec::with_capacity(60),
            current_second: 0,
            last_frame_time: None,
            start_instant: std::time::Instant::now(),
        }
    }

    /// Called each time a video frame is captured.
    /// Records the inter-frame interval for FPS calculation.
    pub fn on_frame(&mut self) {
        let now = std::time::Instant::now();
        let elapsed_seconds = now.duration_since(self.start_instant).as_secs();

        // If we've moved to a new second, finalize the previous one
        while self.current_second < elapsed_seconds {
            self.finalize_current_second();
            self.current_second += 1;
            self.current_second_frame_times.clear();
        }

        // Record frame interval
        if let Some(last) = self.last_frame_time {
            let frame_time_ms = now.duration_since(last).as_secs_f64() * 1000.0;
            self.current_second_frame_times.push(frame_time_ms);
        }

        self.last_frame_time = Some(now);
    }

    /// Finalize the current second's data into an FpsLogEntry.
    fn finalize_current_second(&mut self) {
        // frame_times stores intervals between frames, so N intervals = N+1 frames.
        // Exception: if no intervals were recorded, fps is 0 (no frames at all)
        // or 1 (single frame, no interval to measure).
        let fps = if self.current_second_frame_times.is_empty() {
            0u32
        } else {
            (self.current_second_frame_times.len() + 1) as u32
        };
        let (avg, max) = if self.current_second_frame_times.is_empty() {
            (0.0, 0.0)
        } else {
            let sum: f64 = self.current_second_frame_times.iter().sum();
            let avg = sum / self.current_second_frame_times.len() as f64;
            let max = self
                .current_second_frame_times
                .iter()
                .copied()
                .fold(0.0_f64, f64::max);
            (avg, max)
        };

        self.entries.push(FpsLogEntry {
            second: self.current_second,
            fps,
            frame_time_avg_ms: (avg * 100.0).round() / 100.0,
            frame_time_max_ms: (max * 100.0).round() / 100.0,
        });

        // Cap entries to prevent unbounded memory growth during long sessions.
        // Keep the most recent entries (tail).
        if self.entries.len() > MAX_FPS_ENTRIES {
            let drain_count = self.entries.len() - MAX_FPS_ENTRIES;
            self.entries.drain(..drain_count);
        }
    }

    /// Get the current real-time FPS (frames in the last completed second).
    #[allow(dead_code)]
    pub fn current_fps(&self) -> Option<f64> {
        self.entries.last().map(|e| e.fps as f64)
    }

    /// Finalize and write fps_log.json to the session directory.
    pub async fn save(mut self, session_dir: &Path) -> Result<()> {
        // Finalize any remaining data in the current second
        if !self.current_second_frame_times.is_empty() {
            self.finalize_current_second();
        }

        let path = session_dir.join(constants::filename::recording::FPS_LOG);
        let json = serde_json::to_string_pretty(&self.entries)?;
        tokio::fs::write(&path, json).await?;
        tracing::info!(
            "FPS log saved: {} entries to {:?}",
            self.entries.len(),
            path
        );
        Ok(())
    }
}
