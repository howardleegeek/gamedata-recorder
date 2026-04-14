use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, AtomicBool, Mutex, RwLock, atomic::Ordering},
    time::Duration,
};

use tokio::sync::mpsc;

use crate::{Event, PressState};

// Copied from gilrs 0.11 constants; I want to ensure stable identifiers for these
// to ensure we don't have to change them if gilrs changes them.
pub const BTN_UNKNOWN: u16 = 0;

pub const BTN_SOUTH: u16 = 1;
pub const BTN_EAST: u16 = 2;
pub const BTN_C: u16 = 3;
pub const BTN_NORTH: u16 = 4;
pub const BTN_WEST: u16 = 5;
pub const BTN_Z: u16 = 6;
pub const BTN_LT: u16 = 7;
pub const BTN_RT: u16 = 8;
pub const BTN_LT2: u16 = 9;
pub const BTN_RT2: u16 = 10;
pub const BTN_SELECT: u16 = 11;
pub const BTN_START: u16 = 12;
pub const BTN_MODE: u16 = 13;
pub const BTN_LTHUMB: u16 = 14;
pub const BTN_RTHUMB: u16 = 15;

pub const BTN_DPAD_UP: u16 = 16;
pub const BTN_DPAD_DOWN: u16 = 17;
pub const BTN_DPAD_LEFT: u16 = 18;
pub const BTN_DPAD_RIGHT: u16 = 19;

pub const AXIS_UNKNOWN: u16 = 0;

pub const AXIS_LSTICKX: u16 = 1;
pub const AXIS_LSTICKY: u16 = 2;
pub const AXIS_LEFTZ: u16 = 3;
pub const AXIS_RSTICKX: u16 = 4;
pub const AXIS_RSTICKY: u16 = 5;
pub const AXIS_RIGHTZ: u16 = 6;
pub const AXIS_DPADX: u16 = 7;
pub const AXIS_DPADY: u16 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GamepadId {
    XInput(usize),
    WGI(usize),
}
impl std::fmt::Display for GamepadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GamepadId::XInput(id) => write!(f, "XInput:{}", id),
            GamepadId::WGI(id) => write!(f, "WGI:{}", id),
        }
    }
}
impl std::str::FromStr for GamepadId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(id) = s.strip_prefix("XInput:") {
            return id
                .parse::<usize>()
                .map(GamepadId::XInput)
                .map_err(|e| format!("Invalid XInput id: {e}"));
        }
        if let Some(id) = s.strip_prefix("WGI:") {
            return id
                .parse::<usize>()
                .map(GamepadId::WGI)
                .map_err(|e| format!("Invalid WGI id: {e}"));
        }
        Err(format!("Invalid gamepad id: {s}"))
    }
}
impl serde::Serialize for GamepadId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_string().serialize(serializer)
    }
}
impl<'de> serde::Deserialize<'de> for GamepadId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActiveGamepad {
    pub digital: HashSet<u16>,
    pub analog: HashMap<u16, f32>,
    pub axis: HashMap<u16, f32>,
}

#[derive(Default)]
pub struct ActiveGamepads {
    pub devices: HashMap<GamepadId, ActiveGamepad>,
}
impl ActiveGamepads {
    pub fn get_or_insert(&mut self, id: GamepadId) -> &mut ActiveGamepad {
        self.devices.entry(id).or_insert_with(|| ActiveGamepad {
            digital: HashSet::new(),
            analog: HashMap::new(),
            axis: HashMap::new(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GamepadMetadata {
    pub name: String,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
}

pub struct GamepadThreads {
    _xinput_thread: std::thread::JoinHandle<()>,
    _wgi_thread: std::thread::JoinHandle<()>,
    shutdown: Arc<AtomicBool>,
}

impl GamepadThreads {
    /// Signal the gamepad capture threads to shut down gracefully.
    /// This sets an atomic flag that the threads check between event polling.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

/// Sanitize gamepad name to ensure it's never empty.
/// Empty names can cause deduplication issues and aren't useful for debugging.
fn sanitize_gamepad_name(name: &str, id: usize) -> String {
    if name.trim().is_empty() {
        format!("Unknown Gamepad {}", id)
    } else {
        name.to_string()
    }
}

pub fn initialize_thread(
    input_tx: mpsc::Sender<Event>,
    active_gamepads: Arc<Mutex<ActiveGamepads>>,
    gamepads: Arc<RwLock<HashMap<GamepadId, GamepadMetadata>>>,
) -> GamepadThreads {
    let already_captured_by_xinput = Arc::new(RwLock::new(HashSet::new()));
    // Shared shutdown flag for clean thread termination
    let shutdown = Arc::new(AtomicBool::new(false));
    // Polling interval for checking shutdown signal (100ms)
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    // We use both the `xinput` and `wgi` versions of gilrs so that we can capture Xbox controllers
    // (which only work with `xinput`) and PS controllers (which only work with `wgi`).
    //
    // We should ostensibly be able to use just `wgi`, but `wgi` only supports all controllers with a focused window
    // associated with the process, which we can't do for our delightful little input recorder.
    // However, it *does* work without a focused window for PS controllers.
    //
    // I love Windows.

    // xinput
    let _xinput_thread = std::thread::spawn({
        let already_captured_by_xinput = already_captured_by_xinput.clone();
        let gamepads = gamepads.clone();
        let input_tx = input_tx.clone();
        let active_gamepads = active_gamepads.clone();
        let shutdown = shutdown.clone();
        move || {
            let mut gilrs = match gilrs_xinput::Gilrs::new() {
                Ok(g) => g,
                Err(e) => {
                    tracing::warn!("Failed to initialize XInput gamepad capture: {e}");
                    return;
                }
            };

            // Examine new events
            while let Some(gilrs_xinput::Event { id, event, .. }) = gilrs.next_event_blocking(None)
            {
                let gamepad = gilrs.gamepad(id);
                let gamepad_name = gamepad.name().to_string();

                // Handle poisoned locks gracefully: if another thread panicked while
                // holding the lock, log the error and break rather than crashing.
                let mut gamepads_guard = match gamepads.write() {
                    Ok(guard) => guard,
                    Err(e) => {
                        tracing::error!("Gamepads RwLock poisoned (xinput), stopping capture: {e}");
                        break;
                    }
                };
                gamepads_guard.insert(
                    GamepadId::XInput(id.into()),
                    GamepadMetadata {
                        name: gamepad_name.clone(),
                        vendor_id: gamepad.vendor_id(),
                        product_id: gamepad.product_id(),
                    },
                );
                drop(gamepads_guard);

                let mut captured_guard = match already_captured_by_xinput.write() {
                    Ok(guard) => guard,
                    Err(e) => {
                        tracing::error!("Captured lock poisoned (xinput), stopping capture: {e}");
                        break;
                    }
                };
                captured_guard.insert(gamepad_name);
                drop(captured_guard);

                let Some(event) = map_event_xinput(GamepadId::XInput(id.into()), event) else {
                    continue;
                };
                let gamepad = gilrs.gamepad(id);
                let gamepad_id = GamepadId::XInput(id.into());
                let gamepad_name = sanitize_gamepad_name(gamepad.name(), id.into());

                // Handle disconnection events to clean up stale state
                if matches!(
                    event,
                    gilrs_xinput::EventType::Disconnected | gilrs_xinput::EventType::Dropped
                ) {
                    let mut active_guard = match active_gamepads.lock() {
                        Ok(guard) => guard,
                        Err(e) => {
                            tracing::error!(
                                "Active gamepads lock poisoned (xinput), stopping capture: {e}"
                            );
                            break;
                        }
                    };
                    active_guard.devices.remove(&gamepad_id);
                    drop(active_guard);

                    // Also clean up stale metadata to prevent memory growth
                    if let Ok(mut gamepads_guard) = gamepads.write() {
                        gamepads_guard.remove(&gamepad_id);
                    }
                    continue;
                }

                // Map the event first to ensure we only mark gamepads as captured
                // if they produce useful events (not Connected/Disconnected/Dropped etc.)
                let Some(event) = map_event_xinput(gamepad_id, event) else {
                    continue;
                };

                // Only update metadata and captured set on first encounter to reduce
                // lock contention - gamepad info rarely changes after connection
                let needs_registration = {
                    let mut gamepads_guard = match gamepads.write() {
                        Ok(guard) => guard,
                        Err(e) => {
                            tracing::error!(
                                "Gamepads RwLock poisoned (xinput), stopping capture: {e}"
                            );
                            break;
                        }
                    };
                    let is_new = !gamepads_guard.contains_key(&gamepad_id);
                    if is_new {
                        gamepads_guard.insert(
                            gamepad_id,
                            GamepadMetadata {
                                name: gamepad_name,
                                vendor_id: gamepad.vendor_id(),
                                product_id: gamepad.product_id(),
                            },
                        );
                    }
                    drop(gamepads_guard);
                    is_new
                };

                if needs_registration {
                    let mut captured_guard = match already_captured_by_xinput.write() {
                        Ok(guard) => guard,
                        Err(e) => {
                            tracing::error!(
                                "Captured lock poisoned (xinput), stopping capture: {e}"
                            );
                            break;
                        }
                    };
                    captured_guard.insert(gamepad_name);
                    drop(captured_guard);
                }

                let mut active_guard = match active_gamepads.lock() {
                    Ok(guard) => guard,
                    Err(e) => {
                        tracing::error!(
                            "Active gamepads lock poisoned (xinput), stopping capture: {e}"
                        );
                        break;
                    }
                };
                update_active_gamepad(&mut active_guard, event);
                drop(active_guard);

                if input_tx.blocking_send(event).is_err() {
                    tracing::warn!("Gamepad input tx closed, stopping gamepad capture");
                    break;
                }
            }
        }
    });

    // wgi
    let _wgi_thread = std::thread::spawn({
        let already_captured_by_xinput = already_captured_by_xinput.clone();
        let shutdown = shutdown.clone();
        move || {
            let mut gilrs = match gilrs_wgi::Gilrs::new() {
                Ok(g) => g,
                Err(e) => {
                    tracing::warn!("Failed to initialize WGI gamepad capture: {e}");
                    return;
                }
            };

            // Examine new events with periodic shutdown checks
            loop {
                if shutdown.load(Ordering::SeqCst) {
                    tracing::debug!("WGI gamepad capture shutting down");
                    break;
                }

                let event_opt = gilrs.next_event_blocking(Some(POLL_INTERVAL));
                let Some(gilrs_wgi::Event { id, event, .. }) = event_opt else {
                    continue;
                };
                let gamepad = gilrs.gamepad(id);
                let gamepad_name = gamepad.name().to_string();

                // Handle disconnection events to clean up stale state
                if matches!(
                    event,
                    gilrs_wgi::EventType::Disconnected | gilrs_wgi::EventType::Dropped
                ) {
                    let mut active_guard = match active_gamepads.lock() {
                        Ok(guard) => guard,
                        Err(e) => {
                            tracing::error!(
                                "Active gamepads lock poisoned (wgi), stopping capture: {e}"
                            );
                            break;
                        }
                    };
                    active_guard.devices.remove(&gamepad_id);
                    drop(active_guard);

                    // Also clean up stale metadata to prevent memory growth
                    if let Ok(mut gamepads_guard) = gamepads.write() {
                        gamepads_guard.remove(&gamepad_id);
                    }
                };
                gamepads_guard.insert(
                    GamepadId::WGI(id.into()),
                    GamepadMetadata {
                        name: gamepad_name.clone(),
                        vendor_id: gamepad.vendor_id(),
                        product_id: gamepad.product_id(),
                    },
                );
                drop(gamepads_guard);

                let captured_guard = match already_captured_by_xinput.read() {
                    Ok(guard) => guard,
                    Err(e) => {
                        tracing::error!("Captured lock poisoned (wgi), stopping capture: {e}");
                        break;
                    }
                };
                let is_captured = captured_guard.contains(&gamepad_name);
                drop(captured_guard);

                if is_captured {
                    continue;
                }

                let gamepad_name = sanitize_gamepad_name(gamepad.name(), id.into());

                // Only update metadata on first encounter to reduce lock contention
                // - gamepad info rarely changes after connection
                let is_new = {
                    let mut gamepads_guard = match gamepads.write() {
                        Ok(guard) => guard,
                        Err(e) => {
                            tracing::error!(
                                "Gamepads RwLock poisoned (wgi), stopping capture: {e}"
                            );
                            break;
                        }
                    };
                    let needs_insert = !gamepads_guard.contains_key(&gamepad_id);
                    if needs_insert {
                        gamepads_guard.insert(
                            gamepad_id,
                            GamepadMetadata {
                                name: gamepad_name.clone(),
                                vendor_id: gamepad.vendor_id(),
                                product_id: gamepad.product_id(),
                            },
                        );
                    }
                    drop(gamepads_guard);
                    needs_insert
                };

                // Skip processing if this gamepad is already captured by xinput
                // or if it's a known gamepad that was previously captured
                if !is_new {
                    let captured_guard = match already_captured_by_xinput.read() {
                        Ok(guard) => guard,
                        Err(e) => {
                            tracing::error!("Captured lock poisoned (wgi), stopping capture: {e}");
                            break;
                        }
                    };
                    let is_captured = captured_guard.contains(&gamepad_name);
                    drop(captured_guard);

                    if is_captured {
                        continue;
                    }
                }

                let Some(event) = map_event_wgi(gamepad_id, event) else {
                    continue;
                };

                let mut active_guard = match active_gamepads.lock() {
                    Ok(guard) => guard,
                    Err(e) => {
                        tracing::error!(
                            "Active gamepads lock poisoned (wgi), stopping capture: {e}"
                        );
                        break;
                    }
                };
                update_active_gamepad(&mut active_guard, event);
                drop(active_guard);

                if input_tx.blocking_send(event).is_err() {
                    tracing::warn!("Gamepad input tx closed, stopping gamepad capture");
                    break;
                }
            }
        }
    });

    fn update_active_gamepad(active_gamepads: &mut ActiveGamepads, event: Event) {
        match event {
            Event::GamepadButtonPress {
                key,
                press_state,
                id,
            } => {
                let active_gamepad = active_gamepads.get_or_insert(id);
                if press_state == PressState::Pressed {
                    active_gamepad.digital.insert(key);
                } else {
                    active_gamepad.digital.remove(&key);
                    active_gamepad.analog.remove(&key);
                }
            }
            Event::GamepadButtonChange { key, value, id } => {
                active_gamepads.get_or_insert(id).analog.insert(key, value);
            }
            Event::GamepadAxisChange { axis, value, id } => {
                active_gamepads.get_or_insert(id).axis.insert(axis, value);
            }
            _ => {}
        }
    }

    GamepadThreads {
        _xinput_thread,
        _wgi_thread,
        shutdown,
    }
}

macro_rules! generate_map_functions {
    ($gilrs:ident, $map_event:ident, $map_button:ident, $map_axis:ident) => {
        fn $map_event(gamepad_id: GamepadId, event: $gilrs::EventType) -> Option<Event> {
            use $gilrs::EventType;
            match event {
                EventType::ButtonPressed(button, _) => Some(Event::GamepadButtonPress {
                    id: gamepad_id,
                    key: $map_button(button),
                    press_state: PressState::Pressed,
                }),
                EventType::ButtonReleased(button, _) => Some(Event::GamepadButtonPress {
                    id: gamepad_id,
                    key: $map_button(button),
                    press_state: PressState::Released,
                }),
                EventType::ButtonChanged(button, value, _) => Some(Event::GamepadButtonChange {
                    id: gamepad_id,
                    key: $map_button(button),
                    value,
                }),
                EventType::AxisChanged(axis, value, _) => Some(Event::GamepadAxisChange {
                    id: gamepad_id,
                    axis: $map_axis(axis),
                    value,
                }),
                _ => None,
            }
        }

        fn $map_button(button: $gilrs::Button) -> u16 {
            use $gilrs::Button;
            match button {
                Button::South => BTN_SOUTH,
                Button::East => BTN_EAST,
                Button::North => BTN_NORTH,
                Button::West => BTN_WEST,
                Button::C => BTN_C,
                Button::Z => BTN_Z,
                Button::LeftTrigger => BTN_LT,
                Button::LeftTrigger2 => BTN_LT2,
                Button::RightTrigger => BTN_RT,
                Button::RightTrigger2 => BTN_RT2,
                Button::Select => BTN_SELECT,
                Button::Start => BTN_START,
                Button::Mode => BTN_MODE,
                Button::LeftThumb => BTN_LTHUMB,
                Button::RightThumb => BTN_RTHUMB,
                Button::DPadUp => BTN_DPAD_UP,
                Button::DPadDown => BTN_DPAD_DOWN,
                Button::DPadLeft => BTN_DPAD_LEFT,
                Button::DPadRight => BTN_DPAD_RIGHT,
                Button::Unknown => BTN_UNKNOWN,
            }
        }

        fn $map_axis(axis: $gilrs::Axis) -> u16 {
            use $gilrs::Axis;
            match axis {
                Axis::LeftStickX => AXIS_LSTICKX,
                Axis::LeftStickY => AXIS_LSTICKY,
                Axis::LeftZ => AXIS_LEFTZ,
                Axis::RightStickX => AXIS_RSTICKX,
                Axis::RightStickY => AXIS_RSTICKY,
                Axis::RightZ => AXIS_RIGHTZ,
                Axis::DPadX => AXIS_DPADX,
                Axis::DPadY => AXIS_DPADY,
                Axis::Unknown => AXIS_UNKNOWN,
            }
        }
    };
}

generate_map_functions!(
    gilrs_xinput,
    map_event_xinput,
    map_button_xinput,
    map_axis_xinput
);
generate_map_functions!(gilrs_wgi, map_event_wgi, map_button_wgi, map_axis_wgi);
