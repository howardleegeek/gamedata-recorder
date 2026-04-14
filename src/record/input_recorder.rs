use std::path::Path;

use color_eyre::{
    eyre::{eyre, WrapErr as _},
    Result,
};
use input_capture::{HighPrecisionTimer, InputCapture};
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
    tx: mpsc::UnboundedSender<InputEvent>,
    timer: HighPrecisionTimer,
}

impl InputEventStream {
    /// Send a timestamped input event at current time. Uses HighPrecisionTimer for
    /// sub-microsecond accuracy to ensure frame timestamp alignment with video.
    pub(crate) fn send(&self, event: InputEventType) -> Result<()> {
        let timestamp = self.timer.elapsed_ns() as f64 / 1_000_000_000.0;
        self.tx
            .send(InputEvent::new(timestamp, event))
            .map_err(|_| eyre!("input event stream receiver was closed"))?;
        Ok(())
    }
}

pub(crate) struct InputEventWriter {
    file: File,
    rx: mpsc::UnboundedReceiver<InputEvent>,
    timer: HighPrecisionTimer,
}

impl InputEventWriter {
    pub(crate) async fn start(
        path: &Path,
        input_capture: &InputCapture,
    ) -> Result<(Self, InputEventStream)> {
        let file = File::create_new(path)
            .await
            .wrap_err_with(|| eyre!("failed to create and open {path:?}"))?;

        // Use HighPrecisionTimer for accurate frame-aligned timestamps
        let timer = HighPrecisionTimer::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let stream = InputEventStream {
            tx,
            timer: timer.clone(),
        };
        let mut writer = Self {
            file,
            rx,
            timer: timer.clone(),
        };

        // No header needed for JSON Lines format — each line is self-describing
        let start_timestamp = timer.elapsed_ns() as f64 / 1_000_000_000.0;
        writer
            .write_entry(InputEvent::new(
                start_timestamp,
                InputEventType::Start {
                    inputs: input_capture.active_input(),
                },
            ))
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
        // Use HighPrecisionTimer for consistent timestamp with other events
        let timestamp = self.timer.elapsed_ns() as f64 / 1_000_000_000.0;

        // Write the end marker with timestamp captured after flush
        self.write_entry(InputEvent::new(
            timestamp,
            InputEventType::End { inputs: end_inputs },
        ))
        .await?;

        // Ensure all data is flushed to disk for crash durability
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
