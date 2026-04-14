use std::path::Path;

use color_eyre::{
    Result,
    eyre::{WrapErr as _, eyre},
};
use input_capture::InputCapture;
use serde::Serialize;
use tokio::{fs::File, io::AsyncWriteExt as _, sync::mpsc};

use crate::output_types::{InputEvent, InputEventType};

/// JSON-serializable input event for buyer spec compliance.
/// Each event is written as a JSON Lines entry (one JSON object per line).
#[derive(Serialize)]
struct JsonInputEvent {
    timestamp: f64,
    event_type: &'static str,
    event_args: serde_json::Value,
}

impl From<&InputEvent> for JsonInputEvent {
    fn from(event: &InputEvent) -> Self {
        Self {
            timestamp: event.timestamp,
            event_type: event.event.id(),
            event_args: event.event.json_args(),
        }
    }
}

/// Stream for sending timestamped input events to the writer
#[derive(Clone)]
pub(crate) struct InputEventStream {
    tx: mpsc::UnboundedSender<InputEvent>,
}

impl InputEventStream {
    /// Send a timestamped input event at current time. This is the only supported send
    /// since now that we rely on the rx queue to flush outputs to file, we also want this
    /// queue to be populated in chronological order, so arbitrary timestamp writing
    /// shouldn't be supported anyway.
    pub(crate) fn send(&self, event: InputEventType) -> Result<()> {
        self.tx
            .send(InputEvent::new_at_now(event))
            .map_err(|_| eyre!("input event stream receiver was closed"))?;
        Ok(())
    }
}

pub(crate) struct InputEventWriter {
    file: File,
    rx: mpsc::UnboundedReceiver<InputEvent>,
}

impl InputEventWriter {
    pub(crate) async fn start(
        path: &Path,
        input_capture: &InputCapture,
    ) -> Result<(Self, InputEventStream)> {
        let file = File::create_new(path)
            .await
            .wrap_err_with(|| eyre!("failed to create and open {path:?}"))?;

        let (tx, rx) = mpsc::unbounded_channel();
        let stream = InputEventStream { tx };
        let mut writer = Self { file, rx };

        // No header needed for JSON Lines format — each line is self-describing
        writer
            .write_entry(InputEvent::new_at_now(InputEventType::Start {
                inputs: input_capture.active_input(),
            }))
            .await?;

        Ok((writer, stream))
    }

    /// Flush all pending events from the channel and write them to file
    pub(crate) async fn flush(&mut self) -> Result<()> {
        while let Ok(event) = self.rx.try_recv() {
            self.write_entry(event).await?;
        }
        Ok(())
    }

    pub(crate) async fn stop(mut self, input_capture: &InputCapture) -> Result<()> {
        // Most accurate possible timestamp of exactly when the stop input recording was called
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or_else(|_| {
                tracing::warn!("System time is before UNIX epoch, using 0");
                0.0
            });

        // Flush any remaining events
        self.flush().await?;

        // Write the end marker
        self.write_entry(InputEvent::new(
            timestamp,
            InputEventType::End {
                inputs: input_capture.active_input(),
            },
        ))
        .await?;

        // Ensure all data is flushed to disk to prevent data loss on crash
        self.file
            .sync_all()
            .await
            .wrap_err("failed to sync input events file to disk")
    }

    async fn write_entry(&mut self, event: InputEvent) -> Result<()> {
        // JSON Lines format: one JSON object per line (buyer spec compliant)
        let json_event = JsonInputEvent::from(&event);
        let mut line = serde_json::to_string(&json_event)
            .wrap_err("failed to serialize input event to JSON")?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .await
            .wrap_err("failed to save entry to inputs file")
    }
}
