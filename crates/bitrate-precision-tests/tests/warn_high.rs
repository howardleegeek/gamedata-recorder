//! Asserts the one-shot warning side-effect for above-band kbps (PRD R2.10).
//!
//! Symmetric to `warn_low.rs`. Lives in its own test binary so the
//! `OnceLock` warning latch in `config_bitrate.rs` is fresh — the
//! `warn_low.rs` binary's latch would already be set if both tests shared
//! a process.

use std::sync::{Arc, Mutex};

use bitrate_precision_tests::{BITRATE_BAND_MAX_KBPS, clamp_recording_bitrate_kbps_detailed};
use tracing::subscriber::with_default;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::{Layer, Registry};

#[derive(Default, Clone)]
struct CapturedEvents {
    inner: Arc<Mutex<Vec<CapturedEvent>>>,
}

#[derive(Debug, Clone)]
struct CapturedEvent {
    level: tracing::Level,
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
fn above_band_clamps_down_and_warns_once() {
    let sink = CapturedEvents::default();
    let subscriber = Registry::default().with(CaptureLayer { sink: sink.clone() });

    let (first_call, second_call) = with_default(subscriber, || {
        let a = clamp_recording_bitrate_kbps_detailed(15_000);
        // Second above-band call inside the same process — should NOT fire
        // a second warning (one-shot latch). The clamped value still
        // returns correctly.
        let b = clamp_recording_bitrate_kbps_detailed(20_000);
        (a, b)
    });

    assert_eq!(
        first_call.effective_kbps, BITRATE_BAND_MAX_KBPS,
        "15000 kbps must clamp down to band maximum (12000)"
    );
    assert!(
        first_call.clamped,
        "15000 kbps is above band — clamp flag must be true"
    );
    assert_eq!(
        second_call.effective_kbps, BITRATE_BAND_MAX_KBPS,
        "second above-band call still clamps to band maximum"
    );
    assert!(second_call.clamped);

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
        Some(u64::from(BITRATE_BAND_MAX_KBPS)),
        "warn event must carry effective_kbps={} (band max)",
        BITRATE_BAND_MAX_KBPS
    );
}
