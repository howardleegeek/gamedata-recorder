//! LEM Format Input Recorder
//!
//! Records input events in LEM format to actions.jsonl and timestamps.jsonl

use std::{path::Path, sync::Arc};

use color_eyre::{Result, eyre::eyre};
use tokio::{fs::File, io::AsyncWriteExt, sync::mpsc};

use crate::{
    output_types::{
        InputEventType,
        lem_types::{ActionEvent, ActionType, TimestampMapping},
    },
    record::session_manager::SessionManager,
};

/// Maximum queued LEM input commands before dropping.
const LEM_CHANNEL_CAPACITY: usize = 16_384;

/// Stream for sending timestamped input events
#[derive(Clone)]
pub struct LemInputStream {
    tx: mpsc::Sender<InputCommand>,
}

impl LemInputStream {
    /// Send an input event
    pub fn send_event(&self, event: InputEventType) -> Result<()> {
        match self.tx.try_send(InputCommand::Event(event)) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::trace!("LEM input channel full, dropping event");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(eyre!("Input stream receiver closed"))
            }
        }
    }

    /// Send a timestamp mapping
    pub fn send_timestamp(&self, mapping: TimestampMapping) -> Result<()> {
        match self.tx.try_send(InputCommand::Timestamp(mapping)) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::trace!("LEM timestamp channel full, dropping timestamp");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(eyre!("Input stream receiver closed"))
            }
        }
    }

    /// Signal to stop recording — never drop this
    pub fn stop(&self) -> Result<()> {
        // Stop commands must not be dropped — use blocking send
        self.tx
            .try_send(InputCommand::Stop)
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
    rx: mpsc::Receiver<InputCommand>,
    total_actions: u64,
}

impl LemInputRecorder {
    /// Start a new LEM input recording session
    pub async fn start(session_manager: Arc<SessionManager>) -> Result<(Self, LemInputStream)> {
        let actions_path = session_manager.actions_path();
        let timestamps_path = session_manager.timestamps_path();

        let actions_file = File::create_new(&actions_path).await.map_err(|e| {
            eyre!(
                "Failed to create actions file at {}: {}",
                actions_path.display(),
                e
            )
        })?;

        let timestamps_file = File::create_new(&timestamps_path).await.map_err(|e| {
            eyre!(
                "Failed to create timestamps file at {}: {}",
                timestamps_path.display(),
                e
            )
        })?;

        let (tx, rx) = mpsc::channel(LEM_CHANNEL_CAPACITY);
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

    /// Main recording loop
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
        InputEventType::MouseMove { dx, dy } => ActionType::MouseMove {
            x: 0,
            y: 0,
            delta_xy: [*dx, *dy],
        },
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
                    scan_code: 0,
                }
            } else {
                ActionType::KeyUp {
                    key: key_str,
                    scan_code: 0,
                }
            }
        }
        InputEventType::Scroll { amount } => ActionType::MouseWheel {
            direction: if *amount > 0 { "up" } else { "down" }.to_string(),
            amount: amount.abs(),
        },
        _ => return None,
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
        0x30..=0x39 => char::from_digit((vkey - 0x30) as u32, 10)
            .unwrap()
            .to_string(),
        0x41..=0x5A => char::from_u32(vkey as u32 - 0x41 + b'A' as u32)
            .unwrap()
            .to_string(),
        _ => format!("VK_{:02X}", vkey),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_lem_input_recorder() {
        let temp_dir = TempDir::new().unwrap();
        let session_manager = Arc::new(
            SessionManager::create(temp_dir.path(), "TestGame")
                .await
                .unwrap(),
        );

        let (recorder, stream) = LemInputRecorder::start(session_manager.clone())
            .await
            .unwrap();

        stream
            .send_event(InputEventType::MouseMove { dx: 10, dy: 5 })
            .unwrap();
        stream
            .send_event(InputEventType::MouseButton {
                button: 0,
                pressed: true,
            })
            .unwrap();

        stream.stop().unwrap();
        let total = recorder.run().await.unwrap();

        assert_eq!(total, 2);

        let actions_content = tokio::fs::read_to_string(session_manager.actions_path())
            .await
            .unwrap();
        assert!(!actions_content.is_empty());
    }
}
