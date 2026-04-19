use color_eyre::Result;
use input_capture::{ConsentGuard, InputCapture};

pub fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    // Example binary: operator is the developer running this directly, so we
    // construct a granted guard. Real product paths MUST compute the guard
    // from user consent persisted in config — see `src/config.rs` in the
    // host crate.
    let (_input_capture, mut input_rx) = InputCapture::new(&ConsentGuard::granted())?;
    while let Some(event) = input_rx.blocking_recv() {
        tracing::info!(?event, "Received raw input event");
    }

    Ok(())
}
