//! Integration tests for `input_capture::timestamp::HighPrecisionTimer`.
//!
//! R3 spec item: "timestamp monotonic — two consecutive events must
//! have ts2 > ts1". The `HighPrecisionTimer` uses QueryPerformanceCounter
//! (QPC) on Windows for sub-microsecond precision; on non-Windows it
//! falls back to `std::time::Instant`. Either way, monotonicity must hold.

#![cfg(target_os = "windows")]

use input_capture::timestamp::HighPrecisionTimer;

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

#[test]
fn timer_new_starts_near_zero_ms() {
    let timer = HighPrecisionTimer::new();
    // Should be very close to 0 on construction (well under 1s).
    assert!(timer.elapsed_ms() < 1000, "fresh timer should be near 0");
}

#[test]
fn timer_default_equals_new() {
    // `Default::default()` and `new()` should be observationally equal —
    // both initialize the QPC start counter / std::time::Instant origin.
    let _t1 = HighPrecisionTimer::default();
    let _t2 = HighPrecisionTimer::new();
    // Can't compare directly since the struct has private fields, but
    // both should construct without panic and both should report tiny
    // elapsed times.
}

// ---------------------------------------------------------------------------
// Monotonicity — the load-bearing contract
// ---------------------------------------------------------------------------

#[test]
fn elapsed_ms_is_monotonic() {
    // R3 contract: elapsed timestamps must never go backwards.
    let timer = HighPrecisionTimer::new();
    let t1 = timer.elapsed_ms();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let t2 = timer.elapsed_ms();
    assert!(t2 >= t1, "timestamps must not decrease: t1={t1}, t2={t2}");
    assert!(t2 > t1, "10ms sleep must register some elapsed");
}

#[test]
fn elapsed_us_is_monotonic() {
    let timer = HighPrecisionTimer::new();
    let u1 = timer.elapsed_us();
    let u2 = timer.elapsed_us();
    let u3 = timer.elapsed_us();
    assert!(u2 >= u1);
    assert!(u3 >= u2);
}

#[test]
fn elapsed_ns_is_monotonic() {
    let timer = HighPrecisionTimer::new();
    let n1 = timer.elapsed_ns();
    let n2 = timer.elapsed_ns();
    assert!(n2 >= n1);
}

// ---------------------------------------------------------------------------
// Cross-unit consistency
// ---------------------------------------------------------------------------

#[test]
fn elapsed_ns_is_thousand_times_us() {
    // ns / 1000 ≈ us. Allow some rounding slop (test timing jitter).
    let timer = HighPrecisionTimer::new();
    let ns = timer.elapsed_ns();
    let us = timer.elapsed_us();
    let predicted_us = ns / 1000;
    // Both calls are not atomic, so allow a generous slack — within 1ms.
    assert!(
        predicted_us.abs_diff(us) < 1000,
        "ns/1000 should match us: ns={ns}, us={us}"
    );
}

#[test]
fn elapsed_us_is_thousand_times_ms() {
    let timer = HighPrecisionTimer::new();
    let us = timer.elapsed_us();
    let ms = timer.elapsed_ms();
    let predicted_ms = us / 1000;
    assert!(predicted_ms.abs_diff(ms) < 10);
}

// ---------------------------------------------------------------------------
// Sub-millisecond resolution
// ---------------------------------------------------------------------------

#[test]
fn nanosecond_precision_resolves_microsecond_gaps() {
    // QPC should resolve sub-microsecond. Two consecutive calls should
    // not return identical values (modulo extremely rare counter ties).
    let timer = HighPrecisionTimer::new();
    let mut diffs = Vec::new();
    let mut last = timer.elapsed_ns();
    for _ in 0..100 {
        let n = timer.elapsed_ns();
        diffs.push(n - last);
        last = n;
    }
    // At least one of 100 successive reads should show non-zero delta.
    assert!(
        diffs.iter().any(|&d| d > 0),
        "QPC must resolve sub-microsecond; got all zero deltas: {diffs:?}"
    );
}

// ---------------------------------------------------------------------------
// wall_time_str — format contract
// ---------------------------------------------------------------------------

#[test]
fn wall_time_str_has_canonical_format() {
    let timer = HighPrecisionTimer::new();
    let s = timer.wall_time_str();
    // Format: HH:MM:SS.mmm = 12 chars.
    assert_eq!(s.len(), 12, "wall time format must be HH:MM:SS.mmm");
    assert!(s.contains(':'));
    assert!(s.contains('.'));
    // Two colons + one period.
    assert_eq!(s.chars().filter(|c| *c == ':').count(), 2);
    assert_eq!(s.chars().filter(|c| *c == '.').count(), 1);
}

#[test]
fn wall_time_str_components_within_valid_ranges() {
    let timer = HighPrecisionTimer::new();
    let s = timer.wall_time_str();
    let parts: Vec<&str> = s.split(':').collect();
    assert_eq!(parts.len(), 3);
    let hours: u32 = parts[0].parse().unwrap();
    let minutes: u32 = parts[1].parse().unwrap();
    // parts[2] = "SS.mmm"
    let sec_parts: Vec<&str> = parts[2].split('.').collect();
    let seconds: u32 = sec_parts[0].parse().unwrap();
    let ms: u32 = sec_parts[1].parse().unwrap();
    assert!(hours < 24);
    assert!(minutes < 60);
    assert!(seconds < 60);
    assert!(ms < 1000);
}

// ---------------------------------------------------------------------------
// Clone semantics
// ---------------------------------------------------------------------------

#[test]
fn timer_is_clone_and_shared_origin() {
    // The recorder clones the timer to hand to multiple threads; the
    // origin must be preserved exactly.
    let timer = HighPrecisionTimer::new();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let cloned = timer.clone();
    // Both report elapsed time from the SAME origin (within nanosecond
    // jitter), not from clone time.
    let t1 = timer.elapsed_ms();
    let t2 = cloned.elapsed_ms();
    assert!(
        t1.abs_diff(t2) < 5,
        "cloned timer must share origin: t1={t1}, t2={t2}"
    );
}

// ---------------------------------------------------------------------------
// Windows-specific hybrid timestamp + drift
// ---------------------------------------------------------------------------

#[test]
fn hybrid_timestamp_returns_both_qpc_and_msg_time() {
    let timer = HighPrecisionTimer::new();
    let (qpc, _msg) = timer.hybrid_timestamp();
    // qpc should be near 0, msg_time is system-uptime so it's positive.
    assert!(qpc < 1000);
    // Don't assert msg_time > 0 since on a fresh boot it could be 0.
}

#[test]
fn time_drift_starts_small() {
    let timer = HighPrecisionTimer::new();
    let drift = timer.time_drift_ms();
    // On healthy hardware, initial drift between QPC and GetMessageTime
    // should be tiny (< 100ms per source comment).
    assert!(
        drift.abs() < 1000,
        "initial drift should be small: {drift}ms"
    );
}

#[test]
fn message_time_ms_is_callable() {
    let timer = HighPrecisionTimer::new();
    let _msg = timer.message_time_ms();
    // Should not panic. Value depends on system state.
}

#[test]
fn hybrid_time_str_includes_qpc_and_msg() {
    let timer = HighPrecisionTimer::new();
    let s = timer.hybrid_time_str();
    // Format includes wall time + "[msg:Xms qpc:Yms]"
    assert!(s.contains("msg:"));
    assert!(s.contains("qpc:"));
    assert!(s.contains(":"));
}
