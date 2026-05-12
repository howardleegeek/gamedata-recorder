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
    MouseButton {
        button: String,  // "left", "right", "middle"
        pressed: bool,
    },
    /// Mouse wheel scroll
    MouseWheel {
        direction: String,  // "up", "down"
        amount: i16,
    },
    /// Keyboard key press
    KeyDown {
        key: String,       // e.g., "W", "Space", "Shift"
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
        target: Option<[i32; 3]>,  // x, y, z coordinates
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
/// Represents game state at a specific frame
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct StateEvent {
    pub frame_idx: FrameIdx,
    pub t_ns: TimestampNs,
    /// Player position [x, y, z] (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_pos: Option<[f64; 3]>,
    /// Player rotation [pitch, yaw, roll] (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_rot: Option<[f64; 3]>,
    /// Player health (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<u32>,
    /// Current ammo (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ammo: Option<u32>,
    /// Current score (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<u64>,
}

/// Game event for streams/events.jsonl
/// Represents significant game events (kills, hits, etc.)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GameEvent {
    pub t_ns: TimestampNs,
    pub frame_idx: FrameIdx,
    /// Event type (e.g., "shoot", "hit", "kill", "damage_taken")
    pub r#type: String,
    /// Event-specific data
    pub data: serde_json::Value,
}

/// Timestamp mapping for streams/timestamps.jsonl
/// Links frame index to video presentation timestamp and real-world time
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TimestampMapping {
    pub frame_idx: FrameIdx,
    /// Video presentation timestamp in nanoseconds
    pub video_pts_ns: u64,
    /// Real-world timestamp in nanoseconds
    pub real_t_ns: TimestampNs,
    /// Clock drift between video and system time (nanoseconds)
    pub drift_ns: i64,
}

/// Converts from old InputEventType to new ActionEvent
pub fn convert_input_event_to_action(
    event: &crate::output_types::InputEventType,
    timestamp_ns: TimestampNs,
    frame_idx: FrameIdx,
) -> Option<ActionEvent> {
    use crate::output_types::InputEventType;
    
    let action = match event {
        InputEventType::MouseMove { dx, dy } => {
            // Note: We need absolute position from elsewhere
            // This is a placeholder - actual conversion needs mouse position tracking
            ActionType::MouseMove {
                x: 0,  // Will be filled by caller
                y: 0,
                delta_xy: [*dx, *dy],
            }
        }
        InputEventType::MouseButton { button, pressed } => {
            let button_str = match *button {
                0 => "left",
                1 => "right",
                2 => "middle",
                _ => "unknown",
            };
            ActionType::MouseButton {
                button: button_str.to_string(),
                pressed: *pressed,
            }
        }
        InputEventType::Keyboard { key, pressed } => {
            let key_str = vkey_to_string(*key);
            if *pressed {
                ActionType::KeyDown {
                    key: key_str,
                    scan_code: 0, // Will be filled by caller
                }
            } else {
                ActionType::KeyUp {
                    key: key_str,
                    scan_code: 0,
                }
            }
        }
        InputEventType::Scroll { amount } => {
            ActionType::MouseWheel {
                direction: if *amount > 0 { "up" } else { "down" }.to_string(),
                amount: amount.abs(),
            }
        }
        _ => return None, // Skip other event types
    };
    
    Some(ActionEvent {
        t_ns: timestamp_ns,
        frame_idx,
        action,
    })
}

/// Convert virtual key code to string representation
fn vkey_to_string(vkey: u16) -> String {
    match vkey {
        0x01 => "MouseLeft".to_string(),
        0x02 => "MouseRight".to_string(),
        0x08 => "Backspace".to_string(),
        0x09 => "Tab".to_string(),
        0x0D => "Enter".to_string(),
        0x10 => "Shift".to_string(),
        0x11 => "Control".to_string(),
        0x12 => "Alt".to_string(),
        0x1B => "Escape".to_string(),
        0x20 => "Space".to_string(),
        0x25 => "Left".to_string(),
        0x26 => "Up".to_string(),
        0x27 => "Right".to_string(),
        0x28 => "Down".to_string(),
        0x30..=0x39 => ((vkey - 0x30) as u8 as char).to_string(), // 0-9
        0x41..=0x5A => ((vkey - 0x41 + b'A') as char).to_string(), // A-Z
        _ => format!("VK_{}", vkey),
    }
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
    
    #[test]
    fn test_vkey_to_string() {
        assert_eq!(vkey_to_string(0x20), "Space");
        assert_eq!(vkey_to_string(0x41), "A");
        assert_eq!(vkey_to_string(0x5A), "Z");
        assert_eq!(vkey_to_string(0x30), "0");
        assert_eq!(vkey_to_string(0x39), "9");
        assert_eq!(vkey_to_string(0x25), "Left");
        assert_eq!(vkey_to_string(0x26), "Up");
        assert_eq!(vkey_to_string(0x27), "Right");
        assert_eq!(vkey_to_string(0x28), "Down");
        assert_eq!(vkey_to_string(0x1B), "Escape");
        assert_eq!(vkey_to_string(0x10), "Shift");
        assert_eq!(vkey_to_string(0x11), "Control");
        assert_eq!(vkey_to_string(0x12), "Alt");
        assert_eq!(vkey_to_string(0x0D), "Enter");
        assert_eq!(vkey_to_string(0x09), "Tab");
        assert_eq!(vkey_to_string(0x08), "Backspace");
        assert_eq!(vkey_to_string(0x01), "MouseLeft");
        assert_eq!(vkey_to_string(0x02), "MouseRight");
        assert_eq!(vkey_to_string(0x999), "VK_2457");
    }
}