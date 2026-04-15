//! LEM (Large Entity Models) Format Core Types
//!
//! This module defines the core data types for the LEM output format,
//! which is designed for AI training data standardization.

use serde::{Deserialize, Serialize};

/// Timestamp in nanoseconds since Unix epoch
pub type TimestampNs = u64;

/// Frame index (0-based)
pub type FrameIdx = u64;

/// Action/Event type enumeration
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    /// Mouse movement with absolute position and delta
    MouseMove {
        x: i32,
        y: i32,
        #[serde(rename = "delta")]
        delta_xy: [i32; 2],
    },
    /// Mouse button press/release
    MouseButton { button: String, pressed: bool },
    /// Mouse wheel scroll
    MouseWheel { direction: String, amount: i16 },
    /// Keyboard key press
    KeyDown {
        key: String,
        #[serde(rename = "scancode")]
        scan_code: u32,
    },
    /// Keyboard key release
    KeyUp {
        key: String,
        #[serde(rename = "scancode")]
        scan_code: u32,
    },
    /// Game-specific command (optional, for advanced use)
    GameCommand {
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<[i32; 3]>,
    },
}

/// Action event for streams/actions.jsonl
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ActionEvent {
    /// Timestamp in nanoseconds
    pub t_ns: TimestampNs,
    /// Frame index this action belongs to
    pub frame_idx: FrameIdx,
    /// Action type with specific data
    #[serde(flatten)]
    pub action: ActionType,
}

/// State event for streams/states.jsonl
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct StateEvent {
    pub frame_idx: FrameIdx,
    pub t_ns: TimestampNs,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_pos: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_rot: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ammo: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<u64>,
}

/// Game event for streams/events.jsonl
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GameEvent {
    pub t_ns: TimestampNs,
    pub frame_idx: FrameIdx,
    pub r#type: String,
    pub data: serde_json::Value,
}

/// Timestamp mapping for streams/timestamps.jsonl
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TimestampMapping {
    pub frame_idx: FrameIdx,
    pub video_pts_ns: u64,
    pub real_t_ns: TimestampNs,
    pub drift_ns: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_event_serialization() {
        let event = ActionEvent {
            t_ns: 15642909582000000,
            frame_idx: 1,
            action: ActionType::MouseMove {
                x: 965,
                y: 542,
                delta_xy: [5, 2],
            },
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"t_ns\":15642909582000000"));
        assert!(json.contains("\"frame_idx\":1"));
        assert!(json.contains("\"mouse_move\""));
    }

    #[test]
    fn test_timestamp_mapping_serialization() {
        let mapping = TimestampMapping {
            frame_idx: 0,
            video_pts_ns: 0,
            real_t_ns: 1564290958000000,
            drift_ns: 0,
        };

        let json = serde_json::to_string(&mapping).unwrap();
        let parsed: TimestampMapping = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.frame_idx, 0);
    }
}
