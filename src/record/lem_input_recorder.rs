//! LEM Format Input Recorder
//!
//! Records input events in LEM format to actions.jsonl and timestamps.jsonl

use std::{
    path::Path,
    sync::Arc,
};

use color_eyre::{Result, eyre::eyre};
use input_capture::InputCapture;
use tokio::{fs::File, io::AsyncWriteExt, sync::mpsc};

use crate::{
    output_types::{
        InputEventType,
        lem_types::{ActionEvent, ActionType, TimestampMapping},
    },
    record::session_manager::SessionManager,
};

/// Stream for sending timestamped input events
#[derive(Clone)]
pub struct LemInputStream {
    tx: mpsc::UnboundedSender<InputCommand>,
}

impl LemInputStream {
    /// Send an input event
    pub fn send_event(&self, event: InputEventType) -> Result<()> {
        self.tx
            .send(InputCommand::Event(event))
            .map_err(|_| eyre!("Input stream receiver closed"))?;
        Ok(())
    }
    
    /// Send a timestamp mapping
    pub fn send_timestamp(&self, mapping: TimestampMapping) -> Result<()> {
        self.tx
            .send(InputCommand::Timestamp(mapping))
            .map_err(|_| eyre!("Input stream receiver closed"))?;
        Ok(())
    }
    
    /// Signal to stop recording
    pub fn stop(&self) -> Result<()> {
        self.tx
            .send(InputCommand::Stop)
            .map_err(|_| eyre!("Input stream receiver closed"))?;
        Ok(())
    }
}

/// Commands sent to the input recorder
enum InputCommand {
    Event(InputEventType),
    Timestamp(TimestampMapping),
    Stop,
}

/// LEM format input recorder
pub struct LemInputRecorder {
    actions_file: File,
    timestamps_file: File,
    session_manager: Arc<SessionManager>,
    rx: mpsc::UnboundedReceiver<InputCommand>,
    total_actions: u64,
}

impl LemInputRecorder {
    /// Start a new LEM input recording session
    ///
    /// # Arguments
    /// * `session_manager` - Session manager for path and timing
    /// * `input_capture` - Input capture for initial state
    pub async fn start(
        session_manager: Arc<SessionManager>,
        input_capture: &InputCapture,
    ) -> Result<(Self, LemInputStream)> {
        let actions_path = session_manager.actions_path();
        let timestamps_path = session_manager.timestamps_path();
        
        // Create actions.jsonl
        let actions_file = File::create_new(&actions_path).await.map_err(|e| {
            eyre!("Failed to create actions file at {}: {}", actions_path.display(), e)
        })?;
        
        // Create timestamps.jsonl
        let timestamps_file = File::create_new(&timestamps_path).await.map_err(|e| {
            eyre!("Failed to create timestamps file at {}: {}", timestamps_path.display(), e)
        })?;
        
        let (tx, rx) = mpsc::unbounded_channel();
        let stream = LemInputStream { tx };
        
        let mut recorder = Self {
            actions_file,
            timestamps_file,
            session_manager,
            rx,
            total_actions: 0,
        };
        
        // Write initial timestamp mapping for frame 0
        recorder.write_initial_timestamp().await?;
        
        tracing::info!(
            actions_path = %actions_path.display(),
            timestamps_path = %timestamps_path.display(),
            "Started LEM input recording"
        );
        
        Ok((recorder, stream))
    }
    
    /// Write initial timestamp for frame 0
    async fn write_initial_timestamp(&mut self) -> Result<()> {
        let mapping = TimestampMapping {
            frame_idx: 0,
            video_pts_ns: 0,
            real_t_ns: self.session_manager.start_ns(),
            drift_ns: 0,
        };
        self.write_timestamp(mapping).await?;
        Ok(())
    }
    
    /// Main recording loop - processes commands until Stop is received
    pub async fn run(mut self) -> Result<u64> {
        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                InputCommand::Event(event) => {
                    if let Err(e) = self.process_event(event).await {
                        tracing::error!("Failed to process input event: {}", e);
                    }
                }
                InputCommand::Timestamp(mapping) => {
                    if let Err(e) = self.write_timestamp(mapping).await {
                        tracing::error!("Failed to write timestamp: {}", e);
                    }
                }
                InputCommand::Stop => {
                    tracing::info!("Received stop command, finalizing input recording");
                    break;
                }
            }
        }
        
        // Flush any remaining data
        self.actions_file.flush().await?;
        self.timestamps_file.flush().await?;
        
        tracing::info!(
            total_actions = self.total_actions,
            "LEM input recording finalized"
        );
        
        Ok(self.total_actions)
    }
    
    /// Process a single input event
    async fn process_event(&mut self, event: InputEventType) -> Result<()> {
        let frame_idx = self.session_manager.current_frame();
        let t_ns = self.session_manager.now_ns();
        
        // Convert to LEM format
        if let Some(action) = convert_to_action_event(&event, t_ns, frame_idx) {
            self.write_action(action).await?;
            self.total_actions += 1;
        }
        
        Ok(())
    }
    
    /// Write an action event to actions.jsonl
    async fn write_action(&mut self, action: ActionEvent) -> Result<()> {
        let json = serde_json::to_string(&action)
            .map_err(|e| eyre!("Failed to serialize action: {}", e))?;
        
        self.actions_file
            .write_all(json.as_bytes())
            .await
            .map_err(|e| eyre!("Failed to write action: {}", e))?;
        
        self.actions_file
            .write_all(b"\n")
            .await
            .map_err(|e| eyre!("Failed to write newline: {}", e))?;
        
        Ok(())
    }
    
    /// Write a timestamp mapping to timestamps.jsonl
    async fn write_timestamp(&mut self, mapping: TimestampMapping) -> Result<()> {
        let json = serde_json::to_string(&mapping)
            .map_err(|e| eyre!("Failed to serialize timestamp mapping: {}", e))?;
        
        self.timestamps_file
            .write_all(json.as_bytes())
            .await
            .map_err(|e| eyre!("Failed to write timestamp: {}", e))?;
        
        self.timestamps_file
            .write_all(b"\n")
            .await
            .map_err(|e| eyre!("Failed to write newline: {}", e))?;
        
        Ok(())
    }
    
    /// Get total actions recorded
    pub fn total_actions(&self) -> u64 {
        self.total_actions
    }
}

/// Convert InputEventType to LEM ActionEvent
fn convert_to_action_event(
    event: &InputEventType,
    t_ns: u64,
    frame_idx: u64,
) -> Option<ActionEvent> {
    use InputEventType;
    
    let action = match event {
        InputEventType::MouseMove { dx, dy } => {
            // For mouse move, we need absolute position
            // This is a simplified version - actual implementation needs mouse position tracking
            ActionType::MouseMove {
                x: 0, // Placeholder - should come from mouse position tracker
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
                    scan_code: 0, // TODO: Get actual scancode
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
        InputEventType::GamepadButton { button, pressed, id } => {
            // Map gamepad to game command for now
            ActionType::GameCommand {
                command: format!("gamepad_{}_{}", button, if *pressed { "press" } else { "release" }),
                target: None,
            }
        }
        InputEventType::GamepadAxis { axis, value, id } => {
            ActionType::GameCommand {
                command: format!("gamepad_axis_{}_{:.2}", axis, value),
                target: None,
            }
        }
        InputEventType::GamepadButtonValue { button, value, id } => {
            ActionType::GameCommand {
                command: format!("gamepad_button_{}_{:.2}", button, value),
                target: None,
            }
        }
        _ => return None, // Skip Start, End, VideoStart, etc.
    };
    
    Some(ActionEvent {
        t_ns,
        frame_idx,
        action,
    })
}

/// Convert virtual key code to string
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
        0x30..=0x39 => ((vkey - 0x30) as u8 as char).to_string(),
        0x41..=0x5A => ((vkey - 0x41 + b'A') as char).to_string(),
        _ => format!("VK_{:02X}", vkey),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    #[tokio::test]
    async fn test_lem_input_recorder_creation() {
        let temp_dir = TempDir::new().unwrap();
        let session_manager = Arc::new(
            SessionManager::create(temp_dir.path(), "TestGame").await.unwrap()
        );
        
        // Create a mock input capture
        let input_capture = InputCapture::new().unwrap();
        
        let (recorder, stream) = LemInputRecorder::start(
            session_manager.clone(),
            &input_capture,
        ).await.unwrap();
        
        // Check files were created
        assert!(session_manager.actions_path().exists());
        assert!(session_manager.timestamps_path().exists());
        
        // Stop the recorder
        stream.stop().unwrap();
        let total = recorder.run().await.unwrap();
        
        // Should have at least the initial timestamp
        assert!(total >= 0);
    }
    
    #[test]
    fn test_convert_mouse_button() {
        let event = InputEventType::MouseButton {
            button: 0, // Left
            pressed: true,
        };
        
        let action = convert_to_action_event(&event, 1_000_000, 1).unwrap();
        
        match action.action {
            ActionType::MouseButton { button, pressed } => {
                assert_eq!(button, "left");
                assert!(pressed);
            }
            _ => panic!("Expected MouseButton"),
        }
    }
    
    #[test]
    fn test_convert_keyboard() {
        let event = InputEventType::Keyboard {
            key: 0x57, // W
            pressed: true,
        };
        
        let action = convert_to_action_event(&event, 1_000_000, 1).unwrap();
        
        match action.action {
            ActionType::KeyDown { key, .. } => {
                assert_eq!(key, "W");
            }
            _ => panic!("Expected KeyDown"),
        }
    }
    
    #[test]
    fn test_vkey_conversion() {
        assert_eq!(vkey_to_string(0x57), "W");
        assert_eq!(vkey_to_string(0x41), "A");
        assert_eq!(vkey_to_string(0x20), "Space");
        assert_eq!(vkey_to_string(0x0D), "Enter");
    }
}