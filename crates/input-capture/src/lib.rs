use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
};

use color_eyre::Result;
use tokio::sync::mpsc;

mod kbm_capture;
use kbm_capture::KbmCapture;

mod gamepad_capture;
pub use gamepad_capture::{ActiveGamepad, GamepadId, GamepadMetadata};

pub mod action_scaffold;
pub mod timestamp;
pub mod trajectory;
pub mod vkey_names;

#[derive(Debug, Clone, Copy)]
pub enum Event {
    /// Relative mouse movement (x, y)
    MouseMove([i32; 2]),
    /// Mouse button press or release
    MousePress { key: u16, press_state: PressState },
    /// Mouse scroll wheel movement
    /// Negative values indicate scrolling down, positive values indicate scrolling up.
    MouseScroll { scroll_amount: i16 },
    /// Keyboard key press or release
    KeyPress { key: u16, press_state: PressState },
    /// Gamepad button press or release
    GamepadButtonPress {
        key: u16,
        press_state: PressState,
        id: GamepadId,
    },
    /// Gamepad button value change (e.g. analogue buttons like triggers)
    GamepadButtonChange { key: u16, value: f32, id: GamepadId },
    /// Gamepad axis value change
    GamepadAxisChange {
        axis: u16,
        value: f32,
        id: GamepadId,
    },
}
impl Event {
    /// Slightly unintuitive, but None being returned does not mean key was not pressed,
    /// just means that another event that is not exactly a key being pressed was recorded.
    /// e.g. unpressed key, mouse movement, etc.
    pub fn key_press_keycode(&self) -> Option<u16> {
        match self {
            Event::KeyPress {
                key,
                press_state: PressState::Pressed,
            } => Some(*key),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActiveInput {
    pub keyboard: HashSet<u16>,
    pub mouse: HashSet<u16>,
    pub gamepads: HashMap<GamepadId, ActiveGamepad>,
}

pub struct InputCapture {
    _raw_input_thread: std::thread::JoinHandle<()>,
    _gamepad_threads: gamepad_capture::GamepadThreads,
    active_keys: Arc<Mutex<kbm_capture::ActiveKeys>>,
    active_gamepad: Arc<Mutex<gamepad_capture::ActiveGamepads>>,
    gamepads: Arc<RwLock<HashMap<GamepadId, GamepadMetadata>>>,
}
impl InputCapture {
    /// Channel capacity for input events. 1000 prevents dropped events during
    /// high-frequency gaming input (e.g., 1000Hz mouse polling with rapid keypresses).
    const INPUT_CHANNEL_CAPACITY: usize = 1000;

    pub fn new() -> Result<(Self, mpsc::Receiver<Event>)> {
        tracing::debug!("InputCapture::new() called");
        let (input_tx, input_rx) = mpsc::channel(Self::INPUT_CHANNEL_CAPACITY);

        tracing::debug!("Spawning raw input thread for keyboard/mouse capture");
        let active_keys = Arc::new(Mutex::new(kbm_capture::ActiveKeys::default()));
        let _raw_input_thread = std::thread::spawn({
            let input_tx = input_tx.clone();
            let active_keys = active_keys.clone();
            move || {
                KbmCapture::initialize(active_keys)
                    .expect("failed to initialize raw input")
                    .run_queue(move |event| {
                        if input_tx.blocking_send(event).is_err() {
                            tracing::warn!("Keyboard input tx closed, stopping keyboard capture");
                            return false;
                        }
                        true
                    })
                    .expect("failed to run windows message queue");
            }
        });

        tracing::debug!("Initializing gamepad capture threads");
        let active_gamepad = Arc::new(Mutex::new(gamepad_capture::ActiveGamepads::default()));
        let gamepads = Arc::new(RwLock::new(HashMap::new()));
        let _gamepad_threads =
            gamepad_capture::initialize_thread(input_tx, active_gamepad.clone(), gamepads.clone());
        tracing::debug!("InputCapture::new() complete");

        Ok((
            Self {
                _raw_input_thread,
                _gamepad_threads,
                active_keys,
                active_gamepad,
                gamepads,
            },
            input_rx,
        ))
    }

    pub fn active_input(&self) -> ActiveInput {
        // Handle poisoned locks gracefully: if another thread panicked while holding
        // the lock, log the error and return default/empty input state rather than crashing.
        let active_keys = match self.active_keys.lock() {
            Ok(guard) => guard,
            Err(e) => {
                tracing::error!(
                    "ActiveKeys mutex poisoned, returning empty keyboard/mouse state: {e}"
                );
                return ActiveInput::default();
            }
        };
        let active_gamepad = match self.active_gamepad.lock() {
            Ok(guard) => guard,
            Err(e) => {
                tracing::error!(
                    "ActiveGamepads mutex poisoned, returning empty gamepad state: {e}"
                );
                return ActiveInput {
                    keyboard: active_keys.keyboard.clone(),
                    mouse: active_keys.mouse.clone(),
                    gamepads: HashMap::new(),
                };
            }
        };
        ActiveInput {
            keyboard: active_keys.keyboard.clone(),
            mouse: active_keys.mouse.clone(),
            gamepads: active_gamepad.devices.clone(),
        }
    }

    pub fn gamepads(&self) -> HashMap<GamepadId, GamepadMetadata> {
        // Handle poisoned locks gracefully: return empty map instead of panicking
        match self.gamepads.read() {
            Ok(guard) => guard.clone(),
            Err(e) => {
                tracing::error!("Gamepads RwLock poisoned, returning empty gamepad metadata: {e}");
                HashMap::new()
            }
        }
    }
}
