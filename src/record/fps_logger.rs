use std::path::Path;

use color_eyre::Result;
use serde::Serialize;
use tokio::io::AsyncWriteExt as _;

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

/// Accumulates frame timing data and produces per-second FPS statistics.
pub struct FpsLogger {
    /// All completed per-second entries
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
        let elapsed_seconds = now.saturating_duration_since(self.start_instant).as_secs();

        // If we've moved to a new second, finalize all intervening seconds
        // including empty ones (0 fps) to prevent gaps in the FPS log
        while self.current_second < elapsed_seconds {
            self.finalize_current_second();
            self.current_second += 1;
            // Clear frame times after finalizing - new second starts empty
            // (will be populated below if this is the current elapsed second)
            self.current_second_frame_times.clear();
        }
        // Jump to current second if we skipped more than MAX_CATCH_UP_SECONDS
        // (discarding empty entries for time when no recording was active)
        if self.current_second < elapsed_seconds {
            self.current_second = elapsed_seconds;
            self.current_second_frame_times.clear();
        }

        // Record frame interval
        if let Some(last) = self.last_frame_time {
            let frame_time_ms = now.saturating_duration_since(last).as_secs_f64() * 1000.0;
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
    }

    /// Get the current real-time FPS including frames from the in-progress second.
    /// Returns the frame count from the current second if available, otherwise
    /// falls back to the last completed second's FPS.
    pub fn current_fps(&self) -> Option<f64> {
        // Count frames in the current in-progress second
        // frame_times stores intervals between frames, so N intervals = N+1 frames
        let current_second_frames = if self.current_second_frame_times.is_empty() {
            0u32
        } else {
            (self.current_second_frame_times.len() + 1) as u32
        };

        // Return current second's frame count if we have frames in progress
        if current_second_frames > 0 {
            Some(current_second_frames as f64)
        } else {
            // Fall back to last completed second if available
            self.entries.last().map(|e| e.fps as f64)
        }
    }

    /// Finalize and write fps_log.json to the session directory.
    /// Uses atomic write pattern (write .tmp then rename) for crash durability.
    pub async fn save(mut self, session_dir: &Path) -> Result<()> {
        // Calculate total elapsed seconds of the recording
        let elapsed_seconds = std::time::Instant::now()
            .duration_since(self.start_instant)
            .as_secs();

        // Finalize all seconds up to the recording end time, including empty ones.
        // This ensures seconds with 0 frames (loading screens, game freezes) are
        // recorded as 0 FPS rather than being omitted from the log.
        while self.current_second <= elapsed_seconds {
            self.finalize_current_second();
            self.current_second += 1;
            self.current_second_frame_times.clear();
        }

        let path = session_dir.join(constants::filename::recording::FPS_LOG);
        let json = serde_json::to_string_pretty(&self.entries)?;

        // Atomic write pattern: write to temp file, then rename
        // Prevents corruption if the process crashes mid-write
        let temp_path = path.with_extension("tmp");
        tokio::fs::write(&temp_path, json).await?;
        tokio::fs::rename(&temp_path, &path).await?;

        tracing::info!(
            "FPS log saved: {} entries to {:?}",
            self.entries.len(),
            path
        );
        Ok(())
    }
}
