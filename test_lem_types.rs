// Minimal test to verify lem_types.rs syntax
use serde::{Deserialize, Serialize};

// Copy the key types from lem_types.rs
type TimestampNs = u64;
type FrameIdx = u64;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ActionType {
    MouseMove {
        x: i32,
        y: i32,
        #[serde(rename = "delta")]
        delta_xy: [i32; 2],
    },
    MouseButton {
        button: String,
        pressed: bool,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ActionEvent {
    pub t_ns: TimestampNs,
    pub frame_idx: FrameIdx,
    #[serde(flatten)]
    pub action: ActionType,
}

fn main() {
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
    println!("Serialized: {}", json);
    assert!(json.contains("\"t_ns\":15642909582000000"));
    assert!(json.contains("\"frame_idx\":1"));
    assert!(json.contains("\"mouse_move\""));
    println!("All tests passed!");
}
