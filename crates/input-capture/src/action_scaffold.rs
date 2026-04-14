//! Layer 3: Action Scaffold — discrete actions with semantic labeling placeholders.
//!
//! Each discrete event (click, keypress, scroll) becomes an Action with:
//! - Timestamp and frame alignment
//! - The preceding trajectory (mouse path leading to this action)
//! - Placeholder fields for downstream annotation:
//!   - action_label: what the player intended (e.g., "shoot", "open_door", "jump")
//!   - target_entity: what they targeted (e.g., "enemy_npc", "door_handle")
//!   - bounding_box: screen region of the target [x, y, w, h]
//!
//! These placeholders enable VLM (Vision-Language Model) auto-labeling:
//! extract the video frame at action_timestamp → ask VLM "what is the player doing?"

use serde::Serialize;

use super::trajectory::Trajectory;

/// A discrete player action with annotation scaffold.
#[derive(Debug, Clone, Serialize)]
pub struct Action {
    /// Index in the session's action sequence
    pub index: u32,
    /// Timestamp in nanoseconds from session start
    pub timestamp_ns: u64,
    /// Corresponding video frame index (timestamp_ns / frame_interval_ns)
    pub frame_id: u64,
    /// The action type
    pub action_type: ActionType,
    /// Index of the preceding trajectory (mouse path leading here)
    /// None if no mouse movement preceded this action
    pub preceding_trajectory_index: Option<u32>,

    // === Annotation scaffold (filled by downstream VLM or human labeling) ===
    /// Semantic label: "shoot", "open_door", "pick_up_item", "jump", etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_label: Option<String>,
    /// Target entity: "enemy_npc", "health_pack", "door", etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_entity: Option<String>,
    /// Bounding box of target on screen: [x, y, width, height]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounding_box: Option<[u32; 4]>,
}

/// The type of discrete action.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ActionType {
    /// Mouse click
    Click {
        button: u16,
        screen_x: i32,
        screen_y: i32,
    },
    /// Keyboard key press
    KeyPress { key: u16, key_name: String },
    /// Scroll wheel
    Scroll { delta: i16 },
}

/// Build action scaffold from raw events and trajectories.
///
/// Links each discrete action to its preceding trajectory and
/// assigns frame IDs based on the target FPS.
pub fn build_actions(
    events: &[super::trajectory::RawEvent],
    trajectories: &[Trajectory],
    fps: f64,
) -> Vec<Action> {
    // Validate fps to prevent division by zero and ensure reasonable frame interval
    if fps <= 0.0 || !fps.is_finite() {
        tracing::warn!("Invalid fps: {}, using default 30.0", fps);
        return Vec::new();
    }
    let frame_interval_ns = (1_000_000_000.0 / fps) as u64;
    // Ensure frame_interval_ns is at least 1 to prevent division by zero
    let frame_interval_ns = frame_interval_ns.max(1);
    let mut actions = Vec::new();
    let mut action_index: u32 = 0;
    let mut cursor_x: i32 = 0;
    let mut cursor_y: i32 = 0;

    for event in events {
        // Track cursor position
        if let super::trajectory::RawEventKind::MouseMove { dx, dy } = &event.kind {
            cursor_x += dx;
            cursor_y += dy;
            continue;
        }

        let action_type = match &event.kind {
            super::trajectory::RawEventKind::MouseButton {
                button,
                pressed: true,
            } => Some(ActionType::Click {
                button: *button,
                screen_x: cursor_x,
                screen_y: cursor_y,
            }),
            super::trajectory::RawEventKind::KeyDown { vkey, .. } => {
                let key_name = super::vkey_names::vkey_to_name(*vkey).to_string();
                Some(ActionType::KeyPress {
                    key: *vkey,
                    key_name,
                })
            }
            super::trajectory::RawEventKind::Scroll { delta } => {
                Some(ActionType::Scroll { delta: *delta })
            }
            _ => None,
        };

        if let Some(action_type) = action_type {
            // Find the preceding trajectory (the one that ended at or just before this action)
            let preceding_traj = trajectories
                .iter()
                .rev()
                .find(|t| t.end_ns <= event.timestamp_ns)
                .map(|t| t.index);

            let frame_id = event.timestamp_ns / frame_interval_ns;

            actions.push(Action {
                index: action_index,
                timestamp_ns: event.timestamp_ns,
                frame_id,
                action_type,
                preceding_trajectory_index: preceding_traj,
                // Annotation scaffold — empty, to be filled by VLM or human
                action_label: None,
                target_entity: None,
                bounding_box: None,
            });
            action_index += 1;
        }
    }

    actions
}
