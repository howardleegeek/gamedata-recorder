use std::path::Path;

use color_eyre::{
    eyre::{eyre, WrapErr as _},
    Result,
};
use input_capture::InputCapture;
use serde::Serialize;
use tokio::{fs::File, io::AsyncWriteExt as _, sync::mpsc};

use crate::output_types::{InputEvent, InputEventType};

/// Maximum buffered input events before backpressure kicks in.
/// ~10k events = ~10 seconds at 1000Hz polling with some headroom.
/// Prevents unbounded memory growth during long recording sessions.
const INPUT_CHANNEL_CAPACITY: usize = 10000;

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
    tx: mpsc::Sender<InputEvent>,
}

impl InputEventStream {
    /// Send a timestamped input event at current time. This is the only supported send
    /// since now that we rely on the rx queue to flush outputs to file, we also want this
    /// queue to be populated in chronological order, so arbitrary timestamp writing
    /// shouldn't be supported anyway.
    ///
    /// Uses try_send with bounded channel to prevent unbounded memory growth.
    /// If the channel is full, the oldest events are dropped to maintain chronological
    /// order while ensuring the recording remains stable.
    pub(crate) fn send(&self, event: InputEventType) -> Result<()> {
        match self.tx.try_send(InputEvent::new_at_now(event)) {
            Ok(_) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Channel is full - drop the event to prevent memory exhaustion.
                // This maintains recording stability over completeness.
                tracing::warn!("Input event channel full, dropping event to prevent memory growth");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(eyre!("input event stream receiver was closed"))
            }
        }
    }
}

pub(crate) struct InputEventWriter {
    file: File,
    rx: mpsc::Receiver<InputEvent>,
}

impl InputEventWriter {
    pub(crate) async fn start(
        path: &Path,
        input_capture: &InputCapture,
    ) -> Result<(Self, InputEventStream)> {
        let file = File::create_new(path)
            .await
            .wrap_err_with(|| eyre!("failed to create and open {path:?}"))?;

        let (tx, rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
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
        // Capture inputs before flush to get most accurate state
        let end_inputs = input_capture.active_input();

        // Flush any remaining events first to ensure proper ordering
        self.flush().await?;

        // Capture timestamp AFTER flush to eliminate drift between
        // last input event and End marker (metadata serialization gap fix)
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or_else(|_| {
                tracing::warn!("System time is before UNIX epoch, using 0");
                0.0
            });

        // Write the end marker with timestamp captured after flush
        self.write_entry(InputEvent::new(
            timestamp,
            InputEventType::End { inputs: end_inputs },
        ))
        .await?;

        // Ensure data is physically written to disk for crash durability
        self.file
            .sync_all()
            .await
            .wrap_err("failed to sync input file to disk")
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
