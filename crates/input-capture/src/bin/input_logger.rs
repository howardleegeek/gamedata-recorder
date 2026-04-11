//! Standalone input logger CLI.
//! Run: cargo run -p input-capture --bin input_logger
//!
//! Output:
//!   15:51:23.010  KEYDOWN      W  (vk=87 sc=17)
//!   15:51:24.810  MOUSE_MOVE   dx=12 dy=-5
//!   15:51:25.100  MOUSE_BTN    LEFT DOWN

use std::io::Write;

use input_capture::{Event, PressState, timestamp::HighPrecisionTimer, vkey_names::vkey_to_name};

fn main() {
    let timer = HighPrecisionTimer::new();

    eprintln!("GameData Input Logger");
    eprintln!("Capturing keyboard + mouse + gamepad...");
    eprintln!("Press Ctrl+C to stop.\n");

    let (_capture, mut rx) =
        input_capture::InputCapture::new().expect("Failed to initialize input capture");

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // Block on the tokio channel using a simple runtime
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        while let Some(event) = rx.recv().await {
            let t = timer.wall_time_str();
            let line = format_event(&t, &event);
            let _ = writeln!(out, "{}", line);
        }
    });
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
