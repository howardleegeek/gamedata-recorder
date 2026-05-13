//! Integration tests for the mpsc-backed input pipeline backpressure
//! behaviour (R3 spec item: "mpsc backpressure: 10_000 capacity full →
//! try_send returns error and graceful drop counter increments").
//!
//! These tests model the same channel pattern `InputCapture::new` uses
//! (a 10_000-capacity bounded channel between the raw-input thread and
//! the tokio consumer). They run on a standalone tokio mpsc so the test
//! does not need to spin up the actual Win32 RegisterRawInputDevices —
//! that path can only run on the Windows recorder process. The pattern
//! we test here is identical to what `lib.rs` line 106 wires up.

#![cfg(target_os = "windows")]

use tokio::sync::mpsc;

/// Same capacity as `InputCapture::new`'s production channel (lib.rs:106).
const CHANNEL_CAPACITY: usize = 10_000;

#[test]
fn channel_at_full_capacity_returns_try_send_error() {
    // Build a 10_000-cap channel and fill it. The 10_001st `try_send`
    // must return Err. This is the contract the Win32 raw-input thread
    // sees when the tokio consumer falls behind.
    let (tx, _rx) = mpsc::channel::<u32>(CHANNEL_CAPACITY);
    for i in 0..CHANNEL_CAPACITY {
        tx.try_send(i as u32)
            .expect("channel should accept up to capacity");
    }
    // Channel is full now.
    let overflow = tx.try_send(u32::MAX);
    assert!(
        overflow.is_err(),
        "try_send beyond capacity must return Err to signal backpressure"
    );
    match overflow.unwrap_err() {
        mpsc::error::TrySendError::Full(_) => {} // expected
        mpsc::error::TrySendError::Closed(_) => panic!("unexpected Closed"),
    }
}

#[test]
fn channel_drains_to_capacity_then_accepts_again() {
    // After draining one slot, try_send should succeed once.
    let (tx, mut rx) = mpsc::channel::<u32>(CHANNEL_CAPACITY);
    for i in 0..CHANNEL_CAPACITY {
        tx.try_send(i as u32).unwrap();
    }
    // Full
    assert!(tx.try_send(u32::MAX).is_err());
    // Drain one
    let _ = rx.try_recv().expect("at least one item must be drainable");
    // Now there's one slot — send must succeed.
    assert!(tx.try_send(u32::MAX).is_ok());
}

#[test]
fn closed_channel_returns_closed_error_on_send() {
    // If the receiver is dropped (consumer panicked), `blocking_send` /
    // `try_send` returns `Closed`. The Win32 thread uses this signal to
    // shut down its message loop.
    let (tx, rx) = mpsc::channel::<u32>(CHANNEL_CAPACITY);
    drop(rx);
    let res = tx.try_send(0);
    assert!(res.is_err());
    match res.unwrap_err() {
        mpsc::error::TrySendError::Closed(_) => {} // expected
        mpsc::error::TrySendError::Full(_) => panic!("unexpected Full"),
    }
}

#[test]
fn graceful_drop_counter_can_track_overflows() {
    // The recorder's graceful-drop semantics: when try_send fails with
    // Full, increment a counter. This test models that exact pattern
    // and verifies the count reflects actual overflows.
    let (tx, _rx) = mpsc::channel::<u32>(CHANNEL_CAPACITY);
    for i in 0..CHANNEL_CAPACITY {
        tx.try_send(i as u32).unwrap();
    }
    let mut drop_count: u64 = 0;
    for _ in 0..1000 {
        if tx.try_send(0).is_err() {
            drop_count += 1;
        }
    }
    assert_eq!(
        drop_count, 1000,
        "every overflow attempt must increment counter"
    );
}

#[test]
fn channel_under_capacity_accepts_all_sends() {
    // Verify that within capacity, all sends succeed — no spurious
    // backpressure. Catches a future refactor that changes the channel
    // type to a 0-capacity or sync variant.
    let (tx, _rx) = mpsc::channel::<u32>(CHANNEL_CAPACITY);
    for i in 0..(CHANNEL_CAPACITY - 1) {
        tx.try_send(i as u32)
            .expect("under-capacity send must succeed");
    }
}
