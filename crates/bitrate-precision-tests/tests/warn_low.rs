//! Asserts the one-shot warning side-effect for below-band kbps (PRD R2.10).
//!
//! Each file in `tests/` compiles to a separate test binary, so the
//! process-global `OnceLock` warning latch in `config_bitrate.rs` is fresh
//! for this entire binary. We use a single `#[test]` here to avoid
//! intra-binary test ordering hazards: multiple tests inside one binary
//! share the latch, and only the first below-band call would fire the
//! warning.

use std::sync::{Arc, Mutex};

use bitrate_precision_tests::{BITRATE_BAND_MIN_KBPS, clamp_recording_bitrate_kbps_detailed};
use tracing::subscriber::with_default;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::{Layer, Registry};

/// Minimal `tracing_subscriber::Layer` that captures every event's `level`
/// and the value of the `effective_kbps` field (if present) into a shared
/// `Vec`. We keep it small on purpose — the production code only emits
/// structured fields, no string formatting, so we don't need a full visitor.
#[derive(Default, Clone)]
struct CapturedEvents {
    inner: Arc<Mutex<Vec<CapturedEvent>>>,
}

#[derive(Debug, Clone)]
struct CapturedEvent {
    level: tracing::Level,
    /// Captured value of the `effective_kbps` structured field, if the
    /// event carried one. Captured as a `u64` because `tracing`'s visitor
    /// trait exposes integers via `record_u64`.
    effective_kbps: Option<u64>,
}

struct CaptureLayer {
    sink: CapturedEvents,
}

impl<S> Layer<S> for CaptureLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Visitor {
            effective_kbps: Option<u64>,
        }
        impl tracing::field::Visit for Visitor {
            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                if field.name() == "effective_kbps" {
                    self.effective_kbps = Some(value);
                }
            }
            // The clamp also emits `requested_kbps`, `min_kbps`, `max_kbps`
            // as u64 — capture is no-op for those (we only need to assert
            // on `effective_kbps`). The default `record_debug` is a no-op,
            // so the other fields are dropped on the floor, which is what
            // we want.
            fn record_debug(
                &mut self,
                _field: &tracing::field::Field,
                _value: &dyn std::fmt::Debug,
            ) {
            }
        }

        let mut visitor = Visitor {
            effective_kbps: None,
        };
        event.record(&mut visitor);

        if let Ok(mut guard) = self.sink.inner.lock() {
            guard.push(CapturedEvent {
                level: *event.metadata().level(),
                effective_kbps: visitor.effective_kbps,
            });
        }
    }
}

#[test]
fn below_band_clamps_up_and_warns_once() {
    let sink = CapturedEvents::default();
    let subscriber = Registry::default().with(CaptureLayer { sink: sink.clone() });

    let (first_call, second_call) = with_default(subscriber, || {
        let a = clamp_recording_bitrate_kbps_detailed(5_000);
        // Second below-band call inside the same process — should NOT fire
        // a second warning (one-shot latch). The clamped value still
        // returns correctly.
        let b = clamp_recording_bitrate_kbps_detailed(4_000);
        (a, b)
    });

    // Return-value assertions.
    assert_eq!(
        first_call.effective_kbps, BITRATE_BAND_MIN_KBPS,
        "5000 kbps must clamp up to band minimum (6000)"
    );
    assert!(
        first_call.clamped,
        "5000 kbps is below band — clamp flag must be true"
    );
    assert_eq!(
        second_call.effective_kbps, BITRATE_BAND_MIN_KBPS,
        "second below-band call still clamps to band minimum"
    );
    assert!(second_call.clamped);

    // Warning side-effect: exactly ONE warn event, captured by the
    // subscriber. The events vec may contain other tracing levels (info,
    // debug) if the test harness emits any — filter to warn only.
    let events = sink.inner.lock().expect("sink poisoned");
    let warns: Vec<&CapturedEvent> = events
        .iter()
        .filter(|e| e.level == tracing::Level::WARN)
        .collect();
    assert_eq!(
        warns.len(),
        1,
        "expected exactly one WARN event from the one-shot latch; got {} events: {:?}",
        warns.len(),
        events
    );
    assert_eq!(
        warns[0].effective_kbps,
        Some(u64::from(BITRATE_BAND_MIN_KBPS)),
        "warn event must carry effective_kbps={} (band min)",
        BITRATE_BAND_MIN_KBPS
    );
}
