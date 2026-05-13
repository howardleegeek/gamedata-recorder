//! Integration tests for `input_capture::action_scaffold`.
//!
//! Layer 3 of the input pipeline: each discrete event (click, keypress,
//! scroll) becomes an `Action` with frame alignment and trajectory linkage.
//! These tests verify the buyer wire contract for VLM-ready action records.

#![cfg(target_os = "windows")]

use input_capture::action_scaffold::{Action, ActionType, build_actions};
use input_capture::trajectory::{RawEvent, RawEventKind, segment_trajectories};

// ---------------------------------------------------------------------------
// Empty input
// ---------------------------------------------------------------------------

#[test]
fn empty_events_produce_no_actions() {
    let actions = build_actions(&[], &[], 60.0);
    assert!(actions.is_empty());
}

// ---------------------------------------------------------------------------
// Mouse move is NOT an action — actions are discrete only
// ---------------------------------------------------------------------------

#[test]
fn pure_mouse_moves_produce_no_actions() {
    // Mouse moves update cursor position but do not themselves register
    // as actions. Catches a future refactor that accidentally adds
    // MouseMove to the discrete-action set.
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 1, dy: 1 },
        },
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseMove { dx: 2, dy: 2 },
        },
    ];
    let actions = build_actions(&events, &[], 60.0);
    assert!(actions.is_empty());
}

// ---------------------------------------------------------------------------
// Click action
// ---------------------------------------------------------------------------

#[test]
fn click_event_becomes_click_action() {
    let events = vec![RawEvent {
        timestamp_ns: 33_333_333, // ~frame 2 at 60fps
        kind: RawEventKind::MouseButton {
            button: 1,
            pressed: true,
        },
    }];
    let actions = build_actions(&events, &[], 60.0);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].index, 0);
    assert_eq!(actions[0].timestamp_ns, 33_333_333);
    assert!(matches!(actions[0].action_type, ActionType::Click { .. }));
}

#[test]
fn click_action_carries_cursor_screen_coords() {
    // After accumulating mouse moves, the next click captures cursor xy.
    let events = vec![
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 100, dy: 200 },
        },
        RawEvent {
            timestamp_ns: 1_000_000,
            kind: RawEventKind::MouseButton {
                button: 1,
                pressed: true,
            },
        },
    ];
    let actions = build_actions(&events, &[], 60.0);
    assert_eq!(actions.len(), 1);
    match actions[0].action_type {
        ActionType::Click {
            screen_x, screen_y, ..
        } => {
            assert_eq!(screen_x, 100);
            assert_eq!(screen_y, 200);
        }
        _ => panic!("expected Click"),
    }
}

#[test]
fn mouse_button_release_does_not_produce_action() {
    // Only `pressed: true` registers as a Click.
    let events = vec![RawEvent {
        timestamp_ns: 0,
        kind: RawEventKind::MouseButton {
            button: 1,
            pressed: false,
        },
    }];
    let actions = build_actions(&events, &[], 60.0);
    assert!(actions.is_empty());
}

// ---------------------------------------------------------------------------
// KeyPress action
// ---------------------------------------------------------------------------

#[test]
fn keydown_event_becomes_keypress_action_with_vkey_name() {
    let events = vec![RawEvent {
        timestamp_ns: 0,
        kind: RawEventKind::KeyDown {
            vkey: 0x57, // 'W'
            scan_code: 17,
        },
    }];
    let actions = build_actions(&events, &[], 60.0);
    assert_eq!(actions.len(), 1);
    match actions[0].action_type {
        ActionType::KeyPress { key, key_name } => {
            assert_eq!(key, 0x57);
            assert_eq!(key_name, "W");
        }
        _ => panic!("expected KeyPress"),
    }
}

#[test]
fn keyup_event_does_not_produce_action() {
    let events = vec![RawEvent {
        timestamp_ns: 0,
        kind: RawEventKind::KeyUp {
            vkey: 0x57,
            scan_code: 17,
        },
    }];
    let actions = build_actions(&events, &[], 60.0);
    assert!(actions.is_empty());
}

// ---------------------------------------------------------------------------
// Scroll action
// ---------------------------------------------------------------------------

#[test]
fn scroll_event_becomes_scroll_action() {
    let events = vec![RawEvent {
        timestamp_ns: 0,
        kind: RawEventKind::Scroll { delta: -120 },
    }];
    let actions = build_actions(&events, &[], 60.0);
    assert_eq!(actions.len(), 1);
    match actions[0].action_type {
        ActionType::Scroll { delta } => assert_eq!(delta, -120),
        _ => panic!("expected Scroll"),
    }
}

// ---------------------------------------------------------------------------
// Frame ID computation — buyer wire contract
// ---------------------------------------------------------------------------

#[test]
fn frame_id_is_timestamp_divided_by_frame_interval_at_60fps() {
    // 60 fps: frame interval = 16_666_667 ns. timestamp 50_000_000 -> frame 3.
    let events = vec![RawEvent {
        timestamp_ns: 50_000_000,
        kind: RawEventKind::MouseButton {
            button: 1,
            pressed: true,
        },
    }];
    let actions = build_actions(&events, &[], 60.0);
    let expected_frame_id = 50_000_000_u64 / 16_666_666_u64;
    assert_eq!(actions[0].frame_id, expected_frame_id);
}

#[test]
fn frame_id_is_timestamp_divided_by_frame_interval_at_30fps() {
    // 30 fps: frame interval = 33_333_333 ns. timestamp 100_000_000 -> frame 3.
    let events = vec![RawEvent {
        timestamp_ns: 100_000_000,
        kind: RawEventKind::MouseButton {
            button: 1,
            pressed: true,
        },
    }];
    let actions = build_actions(&events, &[], 30.0);
    assert_eq!(actions[0].frame_id, 3);
}

#[test]
fn frame_id_at_timestamp_zero_is_zero() {
    let events = vec![RawEvent {
        timestamp_ns: 0,
        kind: RawEventKind::MouseButton {
            button: 1,
            pressed: true,
        },
    }];
    let actions = build_actions(&events, &[], 30.0);
    assert_eq!(actions[0].frame_id, 0);
}

#[test]
fn invalid_fps_falls_back_to_60_default() {
    // FPS <= 0 or NaN should fall back to 60.0 (per source comment).
    let events = vec![RawEvent {
        timestamp_ns: 0,
        kind: RawEventKind::MouseButton {
            button: 1,
            pressed: true,
        },
    }];
    for bad_fps in [
        0.0_f64,
        -1.0_f64,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        let actions = build_actions(&events, &[], bad_fps);
        assert_eq!(
            actions.len(),
            1,
            "must still produce action for fps={bad_fps}"
        );
    }
}

// ---------------------------------------------------------------------------
// Action index — strictly monotonic
// ---------------------------------------------------------------------------

#[test]
fn action_indices_are_strictly_monotonic() {
    let events: Vec<RawEvent> = (0..20)
        .map(|i| RawEvent {
            timestamp_ns: i * 16_666_667, // 60 fps
            kind: RawEventKind::MouseButton {
                button: 1,
                pressed: true,
            },
        })
        .collect();
    let actions = build_actions(&events, &[], 60.0);
    assert_eq!(actions.len(), 20);
    for (i, a) in actions.iter().enumerate() {
        assert_eq!(a.index, i as u32);
    }
}

// ---------------------------------------------------------------------------
// Trajectory linkage — `preceding_trajectory_index`
// ---------------------------------------------------------------------------

#[test]
fn click_action_links_to_preceding_trajectory() {
    let events = vec![
        // First trajectory ends with a click at t=2ms
        RawEvent {
            timestamp_ns: 0,
            kind: RawEventKind::MouseMove { dx: 1, dy: 1 },
        },
        RawEvent {
            timestamp_ns: 2_000_000,
            kind: RawEventKind::MouseButton {
                button: 1,
                pressed: true,
            },
        },
    ];
    let trajectories = segment_trajectories(&events, 250.0);
    let actions = build_actions(&events, &trajectories, 60.0);
    assert_eq!(actions.len(), 1);
    assert_eq!(
        actions[0].preceding_trajectory_index,
        Some(0),
        "click should link to trajectory 0"
    );
}

#[test]
fn action_with_no_preceding_trajectory_has_none() {
    // Action with no trajectory before it (immediate keypress with no
    // mouse motion) should have None for preceding_trajectory_index.
    let events = vec![RawEvent {
        timestamp_ns: 1_000_000,
        kind: RawEventKind::KeyDown {
            vkey: 0x57,
            scan_code: 17,
        },
    }];
    let trajectories = segment_trajectories(&events, 250.0);
    let actions = build_actions(&events, &trajectories, 60.0);
    assert_eq!(actions.len(), 1);
    assert!(actions[0].preceding_trajectory_index.is_none());
}

// ---------------------------------------------------------------------------
// Action scaffold fields — VLM annotation placeholder
// ---------------------------------------------------------------------------

#[test]
fn action_scaffold_fields_default_to_none() {
    // R3 contract: action_label, target_entity, bounding_box are
    // placeholders for downstream VLM annotation. Builder MUST leave
    // them as None — otherwise the VLM step has to clean up its inputs.
    let events = vec![RawEvent {
        timestamp_ns: 0,
        kind: RawEventKind::MouseButton {
            button: 1,
            pressed: true,
        },
    }];
    let actions = build_actions(&events, &[], 60.0);
    assert!(actions[0].action_label.is_none());
    assert!(actions[0].target_entity.is_none());
    assert!(actions[0].bounding_box.is_none());
}

#[test]
fn action_serializes_with_snake_case_fields() {
    // Buyer wire contract: snake_case names + skip-if-none for the scaffold.
    let events = vec![RawEvent {
        timestamp_ns: 0,
        kind: RawEventKind::MouseButton {
            button: 1,
            pressed: true,
        },
    }];
    let actions = build_actions(&events, &[], 60.0);
    let a: &Action = &actions[0];
    let v = serde_json::to_value(a).unwrap();
    let obj = v.as_object().unwrap();
    for required in [
        "index",
        "timestamp_ns",
        "frame_id",
        "action_type",
        "preceding_trajectory_index",
    ] {
        assert!(
            obj.contains_key(required),
            "action missing field `{required}`"
        );
    }
    // Scaffold fields are skip-if-none — confirm they're absent for default actions.
    assert!(!obj.contains_key("action_label"));
    assert!(!obj.contains_key("target_entity"));
    assert!(!obj.contains_key("bounding_box"));
}

// ---------------------------------------------------------------------------
// Cursor position accumulation across mouse moves
// ---------------------------------------------------------------------------

#[test]
fn cursor_position_accumulates_via_saturating_add() {
    // Use saturating_add to prevent overflow on extreme mouse movements.
    // We can't easily hit i32::MAX in a test, but we can hit a few thousand
    // deltas and verify the result is the sum.
    let mut events = Vec::new();
    for i in 0..100 {
        events.push(RawEvent {
            timestamp_ns: i * 1_000_000,
            kind: RawEventKind::MouseMove { dx: 7, dy: 11 },
        });
    }
    events.push(RawEvent {
        timestamp_ns: 100_000_000,
        kind: RawEventKind::MouseButton {
            button: 1,
            pressed: true,
        },
    });
    let actions = build_actions(&events, &[], 60.0);
    match actions[0].action_type {
        ActionType::Click {
            screen_x, screen_y, ..
        } => {
            assert_eq!(screen_x, 100 * 7);
            assert_eq!(screen_y, 100 * 11);
        }
        _ => panic!("expected Click"),
    }
}
