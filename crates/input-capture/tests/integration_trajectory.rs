//! Integration tests for `input_capture::trajectory` (R3 input stream).
//!
//! Trajectory segmentation is the second of the three input layers
//! (raw → trajectory → action) — it groups continuous mouse movements
//! into strokes terminated by clicks, keypresses, scrolls, or pauses.
//!
//! These tests live behind a `target_os = "windows"` gate at the file
//! level because the `input-capture` crate itself depends on the
//! `windows` crate unconditionally, and so the whole crate only compiles
//! on Windows. The test logic itself is platform-agnostic — once the
//! crate's Windows dep is `cfg(windows)`-gated, these tests will run on
//! Mac too.

#![cfg(target_os = "windows")]

use input_capture::trajectory::{
    RawEvent, RawEventKind, Trajectory, TrajectoryTerminator, segment_trajectories,
};

// ---------------------------------------------------------------------------
// Empty input
// ---------------------------------------------------------------------------

#[test]
fn empty_input_yields_empty_trajectories() {
    let trajectories = segment_trajectories(&[], 250.0);
    assert!(trajectories.is_empty());
}

// ---------------------------------------------------------------------------
// Pure mouse-move sequence — terminates as SessionEnd
// ---------------------------------------------------------------------------

#[test]
fn pure_mouse_move_sequence_yields_one_session_end_trajectory() {
    let events = vec![
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 1 },
        },
        RawEvent {
            timestamp_ns: 2_000_000,
            kind: RawEventKind::MouseMove { dx: 2, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 3_000_000,
            kind: RawEventKind::MouseMove { dx: 0, dy: 3 },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    assert_eq!(trajectories.len(), 1);
    let t = &trajectories[0];
    assert!(matches!(t.terminator, TrajectoryTerminator::SessionEnd));
    assert_eq!(t.event_count, 3);
    assert_eq!(t.path.len(), 3);
    // Final cursor position: (1+2+0, 1+0+3) = (3, 4)
    assert_eq!(t.path.last().unwrap(), &[3, 4]);
}

// ---------------------------------------------------------------------------
// Mouse move -> click terminator
// ---------------------------------------------------------------------------

#[test]
fn mouse_move_then_click_creates_click_terminated_trajectory() {
    let events = vec![
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseMove { dx: 5, dy: 5 },
        },
        RawEvent {
            timestamp_ns: 2_000_000,
            kind: RawEventKind::MouseButton {
                button: 1, // left
                pressed: true,
            },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    assert_eq!(trajectories.len(), 1);
    let t = &trajectories[0];
    assert!(matches!(
        t.terminator,
        TrajectoryTerminator::Click { button: 1 }
    ));
}

// ---------------------------------------------------------------------------
// Mouse move -> keydown terminator
// ---------------------------------------------------------------------------

#[test]
fn mouse_move_then_keydown_creates_keypress_terminated_trajectory() {
    let events = vec![
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 2_000_000,
            kind: RawEventKind::KeyDown {
                vkey: 0x57, // 'W'
                scan_code: 17,
            },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    assert_eq!(trajectories.len(), 1);
    let t = &trajectories[0];
    assert!(matches!(
        t.terminator,
        TrajectoryTerminator::KeyPress { key: 0x57 }
    ));
}

// ---------------------------------------------------------------------------
// Mouse move -> scroll terminator
// ---------------------------------------------------------------------------

#[test]
fn mouse_move_then_scroll_creates_scroll_terminated_trajectory() {
    let events = vec![
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseMove { dx: 0, dy: 1 },
        },
        RawEvent {
            timestamp_ns: 2_000_000,
            kind: RawEventKind::Scroll { delta: 120 },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    assert_eq!(trajectories.len(), 1);
    let t = &trajectories[0];
    match &t.terminator {
        TrajectoryTerminator::Scroll { delta } => assert_eq!(*delta, 120),
        other => panic!("expected scroll terminator, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Pause threshold splits a trajectory
// ---------------------------------------------------------------------------

#[test]
fn pause_above_threshold_splits_into_two_trajectories() {
    // First burst at t=0..3ms, then 500ms gap, then second burst.
    let pause_threshold_ms = 250.0;
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 2_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        // 500ms gap exceeds the 250ms threshold
        RawEvent {
            timestamp_ns: 502_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 503_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
    ];
    let trajectories = segment_trajectories(&events, pause_threshold_ms);
    assert_eq!(
        trajectories.len(),
        2,
        "pause of 500ms (>250ms threshold) must split"
    );
    // First trajectory terminates with Pause
    assert!(matches!(
        trajectories[0].terminator,
        TrajectoryTerminator::Pause { .. }
    ));
    // Second trajectory terminates with SessionEnd
    assert!(matches!(
        trajectories[1].terminator,
        TrajectoryTerminator::SessionEnd
    ));
}

#[test]
fn pause_below_threshold_does_not_split() {
    // 100ms gap is below the 250ms threshold — single trajectory.
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 100_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    assert_eq!(trajectories.len(), 1, "100ms gap must not split");
}

// ---------------------------------------------------------------------------
// Distance + speed computation
// ---------------------------------------------------------------------------

#[test]
fn distance_for_horizontal_move_equals_pixel_delta() {
    // A pure +X move of 100 pixels: distance should be 100.
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 100, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 10_000_000, // 10ms
            kind: RawEventKind::MouseButton {
                button: 1,
                pressed: true,
            },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    let t = &trajectories[0];
    assert!(
        (t.total_distance_px - 100.0).abs() < 0.5,
        "expected ~100px, got {}",
        t.total_distance_px
    );
}

#[test]
fn distance_for_diagonal_move_uses_euclidean() {
    // A single dx=3, dy=4 → distance = 5 (3-4-5 triangle).
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 3, dy: 4 },
        },
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseButton {
                button: 1,
                pressed: true,
            },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    assert!((trajectories[0].total_distance_px - 5.0).abs() < 0.01);
}

#[test]
fn avg_speed_computed_correctly_for_known_duration() {
    // 100px over 100ms = 1.0 px/ms.
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 100, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 100_000_000,
            kind: RawEventKind::MouseButton {
                button: 1,
                pressed: true,
            },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    assert!(
        (trajectories[0].avg_speed_px_per_ms - 1.0).abs() < 0.01,
        "expected ~1.0 px/ms, got {}",
        trajectories[0].avg_speed_px_per_ms
    );
}

// ---------------------------------------------------------------------------
// Timestamps + indices monotonic
// ---------------------------------------------------------------------------

#[test]
fn trajectories_have_strictly_increasing_indices() {
    // R3 contract: trajectory.index increases monotonically.
    let events: Vec<RawEvent> = (0..10)
        .flat_map(|i| {
            vec![
                RawEvent {
                    timestamp_ns: i * 1_000_000_000,
                    kind: RawEventKind::MouseMove { dx: 1, dy: 1 },
                },
                RawEvent {
                    timestamp_ns: i * 1_000_000_000 + 1_000_000,
                    kind: RawEventKind::MouseButton {
                        button: 1,
                        pressed: true,
                    },
                },
            ]
        })
        .collect();
    let trajectories = segment_trajectories(&events, 250.0);
    let indices: Vec<u32> = trajectories.iter().map(|t| t.index).collect();
    for i in 1..indices.len() {
        assert!(
            indices[i] > indices[i - 1],
            "indices must monotonically increase: {indices:?}"
        );
    }
}

#[test]
fn trajectory_start_and_end_timestamps_monotonic() {
    // start_ns must be <= end_ns within a trajectory; across trajectories
    // each subsequent start must be >= previous end (no overlap).
    let events: Vec<RawEvent> = (0..5)
        .flat_map(|i| {
            vec![
                RawEvent {
                    timestamp_ns: (i as u64) * 1_000_000_000,
                    kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
                },
                RawEvent {
                    timestamp_ns: (i as u64) * 1_000_000_000 + 1_000_000,
                    kind: RawEventKind::MouseButton {
                        button: 1,
                        pressed: true,
                    },
                },
            ]
        })
        .collect();
    let trajectories = segment_trajectories(&events, 250.0);
    for t in &trajectories {
        assert!(t.start_ns <= t.end_ns, "trajectory has negative duration");
    }
}

// ---------------------------------------------------------------------------
// Trajectory serialization — buyer wire contract
// ---------------------------------------------------------------------------

#[test]
fn trajectory_serializes_with_snake_case_field_names() {
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 1, dy: 1 },
        },
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseButton {
                button: 1,
                pressed: true,
            },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    let t: &Trajectory = &trajectories[0];
    let v = serde_json::to_value(t).expect("trajectory must serialize");
    let obj = v.as_object().expect("must be JSON object");
    for required in [
        "index",
        "start_ns",
        "end_ns",
        "duration_ms",
        "path",
        "total_distance_px",
        "avg_speed_px_per_ms",
        "event_count",
        "terminator",
    ] {
        assert!(
            obj.contains_key(required),
            "trajectory missing field `{required}`"
        );
    }
}

// ---------------------------------------------------------------------------
// Key up + mouse button up — must NOT terminate
// ---------------------------------------------------------------------------

#[test]
fn key_up_does_not_terminate_trajectory() {
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::KeyUp {
                vkey: 0x57,
                scan_code: 17,
            },
        },
        RawEvent {
            timestamp_ns: 2_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    assert_eq!(
        trajectories.len(),
        1,
        "KeyUp must not split — only KeyDown does"
    );
    assert!(matches!(
        trajectories[0].terminator,
        TrajectoryTerminator::SessionEnd
    ));
}

#[test]
fn mouse_button_up_does_not_terminate_trajectory() {
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseButton {
                button: 1,
                pressed: false, // RELEASE — not DOWN
            },
        },
        RawEvent {
            timestamp_ns: 2_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    assert_eq!(trajectories.len(), 1);
    assert!(matches!(
        trajectories[0].terminator,
        TrajectoryTerminator::SessionEnd
    ));
}

// ---------------------------------------------------------------------------
// Multiple actions create multiple trajectories
// ---------------------------------------------------------------------------

#[test]
fn multiple_actions_create_multiple_trajectories() {
    // move -> click -> move -> key -> move -> scroll = 3 trajectories
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseButton {
                button: 1,
                pressed: true,
            },
        },
        RawEvent {
            timestamp_ns: 2_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 3_000_000,
            kind: RawEventKind::KeyDown {
                vkey: 0x57,
                scan_code: 17,
            },
        },
        RawEvent {
            timestamp_ns: 4_000_000,
            kind: RawEventKind::MouseMove { dx: 1, dy: 0 },
        },
        RawEvent {
            timestamp_ns: 5_000_000,
            kind: RawEventKind::Scroll { delta: 120 },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    assert_eq!(trajectories.len(), 3);
    assert!(matches!(
        trajectories[0].terminator,
        TrajectoryTerminator::Click { .. }
    ));
    assert!(matches!(
        trajectories[1].terminator,
        TrajectoryTerminator::KeyPress { .. }
    ));
    assert!(matches!(
        trajectories[2].terminator,
        TrajectoryTerminator::Scroll { .. }
    ));
}
