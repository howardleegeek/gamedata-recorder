//! R5.3 — `fps_stats.json` sidecar with real percentile metrics over the
//! session lifetime.
//!
//! The PRD requires the sidecar to expose `{ median, p1, p5, p50, p95, p99
//! of in-game FPS over session lifetime }`. The previous implementation
//! reused the 1 Hz heartbeat aggregate in `fps_log.json`, which approximates
//! FPS but smears across one-second windows and hides single-frame stalls.
//! This module computes the same statistics from REAL per-frame timestamps
//! captured in `FpsLogger::frame_timestamps`.
//!
//! Cross-platform: no Win32 / OBS deps. All inputs are nanosecond timestamps
//! since recording start, identical to the wire shape of `frames.jsonl`.

use serde::{Deserialize, Serialize};

/// Aggregate FPS statistics for one recording session, written to
/// `fps_stats.json` alongside `fps_log.json` and `frames.jsonl`.
///
/// Wire-format note: all fields are `Option<f64>` so a recording with too
/// few frames (single-frame mode, instant cancel) emits `null` for stats
/// that can't be computed, rather than fabricating zeros that look like a
/// stalled encoder. Downstream readers distinguish "0.0 FPS" from "we
/// couldn't tell" — important for AI-training filters.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct FpsStats {
    /// Frame count used to compute the statistics (= number of inter-frame
    /// intervals + 1). Useful for sanity-checking against the
    /// session-level `frame_count` in `metadata.json`.
    #[serde(default)]
    pub frame_count: u64,
    /// Recording duration in nanoseconds, derived from the last frame's
    /// timestamp. Matches `Metadata::duration_ns`.
    #[serde(default)]
    pub duration_ns: u64,
    /// Arithmetic mean of per-frame instantaneous FPS samples. Provided for
    /// continuity with `metadata.json::average_fps`; the percentiles below
    /// are the buyer-requested signal.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mean: Option<f64>,
    /// Minimum instantaneous FPS (worst single-frame stall).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub min: Option<f64>,
    /// Maximum instantaneous FPS (best single-frame interval — usually a
    /// VSync edge artefact, but useful to spot bogus timestamps).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max: Option<f64>,
    /// 50th percentile (median) instantaneous FPS. Buyer-required.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub median: Option<f64>,
    /// 1st percentile instantaneous FPS (worst 1% of frames). Buyer-required —
    /// this is the dominant signal for "did the recording stutter?" in the
    /// downstream AI-training quality filter.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub p1: Option<f64>,
    /// 5th percentile.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub p5: Option<f64>,
    /// 50th percentile — duplicates `median` for downstream consumers that
    /// key off the `p50` label specifically. Kept separate from `median`
    /// because the PRD lists BOTH and we don't want to bikeshed which one
    /// the buyer reads.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub p50: Option<f64>,
    /// 95th percentile.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub p95: Option<f64>,
    /// 99th percentile.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub p99: Option<f64>,
}

impl FpsStats {
    /// Compute `FpsStats` from a slice of per-frame elapsed timestamps (ns
    /// since recording start, monotonically increasing).
    ///
    /// Algorithm:
    ///   1. Convert each adjacent pair `(t[i], t[i-1])` into an instantaneous
    ///      FPS sample: `fps_i = 1e9 / (t[i] - t[i-1])`.
    ///   2. Sort the sample vector ascending.
    ///   3. Pick percentiles via the nearest-rank method
    ///      (`samples[clamp((p/100) * n, 0, n-1)]`), which matches what the
    ///      buyer's downstream Python `numpy.percentile(..., method="lower")`
    ///      script does — and avoids interpolation rounding errors when N is
    ///      small.
    ///
    /// Behaviour at edges:
    ///   - Empty input: every stat is `None`, `frame_count = 0`,
    ///     `duration_ns = 0`. Caller should still write the sidecar so the
    ///     consumer's "missing == abandoned recording" heuristic works.
    ///   - Single frame: `frame_count = 1`, `duration_ns = t[0]`, all stats
    ///     still `None` (no interval to compute FPS from).
    ///   - Two or more frames with at least one strictly-positive interval:
    ///     all stats populated.
    ///
    /// Zero-duration intervals (timestamp ties from quantized clocks) are
    /// dropped from the sample set rather than producing `+Infinity` FPS,
    /// which isn't a valid JSON number. The dropped count is implicitly
    /// reflected in the smaller sample size; downstream consumers can
    /// cross-check against `frame_count`.
    pub fn from_frame_timestamps_ns(timestamps_ns: &[u64]) -> Self {
        let frame_count = timestamps_ns.len() as u64;
        let duration_ns = timestamps_ns.last().copied().unwrap_or(0);

        if timestamps_ns.len() < 2 {
            return Self {
                frame_count,
                duration_ns,
                ..Self::default()
            };
        }

        // Build the instantaneous-FPS sample vector. We treat any
        // non-positive interval (clock tie or clock regression) as "no
        // measurement" and skip it rather than emitting Infinity / NaN.
        let mut samples: Vec<f64> = Vec::with_capacity(timestamps_ns.len());
        for window in timestamps_ns.windows(2) {
            let prev = window[0];
            let cur = window[1];
            if cur > prev {
                let dt_ns = (cur - prev) as f64;
                samples.push(1_000_000_000.0_f64 / dt_ns);
            }
        }

        if samples.is_empty() {
            // Every interval was a tie — degenerate input. Return frame_count
            // and duration so the file still pinpoints the session, but
            // every stat stays None.
            return Self {
                frame_count,
                duration_ns,
                ..Self::default()
            };
        }

        // Ascending sort. `total_cmp` handles NaN deterministically — we
        // shouldn't have any NaNs at this point (we filtered ties) but it's
        // the right primitive for `f64` ordering.
        samples.sort_by(|a, b| a.total_cmp(b));

        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let min = *samples.first().expect("samples non-empty checked above");
        let max = *samples.last().expect("samples non-empty checked above");

        let median = nearest_rank_percentile(&samples, 50.0);
        let p1 = nearest_rank_percentile(&samples, 1.0);
        let p5 = nearest_rank_percentile(&samples, 5.0);
        let p50 = median;
        let p95 = nearest_rank_percentile(&samples, 95.0);
        let p99 = nearest_rank_percentile(&samples, 99.0);

        Self {
            frame_count,
            duration_ns,
            mean: Some(mean),
            min: Some(min),
            max: Some(max),
            median: Some(median),
            p1: Some(p1),
            p5: Some(p5),
            p50: Some(p50),
            p95: Some(p95),
            p99: Some(p99),
        }
    }
}

/// Nearest-rank percentile (no interpolation). Matches `numpy.percentile`
/// with `method="lower"`. `samples` MUST already be sorted ascending and
/// non-empty (the caller enforces both pre-conditions).
///
/// Why nearest-rank instead of linear interpolation: when N is small (a
/// 20-second recording at 30 fps has N≈600), interpolation between two
/// neighbouring samples can shift the reported p99 by enough to fail the
/// buyer's "p99 ≥ 50 fps" SLO check by accident. Nearest-rank picks an
/// actual observed value, which is closer to "what the user saw".
fn nearest_rank_percentile(samples: &[f64], p: f64) -> f64 {
    debug_assert!(
        !samples.is_empty(),
        "nearest_rank_percentile on empty slice"
    );
    debug_assert!((0.0..=100.0).contains(&p), "percentile out of range: {p}");

    // Rank is 1-based in the classical definition: `ceil(p/100 * n)`. We
    // convert to a 0-based index and clamp into bounds.
    let n = samples.len();
    let rank = ((p / 100.0) * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    samples[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 60-fps constant — every percentile should equal 60.0.
    #[test]
    fn constant_60fps_yields_60fps_at_every_percentile() {
        // 600 frames × 1/60s spacing = 10s recording.
        let mut t = Vec::with_capacity(601);
        for i in 0..=600u64 {
            t.push(i * 16_666_667);
        }
        let stats = FpsStats::from_frame_timestamps_ns(&t);

        assert_eq!(stats.frame_count, 601);
        // All percentiles ~60 fps (rounding noise from 16.666666... → 16666667 ns).
        for v in [stats.p1, stats.p5, stats.p50, stats.p95, stats.p99] {
            let v = v.unwrap();
            assert!((v - 60.0).abs() < 0.01, "expected ~60 fps, got {v}");
        }
    }

    /// 600 frames at 60 fps with ten 200ms stalls — p1 should reflect the
    /// stalls. The PRD's example use case ("did the recording stutter?")
    /// hinges on this being correctly detected.
    #[test]
    fn occasional_stalls_drive_p1_down() {
        let mut t = Vec::new();
        let mut cur: u64 = 0;
        for i in 0..600 {
            // Every 60th frame is a 200ms stall (= 5 fps instantaneous).
            // 10 stalls / 600 frames = 1.67%, so p1 will land in the stall set.
            let dt = if i > 0 && i % 60 == 0 {
                200_000_000
            } else {
                16_666_667
            };
            cur += dt;
            t.push(cur);
        }
        let stats = FpsStats::from_frame_timestamps_ns(&t);

        let p1 = stats.p1.unwrap();
        let p50 = stats.p50.unwrap();
        assert!(p1 < 10.0, "stalls should drag p1 < 10 fps, got {p1}");
        assert!(
            (p50 - 60.0).abs() < 1.0,
            "median should still be ~60, got {p50}"
        );
    }

    /// Empty input — every stat stays None. Don't fabricate.
    #[test]
    fn empty_input_yields_none_for_every_stat() {
        let stats = FpsStats::from_frame_timestamps_ns(&[]);
        assert_eq!(stats.frame_count, 0);
        assert_eq!(stats.duration_ns, 0);
        assert_eq!(stats.median, None);
        assert_eq!(stats.p1, None);
        assert_eq!(stats.p99, None);
    }

    /// One frame — frame_count and duration_ns are populated, every stat None.
    #[test]
    fn single_frame_input_returns_count_but_no_stats() {
        let stats = FpsStats::from_frame_timestamps_ns(&[42_000_000]);
        assert_eq!(stats.frame_count, 1);
        assert_eq!(stats.duration_ns, 42_000_000);
        assert_eq!(stats.median, None);
        assert_eq!(stats.p99, None);
    }

    /// All timestamps tied (zero-duration intervals) — degenerate but must
    /// not produce +Infinity. Per the docstring, every stat stays None.
    #[test]
    fn tied_timestamps_do_not_produce_infinity() {
        let t = vec![0u64, 0, 0, 0, 0];
        let stats = FpsStats::from_frame_timestamps_ns(&t);
        assert_eq!(stats.frame_count, 5);
        assert_eq!(stats.duration_ns, 0);
        assert_eq!(stats.median, None);
        assert_eq!(stats.min, None);
    }

    /// Mixed: some ties, some valid intervals. Ties are dropped, valid
    /// intervals drive the stats.
    #[test]
    fn mixed_ties_are_dropped_valid_intervals_drive_stats() {
        // Two valid intervals (16.667ms each = 60 fps) plus one tie.
        let t = vec![0u64, 16_666_667, 16_666_667, 33_333_334];
        let stats = FpsStats::from_frame_timestamps_ns(&t);

        // Two intervals survived (the tie was dropped). Both ~60 fps.
        let median = stats.median.unwrap();
        assert!(
            (median - 60.0).abs() < 0.01,
            "expected median ~60, got {median}"
        );
    }

    /// Nearest-rank: p50 of 4 samples ascending must be index 1 (rank 2)
    /// per `ceil(0.5 * 4) = 2`. This is the same convention numpy uses
    /// with `method="lower"`.
    #[test]
    fn percentile_uses_nearest_rank_not_interpolation() {
        let samples = vec![10.0, 20.0, 30.0, 40.0];
        // p50 = ceil(0.5 * 4) - 1 = 1 → 20.0
        assert_eq!(nearest_rank_percentile(&samples, 50.0), 20.0);
        // p25 = ceil(0.25 * 4) - 1 = 0 → 10.0
        assert_eq!(nearest_rank_percentile(&samples, 25.0), 10.0);
        // p99 = ceil(0.99 * 4) - 1 = 3 → 40.0
        assert_eq!(nearest_rank_percentile(&samples, 99.0), 40.0);
        // p1 = ceil(0.01 * 4) - 1 = 0 → 10.0
        assert_eq!(nearest_rank_percentile(&samples, 1.0), 10.0);
        // p100 = ceil(1.0 * 4) - 1 = 3 → 40.0
        assert_eq!(nearest_rank_percentile(&samples, 100.0), 40.0);
    }

    /// Wire-format regression: serializing an empty FpsStats produces
    /// exactly `{"frame_count":0,"duration_ns":0}` — no None fields leak.
    #[test]
    fn empty_fps_stats_serializes_to_minimal_json() {
        let stats = FpsStats::default();
        let json = serde_json::to_string(&stats).unwrap();
        assert_eq!(json, r#"{"frame_count":0,"duration_ns":0}"#);
    }

    /// Wire-format regression: populated FpsStats includes all percentile keys.
    #[test]
    fn populated_fps_stats_serializes_all_percentile_keys() {
        let mut t = Vec::new();
        let mut cur: u64 = 0;
        for _ in 0..120 {
            cur += 16_666_667;
            t.push(cur);
        }
        let stats = FpsStats::from_frame_timestamps_ns(&t);
        let json = serde_json::to_string(&stats).unwrap();
        for key in [
            "mean", "min", "max", "median", "p1", "p5", "p50", "p95", "p99",
        ] {
            assert!(
                json.contains(&format!("\"{key}\":")),
                "missing {key}: {json}"
            );
        }
    }

    /// Wire-format regression: legacy consumers without the FpsStats schema
    /// should be able to deserialize an empty `{}` object — important for
    /// pre-R5.3 recordings that the migration tool might re-emit.
    #[test]
    fn legacy_empty_object_deserializes_with_all_defaults() {
        let stats: FpsStats = serde_json::from_str("{}").unwrap();
        assert_eq!(stats.frame_count, 0);
        assert_eq!(stats.median, None);
    }

    /// Wire-format regression: round-trip through JSON preserves all values.
    #[test]
    fn fps_stats_round_trips_through_json() {
        let mut t = Vec::new();
        let mut cur: u64 = 0;
        for _ in 0..300 {
            cur += 16_666_667;
            t.push(cur);
        }
        let stats = FpsStats::from_frame_timestamps_ns(&t);
        let json = serde_json::to_string(&stats).unwrap();
        let parsed: FpsStats = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.frame_count, stats.frame_count);
        assert_eq!(parsed.median, stats.median);
        assert_eq!(parsed.p99, stats.p99);
    }
}
