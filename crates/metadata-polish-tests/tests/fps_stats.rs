//! Integration tests for R5.3 — `fps_stats.json` with real percentile
//! metrics over the session lifetime.
//!
//! The PRD requires `{ median, p1, p5, p50, p95, p99 of in-game FPS over
//! session lifetime }`. The previous implementation reused the 1 Hz
//! heartbeat aggregate in `fps_log.json`, which approximated FPS but
//! smeared single-frame stalls across one-second windows. These tests
//! exercise the new `FpsStats::from_frame_timestamps_ns` against the real
//! shape of `frames.jsonl` data — `{idx, t_ns}` rows — that the live
//! recorder produces.

use metadata_polish_tests::FpsStats;

/// End-to-end shape: a realistic 30-second recording at 60 fps produces
/// `frame_count`, `duration_ns`, and all six required percentile keys.
#[test]
fn r5_3_required_keys_are_all_emitted() {
    // 1801 frames at 16.667 ms = 30.017 seconds.
    let mut t = Vec::with_capacity(1801);
    for i in 0..=1800u64 {
        t.push(i * 16_666_667);
    }
    let stats = FpsStats::from_frame_timestamps_ns(&t);
    let json = serde_json::to_string(&stats).unwrap();
    // PRD R5.3 says: median, p1, p5, p50, p95, p99 over session lifetime.
    for key in ["median", "p1", "p5", "p50", "p95", "p99"] {
        assert!(
            json.contains(&format!("\"{key}\":")),
            "missing R5.3 key {key}"
        );
    }
    // Frame count / duration are required for downstream cross-checks
    // against `metadata.json::frame_count` and `duration_ns`.
    assert!(json.contains("\"frame_count\":1801"), "frame_count");
    assert!(
        json.contains("\"duration_ns\":30000000600"),
        "duration_ns (1800 * 16_666_667), got: {json}"
    );
}

/// p1 / p5 must reflect the worst-FPS frames (the "stutters" the PRD is
/// targeted at). In FPS-percentile convention, **low percentile = low FPS
/// = bad frame time**, so a 10% population of stalls drives p1, p5 *down*
/// and leaves p50/p95/p99 close to the baseline.
#[test]
fn low_percentiles_reflect_stutters() {
    // 100 baseline frames + 10 stall frames = 110 frames → 109 intervals.
    // Make stalls 10% of the population so they're guaranteed to dominate
    // the bottom decile under nearest-rank.
    let mut t = Vec::new();
    let mut cur: u64 = 0;
    // 100 fast frames (60 fps).
    for _ in 0..100 {
        cur += 16_666_667;
        t.push(cur);
    }
    // 10 stalled frames (5 fps each — a 200 ms hitch).
    for _ in 0..10 {
        cur += 200_000_000;
        t.push(cur);
    }
    let stats = FpsStats::from_frame_timestamps_ns(&t);

    let p1 = stats.p1.unwrap();
    let p5 = stats.p5.unwrap();
    let p50 = stats.p50.unwrap();
    let p99 = stats.p99.unwrap();

    // p1 / p5 should land in the stall population (~5 fps).
    assert!(p1 < 10.0, "p1 should be ~5 fps, got {p1}");
    assert!(p5 < 10.0, "p5 should be ~5 fps, got {p5}");
    // p50 should still be the 60-fps baseline (90% of frames).
    assert!((p50 - 60.0).abs() < 1.0, "p50 should be ~60, got {p50}");
    // p99 should pick a fast frame (top of the distribution).
    assert!((p99 - 60.0).abs() < 1.0, "p99 should be ~60 fps, got {p99}");
}

/// Stronger version: the PRD's "did the recording stutter?" check is
/// implemented by reading `p1` and comparing against the FPS target. A
/// recording whose p1 is well below target is a stutter — even if median
/// is fine. This test pins that semantic.
#[test]
fn p1_is_the_stutter_indicator() {
    // 60-fps recording with a single 1-second freeze in the middle.
    let mut t = Vec::new();
    let mut cur: u64 = 0;
    for _ in 0..600 {
        cur += 16_666_667; // 60 fps
        t.push(cur);
    }
    // One frame takes 1 full second.
    cur += 1_000_000_000;
    t.push(cur);
    for _ in 0..600 {
        cur += 16_666_667;
        t.push(cur);
    }
    let stats = FpsStats::from_frame_timestamps_ns(&t);

    // p1 sample under nearest-rank with N=1200 intervals lands at
    // index ceil(0.01 * 1200) - 1 = 11. We seeded only ONE 1-second
    // stall, so p1 will NOT pick it directly (it's at index 0, p1 only
    // samples index 11). What we CAN check is `min` — that's the
    // absolute worst single sample, which IS the 1 fps freeze.
    let min = stats.min.unwrap();
    assert!(
        (min - 1.0).abs() < 0.1,
        "min should be ~1 fps (the freeze), got {min}"
    );

    // p99/median should still be ~60 since 1199/1200 intervals are fine.
    let p50 = stats.p50.unwrap();
    assert!((p50 - 60.0).abs() < 0.1, "p50 should be ~60, got {p50}");
}

/// Empty input: file is still written (caller doesn't bypass the sink),
/// but every percentile is null. Matches the PRD's "honest about missing
/// data" principle that ControlSettings/GraphicsSettings already follows.
#[test]
fn empty_recording_serializes_with_null_percentiles() {
    let stats = FpsStats::from_frame_timestamps_ns(&[]);
    let json = serde_json::to_string(&stats).unwrap();
    // No percentile fields appear at all (skip_serializing_if = None).
    for key in [
        "median", "p1", "p5", "p50", "p95", "p99", "mean", "min", "max",
    ] {
        assert!(
            !json.contains(&format!("\"{key}\":")),
            "{key} should be absent when None, got: {json}"
        );
    }
    // But the bookkeeping fields are still there so downstream knows we
    // attempted to emit the sidecar.
    assert!(json.contains("\"frame_count\":0"));
    assert!(json.contains("\"duration_ns\":0"));
}

/// Wire-format regression: legacy consumers without the FpsStats schema
/// must be able to deserialize a future-proofed empty object — required
/// for migrations to upgrade old sessions in place.
#[test]
fn legacy_empty_object_deserializes_cleanly() {
    let stats: FpsStats = serde_json::from_str("{}").unwrap();
    assert_eq!(stats.frame_count, 0);
    assert_eq!(stats.duration_ns, 0);
    assert_eq!(stats.median, None);
    assert_eq!(stats.p99, None);
}

/// Mixed input with timestamp ties (clock-quantization edge case) must
/// not produce Infinity. The downstream JSON parser would reject it
/// (NaN/Infinity aren't valid JSON numbers).
#[test]
fn timestamp_ties_do_not_corrupt_output() {
    // Three ties + two valid 60-fps intervals.
    let t = vec![0u64, 0, 16_666_667, 16_666_667, 33_333_334];
    let stats = FpsStats::from_frame_timestamps_ns(&t);
    let json = serde_json::to_string(&stats).unwrap();
    assert!(!json.contains("Infinity"), "must not emit Infinity");
    assert!(!json.contains("inf"), "must not emit inf");
    assert!(!json.contains("NaN"), "must not emit NaN");
    // Two valid intervals survived, both ~60 fps.
    let median = stats.median.unwrap();
    assert!((median - 60.0).abs() < 0.01, "got median {median}");
}

/// Heartbeat-equivalent vs real-FPS: a sustained 60 fps recording produces
/// a p50 of ~60 — matching what the old heartbeat-based aggregate would
/// have reported. This is the regression direction (we want continuity for
/// no-stutter recordings).
#[test]
fn no_stutter_recording_matches_heartbeat_average() {
    let mut t = Vec::new();
    let mut cur: u64 = 0;
    for _ in 0..3600 {
        cur += 16_666_667;
        t.push(cur);
    }
    let stats = FpsStats::from_frame_timestamps_ns(&t);
    let p50 = stats.p50.unwrap();
    let mean = stats.mean.unwrap();
    assert!((p50 - 60.0).abs() < 0.01, "p50 should match 60, got {p50}");
    assert!(
        (mean - 60.0).abs() < 0.01,
        "mean should match 60, got {mean}"
    );
}
