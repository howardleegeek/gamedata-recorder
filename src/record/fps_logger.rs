use std::path::Path;

use color_eyre::Result;
use serde::Serialize;

use crate::util::durable_write;

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

/// A single row of `frames.jsonl` — frame index + elapsed nanoseconds since
/// recording start. Matches the competitor's schema so downstream tooling can
/// consume either output unchanged.
#[derive(Debug, Serialize)]
pub struct FrameTimestamp {
    /// Zero-based frame index.
    pub idx: u64,
    /// Nanoseconds since recording start.
    pub t_ns: u64,
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
    /// Rolling total of frames observed across the entire recording.
    /// Unlike `entries`, this is never capped — we need the exact count for
    /// the final metadata.json.
    total_frames: u64,
    /// Per-frame {idx, t_ns} rows destined for frames.jsonl.
    /// Memory footprint: ~16B per entry uncompressed × 60fps × 600s ≈ 600KB for a 10-min recording.
    frame_timestamps: Vec<FrameTimestamp>,
}

impl FpsLogger {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            current_second_frame_times: Vec::with_capacity(60),
            current_second: 0,
            last_frame_time: None,
            start_instant: std::time::Instant::now(),
            total_frames: 0,
            frame_timestamps: Vec::new(),
        }
    }

    /// Total frames observed since recording started.
    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }

    /// Elapsed wall-clock time since `FpsLogger::new()` was called.
    pub fn elapsed(&self) -> std::time::Duration {
        self.start_instant.elapsed()
    }

    /// Called each time a video frame is captured.
    /// Records the inter-frame interval for FPS calculation.
    pub fn on_frame(&mut self) {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.start_instant);
        let elapsed_seconds = elapsed.as_secs();

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

        // Append this frame to frames.jsonl buffer and bump the cumulative counter.
        // `total_frames` is used as the next frame's idx BEFORE increment so the
        // first frame is idx=0.
        self.frame_timestamps.push(FrameTimestamp {
            idx: self.total_frames,
            t_ns: elapsed.as_nanos() as u64,
        });
        self.total_frames += 1;

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

    /// Finalize and persist both `fps_log.json` (per-second aggregate) and
    /// `frames.jsonl` (per-frame timestamps) to the session directory.
    ///
    /// Returns the total number of frames observed, so callers can populate
    /// `frame_count` in the session metadata without a second pass.
    pub async fn save(mut self, session_dir: &Path) -> Result<u64> {
        // Finalize any remaining data in the current second
        if !self.current_second_frame_times.is_empty() {
            self.finalize_current_second();
        }

        // Use durable_write so fps_log.json reaches disk atomically and with
        // its data fsync'd. Without the fsync, a crash after `write` committed
        // the directory entry but before the page cache flushed could leave
        // a 0-byte fps_log.json under the final name — breaking downstream
        // FPS-based quality checks that treat missing == zero frames.
        let fps_log_path = session_dir.join(constants::filename::recording::FPS_LOG);
        let fps_json = serde_json::to_string_pretty(&self.entries)?;
        durable_write::write_atomic_async(&fps_log_path, fps_json.into_bytes()).await?;
        tracing::info!(
            "FPS log saved: {} entries to {:?}",
            self.entries.len(),
            fps_log_path
        );

        // Write frames.jsonl — one JSON object per line, no pretty-printing.
        // Pre-size the buffer by average line length (~32 bytes) to avoid reallocations.
        let mut jsonl = String::with_capacity(self.frame_timestamps.len() * 32);
        for ft in &self.frame_timestamps {
            let line = serde_json::to_string(ft)?;
            jsonl.push_str(&line);
            jsonl.push('\n');
        }
        let frames_path = session_dir.join(constants::filename::recording::FRAMES_JSONL);
        // Same rationale as fps_log above — frames.jsonl is the per-frame
        // timestamp ground truth for video-input alignment; a truncated copy
        // silently mis-aligns training data by frames-to-seconds.
        durable_write::write_atomic_async(&frames_path, jsonl.into_bytes()).await?;
        tracing::info!(
            "Frames JSONL saved: {} frames to {:?}",
            self.frame_timestamps.len(),
            frames_path
        );

        Ok(self.total_frames)
    }
}
