//! Integration tests for `input_capture::vkey_names`.
//!
//! `vkey_to_name` maps Win32 virtual key codes to human-readable names.
//! These names are part of the buyer wire contract — they appear in
//! `inputs.jsonl` per keypress and are pattern-matched by downstream
//! training pipelines.

#![cfg(target_os = "windows")]

use input_capture::vkey_names::vkey_to_name;

// ---------------------------------------------------------------------------
// Letters
// ---------------------------------------------------------------------------

#[test]
fn letter_keys_map_to_uppercase_ascii() {
    // 0x41..0x5A = 'A'..'Z' per Win32 contract.
    assert_eq!(vkey_to_name(0x41), "A");
    assert_eq!(vkey_to_name(0x42), "B");
    assert_eq!(vkey_to_name(0x57), "W");
    assert_eq!(vkey_to_name(0x53), "S");
    assert_eq!(vkey_to_name(0x44), "D");
    assert_eq!(vkey_to_name(0x5A), "Z");
}

// ---------------------------------------------------------------------------
// Number row
// ---------------------------------------------------------------------------

#[test]
fn number_keys_map_to_digit_chars() {
    // 0x30..0x39 = '0'..'9' per Win32 contract.
    for digit in 0..10u16 {
        let vk = 0x30 + digit;
        let expected = match digit {
            0 => "0",
            1 => "1",
            2 => "2",
            3 => "3",
            4 => "4",
            5 => "5",
            6 => "6",
            7 => "7",
            8 => "8",
            9 => "9",
            _ => unreachable!(),
        };
        assert_eq!(vkey_to_name(vk), expected);
    }
}

// ---------------------------------------------------------------------------
// Numpad
// ---------------------------------------------------------------------------

#[test]
fn numpad_keys_map_to_num_prefix() {
    // 0x60..0x69 = NUM0..NUM9
    assert_eq!(vkey_to_name(0x60), "NUM0");
    assert_eq!(vkey_to_name(0x69), "NUM9");
    assert_eq!(vkey_to_name(0x6A), "NUM*");
    assert_eq!(vkey_to_name(0x6B), "NUM+");
    assert_eq!(vkey_to_name(0x6D), "NUM-");
    assert_eq!(vkey_to_name(0x6F), "NUM/");
}

// ---------------------------------------------------------------------------
// Function keys
// ---------------------------------------------------------------------------

#[test]
fn function_keys_f1_through_f12() {
    assert_eq!(vkey_to_name(0x70), "F1");
    assert_eq!(vkey_to_name(0x71), "F2");
    assert_eq!(vkey_to_name(0x7B), "F12");
}

// ---------------------------------------------------------------------------
// Modifier keys
// ---------------------------------------------------------------------------

#[test]
fn modifier_keys_have_canonical_names() {
    assert_eq!(vkey_to_name(0x10), "SHIFT");
    assert_eq!(vkey_to_name(0x11), "CTRL");
    assert_eq!(vkey_to_name(0x12), "ALT");
    assert_eq!(vkey_to_name(0xA0), "LSHIFT");
    assert_eq!(vkey_to_name(0xA1), "RSHIFT");
    assert_eq!(vkey_to_name(0xA2), "LCTRL");
    assert_eq!(vkey_to_name(0xA3), "RCTRL");
    assert_eq!(vkey_to_name(0xA4), "LALT");
    assert_eq!(vkey_to_name(0xA5), "RALT");
}

#[test]
fn windows_keys() {
    assert_eq!(vkey_to_name(0x5B), "LWIN");
    assert_eq!(vkey_to_name(0x5C), "RWIN");
    assert_eq!(vkey_to_name(0x5D), "APPS");
}

// ---------------------------------------------------------------------------
// Navigation + special keys
// ---------------------------------------------------------------------------

#[test]
fn navigation_keys() {
    assert_eq!(vkey_to_name(0x25), "LEFT");
    assert_eq!(vkey_to_name(0x26), "UP");
    assert_eq!(vkey_to_name(0x27), "RIGHT");
    assert_eq!(vkey_to_name(0x28), "DOWN");
    assert_eq!(vkey_to_name(0x21), "PAGEUP");
    assert_eq!(vkey_to_name(0x22), "PAGEDOWN");
    assert_eq!(vkey_to_name(0x23), "END");
    assert_eq!(vkey_to_name(0x24), "HOME");
    assert_eq!(vkey_to_name(0x2D), "INSERT");
    assert_eq!(vkey_to_name(0x2E), "DELETE");
}

#[test]
fn special_keys() {
    assert_eq!(vkey_to_name(0x08), "BACKSPACE");
    assert_eq!(vkey_to_name(0x09), "TAB");
    assert_eq!(vkey_to_name(0x0D), "ENTER");
    assert_eq!(vkey_to_name(0x13), "PAUSE");
    assert_eq!(vkey_to_name(0x14), "CAPSLOCK");
    assert_eq!(vkey_to_name(0x1B), "ESC");
    assert_eq!(vkey_to_name(0x20), "SPACE");
    assert_eq!(vkey_to_name(0x2C), "PRINTSCREEN");
    assert_eq!(vkey_to_name(0x90), "NUMLOCK");
    assert_eq!(vkey_to_name(0x91), "SCROLLLOCK");
}

// ---------------------------------------------------------------------------
// Punctuation
// ---------------------------------------------------------------------------

#[test]
fn punctuation_keys() {
    assert_eq!(vkey_to_name(0xBA), ";");
    assert_eq!(vkey_to_name(0xBB), "=");
    assert_eq!(vkey_to_name(0xBC), ",");
    assert_eq!(vkey_to_name(0xBD), "-");
    assert_eq!(vkey_to_name(0xBE), ".");
    assert_eq!(vkey_to_name(0xBF), "/");
    assert_eq!(vkey_to_name(0xC0), "`");
    assert_eq!(vkey_to_name(0xDB), "[");
    assert_eq!(vkey_to_name(0xDC), "\\");
    assert_eq!(vkey_to_name(0xDD), "]");
    assert_eq!(vkey_to_name(0xDE), "'");
}

// ---------------------------------------------------------------------------
// Unknown / unmapped
// ---------------------------------------------------------------------------

#[test]
fn unknown_vkey_returns_question_mark() {
    // Unmapped VKs return "?" sentinel. The recorder uses this as a
    // signal to log unhandled keys; we lock in "?" so the log filter
    // doesn't need updating across releases.
    assert_eq!(vkey_to_name(0x00), "?");
    assert_eq!(vkey_to_name(0x01), "?"); // VK_LBUTTON — not a keyboard key
    assert_eq!(vkey_to_name(0xFE), "?");
    assert_eq!(vkey_to_name(0xFF), "?");
}

// ---------------------------------------------------------------------------
// Return type: 'static str
// ---------------------------------------------------------------------------

#[test]
fn returned_string_is_static_lifetime() {
    // vkey_to_name returns &'static str. This is what lets the recorder
    // store action_type.key_name without owned allocation. Smoke test
    // that the return type chain is reachable.
    let name: &'static str = vkey_to_name(0x41);
    assert_eq!(name, "A");
}

// ---------------------------------------------------------------------------
// Sweep every defined VK
// ---------------------------------------------------------------------------

#[test]
fn known_vkeys_never_return_question_mark() {
    // Sweep the explicit `match` arms in vkey_to_name to ensure every
    // mapped entry returns a non-"?" name. A future refactor that
    // accidentally drops a branch would fail this test.
    let known: &[u16] = &[
        0x08, 0x09, 0x0D, 0x10, 0x11, 0x12, 0x13, 0x14, 0x1B, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25,
        0x26, 0x27, 0x28, 0x2C, 0x2D, 0x2E, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38,
        0x39, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E,
        0x4F, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D,
        0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E,
        0x6F, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x90, 0x91,
        0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xC0, 0xDB, 0xDC,
        0xDD, 0xDE,
    ];
    for &vk in known {
        let name = vkey_to_name(vk);
        assert_ne!(name, "?", "expected mapping for VK {vk:#04X}, got '?'");
    }
}
