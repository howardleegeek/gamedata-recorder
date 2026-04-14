//! Standalone input logger CLI.
//! Run: cargo run -p input-capture --bin input_logger
//!
//! Output:
//!   15:51:23.010  KEYDOWN      W  (vk=87 sc=17)
//!   15:51:24.810  MOUSE_MOVE   dx=12 dy=-5
//!   15:51:25.100  MOUSE_BTN    LEFT DOWN
//!
//! With compression:
//!   cargo run -p input-capture --bin input_logger -- --compress --output events.log.zst

use std::io::Write;

use input_capture::{Event, PressState, timestamp::HighPrecisionTimer, vkey_names::vkey_to_name};

#[derive(Debug)]
struct Args {
    compress: bool,
    output: Option<String>,
    level: i32,
}

impl Args {
    fn parse() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let mut compress = false;
        let mut output = None;
        let mut level = 3; // Default zstd compression level

        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--compress" | "-c" => compress = true,
                "--output" | "-o" => {
                    if i + 1 < args.len() {
                        output = Some(args[i + 1].clone());
                        i += 1;
                    }
                }
                "--level" | "-l" => {
                    if i + 1 < args.len() {
                        if let Ok(l) = args[i + 1].parse::<i32>() {
                            level = l.clamp(1, 22);
                        }
                        i += 1;
                    }
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => {}
            }
            i += 1;
        }

        Self {
            compress,
            output,
            level,
        }
    }
}

fn print_help() {
    eprintln!("GameData Input Logger");
    eprintln!();
    eprintln!("Usage: input_logger [OPTIONS]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -c, --compress       Enable zstd compression for output");
    eprintln!("  -o, --output <FILE>  Write to file instead of stdout");
    eprintln!("  -l, --level <1-22>   Zstd compression level (default: 3)");
    eprintln!("  -h, --help           Print this help message");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  input_logger                    # Output to stdout");
    eprintln!("  input_logger -o events.log      # Write to file");
    eprintln!("  input_logger -c -o events.zst     # Compress with zstd");
    eprintln!("  input_logger -c -l 10 -o ev.zst  # High compression");
}

/// Output writer trait for abstraction over stdout and compressed file
trait OutputWriter: Write {
    fn flush_output(&mut self) -> std::io::Result<()>;
}

impl OutputWriter for std::io::StdoutLock<'_> {
    fn flush_output(&mut self) -> std::io::Result<()> {
        self.flush()
    }
}

impl OutputWriter for std::fs::File {
    fn flush_output(&mut self) -> std::io::Result<()> {
        self.flush()
    }
}

#[cfg(feature = "compression")]
struct ZstdWriter {
    encoder: zstd::stream::write::Encoder<'static, std::fs::File>,
}

#[cfg(feature = "compression")]
impl Write for ZstdWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.encoder.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.encoder.flush()
    }
}

#[cfg(feature = "compression")]
impl OutputWriter for ZstdWriter {
    fn flush_output(&mut self) -> std::io::Result<()> {
        self.encoder.flush()
    }
}

fn main() {
    let args = Args::parse();

    eprintln!("GameData Input Logger");
    if args.compress {
        #[cfg(feature = "compression")]
        eprintln!("Compression: enabled (level {})", args.level);
        #[cfg(not(feature = "compression"))]
        {
            eprintln!("Error: compression requested but 'compression' feature not enabled.");
            eprintln!(
                "Rebuild with: cargo run -p input-capture --bin input_logger --features compression"
            );
            std::process::exit(1);
        }
    }
    if let Some(ref path) = args.output {
        eprintln!("Output file: {}", path);
    }
    eprintln!("Capturing keyboard + mouse + gamepad...");
    eprintln!("Press Ctrl+C to stop.\n");

    let timer = HighPrecisionTimer::new();

    let (_capture, mut rx) =
        input_capture::InputCapture::new().expect("Failed to initialize input capture");

    // Block on the tokio channel using a simple runtime
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime for input logger");

    rt.block_on(async {
        // Setup output writer based on arguments
        let _result: Result<(), Box<dyn std::error::Error>> = match &args.output {
            Some(path) => {
                let file = std::fs::File::create(path).expect("Failed to create output file");

                #[cfg(feature = "compression")]
                if args.compress {
                    let encoder = zstd::stream::write::Encoder::new(file, args.level)
                        .expect("Failed to create zstd encoder");
                    let mut writer = ZstdWriter { encoder };
                    run_logger(&timer, &mut rx, &mut writer).await
                } else {
                    let mut writer = file;
                    run_logger(&timer, &mut rx, &mut writer).await
                }

                #[cfg(not(feature = "compression"))]
                {
                    let mut writer = file;
                    run_logger(&timer, &mut rx, &mut writer).await
                }
            }
            None => {
                let stdout = std::io::stdout();
                let mut writer = stdout.lock();
                run_logger(&timer, &mut rx, &mut writer).await
            }
        };
    });
}

async fn run_logger<W: OutputWriter>(
    timer: &HighPrecisionTimer,
    rx: &mut tokio::sync::mpsc::Receiver<Event>,
    out: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    while let Some(event) = rx.recv().await {
        let t = timer.wall_time_str();
        let line = format_event(&t, &event);
        let _ = writeln!(out, "{}", line);

        // Flush periodically to ensure data is written
        // Flush on all input events (keyboard, mouse buttons, scroll) for crash durability
        // CRITICAL: Flush on both press AND release to prevent state inconsistency
        // if a crash occurs between press and release (e.g., press recorded, release lost)
        let should_flush = matches!(
            event,
            Event::KeyPress { .. }
                | Event::MousePress { .. }
                | Event::MouseScroll { .. }
                | Event::GamepadButtonPress { .. }
        );
        if should_flush {
            let _ = out.flush_output();
        }
    }
    Ok(())
}

fn format_event(t: &str, event: &Event) -> String {
    match event {
        Event::KeyPress { key, press_state } => {
            let state = match press_state {
                PressState::Pressed => "KEYDOWN",
                PressState::Released => "KEYUP",
            };
            let name = vkey_to_name(*key);
            format!("{}  {:<12} {}  (vk={})", t, state, name, key)
        }
        Event::MouseMove([dx, dy]) => {
            format!("{}  MOUSE_MOVE   dx={} dy={}", t, dx, dy)
        }
        Event::MousePress { key, press_state } => {
            let state = match press_state {
                PressState::Pressed => "DOWN",
                PressState::Released => "UP",
            };
            let btn = match key {
                1 => "LEFT",
                2 => "RIGHT",
                3 => "MIDDLE",
                4 => "X1",
                5 => "X2",
                _ => "?",
            };
            format!("{}  MOUSE_BTN    {} {}", t, btn, state)
        }
        Event::MouseScroll { scroll_amount } => {
            let dir = if *scroll_amount > 0 { "UP" } else { "DOWN" };
            format!("{}  MOUSE_WHEEL  {} ({})", t, dir, scroll_amount)
        }
        Event::GamepadButtonPress {
            key,
            press_state,
            id,
        } => {
            let state = match press_state {
                PressState::Pressed => "DOWN",
                PressState::Released => "UP",
            };
            format!("{}  PAD_{:?}_BTN  {} {}", t, id, key, state)
        }
        Event::GamepadButtonChange { key, value, id } => {
            format!("{}  PAD_{:?}_VAL  btn={} val={:.2}", t, id, key, value)
        }
        Event::GamepadAxisChange { axis, value, id } => {
            format!("{}  PAD_{:?}_AXIS axis={} val={:.2}", t, id, axis, value)
        }
    }
}
