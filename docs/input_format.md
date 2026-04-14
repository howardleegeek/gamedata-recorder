# Input Recording Format

This document describes the format of the `inputs.csv` file produced by GameData Recorder's recording system.

## Overview

The input recording format is a CSV file with JSON-encoded event arguments. Each row represents a single input event.

```csv
timestamp,event_type,event_args
1767633347.5859811,START,"{""inputs"":{""keyboard"":[116],""mouse"":[]}}"
1767633347.708028,VIDEO_START,"[]"
1767633347.7088318,KEYBOARD,"[116,false]"
1767633347.7088337,MOUSE_MOVE,"[0,1]"
```

### Columns

| Column | Type | Description |
|--------|------|-------------|
| `timestamp` | `f64` | UNIX timestamp (seconds since epoch) |
| `event_type` | `string` | The type of event (see [Event Types](#event-types)) |
| `event_args` | `JSON string` | JSON-encoded arguments specific to the event type |

### Timestamps

Timestamps are **UNIX timestamps** (seconds since the Unix epoch, January 1, 1970). They are *not* relative to the start of the recording.

To get relative timestamps, subtract the `VIDEO_START` event's timestamp from all other timestamps.

## Video Alignment

**Important:** When synchronizing inputs with video playback, do **not** use `START`. Instead:

1. **`VIDEO_START`**: Align video playback to this timestamp. This is when video capture actually begins.
2. **`HOOK_START`**: Before this event, video frames will be black. If your video has initial black frames, consider aligning to `HOOK_START` or the first non-black frame.

Use whichever gives you the best result for your use case. `START` is only useful for knowing when the recorder process began, not for video synchronization.

## Event Types

### Recording Lifecycle Events

#### `START`

The very beginning of the recording bundle. This is when the recorder starts, but **not** when the video starts.

**Args:** `{"inputs": <Inputs>}`

The `inputs` object contains the keys/buttons that were held down at the moment recording started. See [Inputs Structure](#inputs-structure) below.

#### `END`

The very end of the recording bundle.

**Args:** `{"inputs": <Inputs>}`

Same structure as `START` - contains the keys/buttons held down when recording ended.

#### Inputs Structure

The `inputs` object in `START` and `END` events has the following structure:

```json
{
  "keyboard": [16, 65],
  "mouse": [1],
  "gamepads": {
    "XInput:0": {
      "digital": [1, 4],
      "analog": {"9": 0.75, "10": 0.5},
      "axis": {"1": -0.5, "2": 0.25}
    }
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `keyboard` | `u16[]` | Virtual keycodes of keyboard keys currently held down |
| `mouse` | `u16[]` | Virtual keycodes of mouse buttons currently held down |
| `gamepads` | `Map<string, GamepadInputs>` | Map of gamepad ID to its input state (omitted if empty) |

**GamepadInputs structure:**

| Field | Type | Description |
|-------|------|-------------|
| `digital` | `u16[]` | Button codes of digital buttons currently held down |
| `analog` | `Map<string, f32>` | Button code to analog value (0.0-1.0) for analog buttons (e.g., triggers) |
| `axis` | `Map<string, f32>` | Axis code to value (-1.0 to 1.0) for analog sticks |

**Gamepad IDs** are strings in the format `"XInput:<n>"` or `"WGI:<n>"` where `<n>` is the device index. XInput is used for Xbox controllers, WGI (Windows.Gaming.Input) for PlayStation controllers.

#### `VIDEO_START`

When the video recording actually begins. There is typically a lag between `START` and `VIDEO_START`.

**Args:** `[]`

**Important:** To synchronize input playback with video, align to `VIDEO_START`, not `START`.

#### `VIDEO_END`

When the video recording ends.

**Args:** `[]`

#### `HOOK_START`

When the recorder successfully hooks into the game and starts receiving actual footage. Before this event, the video will be **black frames**.

**Args:** `[]`

**Tip:** For the most accurate alignment, you may want to use `HOOK_START` as your reference point, especially if your video has initial black frames.

### Input Events

#### `KEYBOARD`

A keyboard key press or release.

**Args:** `[keycode, pressed]`

| Arg | Type | Description |
|-----|------|-------------|
| `keycode` | `u16` | Windows virtual keycode (see [Keycodes](#keycodes)) |
| `pressed` | `bool` | `true` = key down, `false` = key up |

**Example:** `[65, true]` = A key pressed

#### `MOUSE_MOVE`

Relative mouse movement.

**Args:** `[dx, dy]`

| Arg | Type | Description |
|-----|------|-------------|
| `dx` | `i32` | Horizontal movement (positive = right) |
| `dy` | `i32` | Vertical movement (positive = down) |

**Example:** `[10, -5]` = moved 10 pixels right, 5 pixels up

#### `MOUSE_BUTTON`

A mouse button press or release.

**Args:** `[button, pressed]`

| Arg | Type | Description |
|-----|------|-------------|
| `button` | `u16` | Mouse button virtual keycode (see [Keycodes](#keycodes)) |
| `pressed` | `bool` | `true` = button down, `false` = button up |

**Example:** `[1, true]` = left mouse button pressed (VK_LBUTTON)

#### `SCROLL`

Mouse scroll wheel movement.

**Args:** `[amount]`

| Arg | Type | Description |
|-----|------|-------------|
| `amount` | `i16` | Scroll amount (positive = scroll up) |

**Example:** `[120]` = scroll up

### Gamepad Events

Gamepad button and axis codes are defined in [`crates/input-capture/src/gamepad_capture.rs`](../crates/input-capture/src/gamepad_capture.rs).

#### `GAMEPAD_BUTTON`

A gamepad button press or release.

**Args:** `[button, pressed]` or `[button, pressed, gamepad_id]`

| Arg | Type | Description |
|-----|------|-------------|
| `button` | `u16` | Gamepad button code (e.g., `BTN_SOUTH = 1`, `BTN_EAST = 2`) |
| `pressed` | `bool` | `true` = button down, `false` = button up |
| `gamepad_id` | `string` (optional) | Gamepad identifier (e.g., `"XInput:0"`, `"WGI:0"`) |

#### `GAMEPAD_BUTTON_VALUE`

Analog button value change (e.g., triggers).

**Args:** `[button, value]` or `[button, value, gamepad_id]`

| Arg | Type | Description |
|-----|------|-------------|
| `button` | `u16` | Gamepad button code |
| `value` | `f32` | Analog value (typically 0.0 to 1.0) |
| `gamepad_id` | `string` (optional) | Gamepad identifier |

#### `GAMEPAD_AXIS`

Analog stick or axis movement.

**Args:** `[axis, value]` or `[axis, value, gamepad_id]`

| Arg | Type | Description |
|-----|------|-------------|
| `axis` | `u16` | Axis code (e.g., `AXIS_LSTICKX = 1`, `AXIS_LSTICKY = 2`) |
| `value` | `f32` | Axis value (typically -1.0 to 1.0) |
| `gamepad_id` | `string` (optional) | Gamepad identifier |

### Deprecated Events

#### `FOCUS` / `UNFOCUS`

These events are deprecated and should not be used.

#### `UNKNOWN`

Used for backwards compatibility when encountering unrecognized event types. Never produced by new recordings.

## Keycodes

Keyboard events use Windows Virtual Keycodes. See [`src/system/keycode.rs`](../src/system/keycode.rs) for the canonical mapping.

Common keycodes:

| Code (hex) | Code (dec) | Key |
|------------|------------|-----|
| 0x08 | 8 | Backspace |
| 0x09 | 9 | Tab |
| 0x0D | 13 | Enter |
| 0x10 | 16 | Shift |
| 0x11 | 17 | Ctrl |
| 0x12 | 18 | Alt |
| 0x1B | 27 | Esc |
| 0x20 | 32 | Spacebar |
| 0x25-0x28 | 37-40 | Arrow Keys (Left, Up, Right, Down) |
| 0x30-0x39 | 48-57 | 0-9 |
| 0x41-0x5A | 65-90 | A-Z |
| 0x70-0x7B | 112-123 | F1-F12 |

For the complete list, refer to [Microsoft's Virtual Key Codes documentation](https://learn.microsoft.com/en-us/windows/win32/inputdev/virtual-key-codes).

## Caveats

- **Event ordering**: In some edge cases, events may appear in unexpected orders. Robust pipelines should handle or filter these cases.
- **JSON-in-CSV**: The CSV format embeds JSON within quoted strings, which can be awkward to parse. This is a legacy format maintained for backwards compatibility.
