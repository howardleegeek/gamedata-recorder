use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Maximum queued input events before dropping oldest.
/// At 1000 Hz mouse polling, 16384 events ≈ 16 seconds of buffer.
/// This prevents unbounded memory growth that caused OOM on 16GB systems
/// when recording VRAM-heavy games like GTA V Enhanced.
const INPUT_CHANNEL_CAPACITY: usize = 16_384;

/// Global counter for dropped input events due to channel full.
/// Shared across all input event streams to track total drops.
static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);

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
    pub(crate) fn send(&self, event: InputEventType) -> Result<()> {
        // Use try_send to avoid blocking the input capture thread.
        // If the channel is full, we drop the event — better to lose an input event
        // than to grow memory unboundedly and crash the game.
        match self.tx.try_send(InputEvent::new_at_now(event)) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Channel full — drop event to prevent OOM. This is acceptable:
                // a few lost input events won't meaningfully degrade the recording.
                // Track dropped events and log periodically if significant.
                let count = DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
                if count > 0 && count % 100 == 0 {
                    tracing::warn!(
                        "Dropped {} input events due to channel full (system under heavy input load)",
                        count + 1
                    );
                }
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
        .await
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
