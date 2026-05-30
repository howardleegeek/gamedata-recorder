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

/// A single row of `frames.jsonl`.
///
/// DATA-HONESTY NOTE (do not "fix" by fabricating real frame indices):
/// `idx` is NOT an encoded-video-frame index. It is a **poll-tick sample
/// index** — incremented once per `FpsLogger::on_frame()` call, which is
/// driven by the recorder's ~1 Hz `update_fps` poll (see
/// `recording.rs::update_fps` -> `on_frame`). The embedded libobs build
/// exposes no per-frame callback, so a true per-frame index is simply not
/// available to this code. Concretely: for a 30 fps recording the encoded mp4
/// has ~30 frames per second, but this struct emits roughly ONE row per second.
///
/// `t_ns` is genuine: it is the real wall-clock nanoseconds elapsed since
/// recording start, captured at the moment of the poll tick. Only the *framing*
/// of `idx` was historically mislabeled — the timestamps themselves are honest.
///
/// The `sampling` field is written into every row so any downstream consumer
/// sees, in-band, that these are ~1 Hz poll-tick samples and must not be
/// treated as a dense per-frame timeline. `idx`/`t_ns` field names are kept
/// unchanged for backward compatibility with existing readers.
#[derive(Debug, Serialize)]
pub struct FrameTimestamp {
    /// Poll-tick (~1 Hz) sample index, zero-based. NOT an encoded-frame index —
    /// see the struct-level note. Per-frame indexing is unavailable from
    /// embedded libobs.
    pub idx: u64,
    /// Genuine wall-clock nanoseconds since recording start (at this poll tick).
    pub t_ns: u64,
    /// In-band honesty marker: states that `idx` is a ~1 Hz poll-tick sample
    /// index, not a per-encoded-frame index. Constant per build; emitted on
    /// every row so the framing semantics travel with the data.
    pub sampling: &'static str,
}

/// Value of `FrameTimestamp::sampling` — documents that each row is a ~1 Hz
/// poll-tick sample, not a per-encoded-frame record.
const FRAME_SAMPLING_KIND: &str = "poll_tick_~1hz";

/// Maximum FPS log entries to keep in memory.
///
/// Data-honesty fix: the old cap was 600 (= 10 min at 1 entry/sec). Any
/// recording longer than 10 minutes had the *head* of its `fps_log.json`
/// silently dropped, so the file looked complete but secretly started partway
/// through the session — with no `truncated` marker to warn a downstream
/// consumer. Raise the cap to 24 hours' worth of 1 Hz samples; at ~32 bytes per
/// `FpsLogEntry` that is only ~2.7 MB even at the limit, so the memory cost of
/// keeping the full, honest log is trivial. `MAX_FOOTAGE` is 10 min, so in
/// normal operation this cap is never reached at all — it exists purely as an
/// anti-OOM backstop for pathological runs, not as a routine truncation.
const MAX_FPS_ENTRIES: usize = 86_400;

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
    /// Poll-tick {idx, t_ns, sampling} rows destined for frames.jsonl.
    /// NOTE: one row per ~1 Hz `on_frame()` poll tick, NOT per encoded video
    /// frame (embedded libobs exposes no per-frame callback) — see
    /// `FrameTimestamp`. So a 10-min recording produces ~600 rows, not
    /// ~600×fps; memory footprint is therefore negligible (<32 KB).
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

    /// Called once per recorder poll tick (~1 Hz, from `update_fps`), NOT once
    /// per captured video frame — embedded libobs gives no per-frame callback.
    /// Records the inter-tick interval and appends a poll-tick sample to
    /// `frames.jsonl`. (Name kept as `on_frame` for call-site stability.)
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

        // Append this poll-tick sample to the frames.jsonl buffer and bump the
        // cumulative counter. `total_frames` is used as the next sample's idx
        // BEFORE increment so the first tick is idx=0. NB: this counts ~1 Hz
        // poll ticks, NOT encoded video frames (see `FrameTimestamp`); the name
        // `total_frames` is retained only because downstream metadata fields
        // already key off it.
        self.frame_timestamps.push(FrameTimestamp {
            idx: self.total_frames,
            t_ns: elapsed.as_nanos() as u64,
            sampling: FRAME_SAMPLING_KIND,
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
    /// `frames.jsonl` (~1 Hz poll-tick timestamps) to the session directory.
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
        // INTENT (data honesty): each line is a ~1 Hz poll-tick sample
        // ({idx, t_ns, sampling}), NOT a per-encoded-frame record — embedded
        // libobs gives us no per-frame callback. `t_ns` is a genuine wall-clock
        // offset; `idx` is a poll-tick index, and every row carries a
        // `sampling: "poll_tick_~1hz"` marker so consumers don't mistake this
        // for a dense per-frame timeline. See `FrameTimestamp` for details.
        // Pre-size the buffer by average line length (~48 bytes incl. the
        // sampling marker) to avoid reallocations.
        let mut jsonl = String::with_capacity(self.frame_timestamps.len() * 48);
        for ft in &self.frame_timestamps {
            let line = serde_json::to_string(ft)?;
            jsonl.push_str(&line);
            jsonl.push('\n');
        }
        let frames_path = session_dir.join(constants::filename::recording::FRAMES_JSONL);
        // Same durability rationale as fps_log above — frames.jsonl is the
        // ~1 Hz poll-tick timestamp series used for coarse video-input
        // alignment (NOT per-frame ground truth); a truncated copy silently
        // mis-aligns training data, so we write it atomically with fsync.
        durable_write::write_atomic_async(&frames_path, jsonl.into_bytes()).await?;
        tracing::info!(
            "Frames JSONL saved: {} frames to {:?}",
            self.frame_timestamps.len(),
            frames_path
        );

        Ok(self.total_frames)
    }
}
