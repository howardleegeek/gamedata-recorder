use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    time::Duration,
};

use constants::FPS;
use input_capture::{Event, GamepadId, PressState};

#[derive(Default)]
pub(crate) struct EventDebouncer {
    keyboard: KeyDebouncer<u16>,
    mouse_key: KeyDebouncer<u16>,
    gamepad_button: KeyDebouncer<(GamepadId, u16)>,
    gamepad_button_value: AnalogDebouncer<(GamepadId, u16)>,
    gamepad_axis: AnalogDebouncer<(GamepadId, u16)>,
}

impl EventDebouncer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns true if the event should be processed, or false if it should be ignored.
    pub(crate) fn debounce(&mut self, e: Event) -> bool {
        match e {
            Event::MousePress { key, press_state } => self.mouse_key.debounce(key, press_state),
            Event::KeyPress { key, press_state } => self.keyboard.debounce(key, press_state),
            Event::GamepadButtonPress {
                key,
                press_state,
                id,
            } => self.gamepad_button.debounce((id, key), press_state),
            Event::GamepadButtonChange { key, id, .. } => {
                self.gamepad_button_value.debounce((id, key))
            }
            Event::GamepadAxisChange { axis: key, id, .. } => self.gamepad_axis.debounce((id, key)),
            Event::MouseMove(_) | Event::MouseScroll { .. } => true,
        }
    }
}

struct KeyDebouncer<K: Eq + Hash> {
    pressed_keys: HashSet<K>,
}

impl<K: Eq + Hash> Default for KeyDebouncer<K> {
    fn default() -> Self {
        Self {
            pressed_keys: Default::default(),
        }
    }
}
impl<K: Eq + Hash> KeyDebouncer<K> {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns true if the key event should be processed, or false if it should be ignored.
    pub(crate) fn debounce(&mut self, key: K, press_state: PressState) -> bool {
        match press_state {
            PressState::Pressed => self.pressed_keys.insert(key),
            PressState::Released => {
                self.pressed_keys.remove(&key);
                true
            }
        }
    }
}

/// Maximum time between analog input samples, computed at compile time to avoid
/// recalculating on every debounce call. Uses integer math to avoid float operations.
const MAX_ANALOGUE_SAMPLING_MICROSECONDS: u64 = 1_000_000 / ((FPS as u64) * 2);

/// Cleanup interval for old entries to prevent unbounded HashMap growth.
/// Every 30 seconds, remove entries older than 60 seconds.
const CLEANUP_INTERVAL_MICROSECONDS: u64 = 30_000_000; // 30 seconds
const STALE_ENTRY_MICROSECONDS: u64 = 60_000_000; // 60 seconds

struct AnalogDebouncer<K: Eq + Hash> {
    last_change: HashMap<K, std::time::Instant>,
    last_cleanup: std::time::Instant,
}

impl<K: Eq + Hash> Default for AnalogDebouncer<K> {
    fn default() -> Self {
        Self {
            last_change: Default::default(),
            last_cleanup: std::time::Instant::now(),
        }
    }
}

impl<K: Eq + Hash> AnalogDebouncer<K> {
    /// Returns whether or not a sufficient amount of time has passed since the last change.
    pub(crate) fn debounce(&mut self, key: K) -> bool {
        let now = std::time::Instant::now();

        // Periodically clean up stale entries to prevent unbounded growth
        self.maybe_cleanup(now);

        use std::collections::hash_map::Entry;
        match self.last_change.entry(key) {
            Entry::Occupied(mut entry) => {
                if now.saturating_duration_since(*entry.get())
                    > Duration::from_micros(MAX_ANALOGUE_SAMPLING_MICROSECONDS)
                {
                    entry.insert(now);
                    true
                } else {
                    false
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(now);
                true
            }
        }
    }

    /// Remove stale entries to prevent unbounded memory growth during long sessions.
    /// Called periodically from the hot path to amortize cleanup cost.
    fn maybe_cleanup(&mut self, now: std::time::Instant) {
        if now.saturating_duration_since(self.last_cleanup)
            > Duration::from_micros(CLEANUP_INTERVAL_MICROSECONDS)
        {
            let stale_threshold = Duration::from_micros(STALE_ENTRY_MICROSECONDS);
            self.last_change.retain(|_, &mut instant| {
                now.saturating_duration_since(instant) <= stale_threshold
            });
            self.last_cleanup = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_debouncer() {
        use PressState::*;

        let mut debouncer = KeyDebouncer::new();

        assert!(debouncer.debounce(65, Pressed));
        assert!(!debouncer.debounce(65, Pressed));
        assert!(!debouncer.debounce(65, Pressed));
        assert!(debouncer.debounce(65, Released));
        assert!(debouncer.debounce(65, Pressed));
        assert!(!debouncer.debounce(65, Pressed));
    }
}
