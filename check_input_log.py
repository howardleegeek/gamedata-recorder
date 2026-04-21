"""
check_input_log.py — CI input log validation for gamedata-recorder

Accepts both formats declared in `crates/constants/src/lib.rs`:
  * inputs.jsonl  — current production format (one JSON object per line)
  * inputs.csv    — legacy format (timestamp,event_type,json_args per line)

Format is detected per-line: a line starting with '{' is parsed as JSON,
anything else is parsed as CSV. This lets the script validate both fresh
recordings and older archived ones without a CLI flag.

Checks:
  1. Mouse events recorded (movement and clicks)
  2. Keyboard events recorded
  3. Event timestamps are present and monotonic
  4. Metadata markers (START, END, VIDEO_START, VIDEO_END)

Usage:
  python check_input_log.py <input_log>           # auto-detect format
  python check_input_log.py <input_log> --require-mouse --require-keyboard

Exit codes:
  0 = all checks passed
  1 = one or more checks failed
  2 = file not found or unreadable
"""

import argparse
import json
import sys


def _parse_jsonl_line(line: str):
    """Parse a single JSONL line. Returns a dict or None on failure."""
    obj = json.loads(line)  # may raise
    if not isinstance(obj, dict):
        return None
    try:
        return {
            "timestamp": float(obj["timestamp"]),
            "event_type": str(obj["event_type"]),
            # event_args is already a Python value (list/dict/scalar) after json.loads
            "event_args": obj.get("event_args"),
        }
    except (KeyError, ValueError, TypeError):
        return None


def _parse_csv_line(line: str):
    """Parse a single legacy CSV line. Returns a dict or None on failure."""
    # Format: timestamp,event_type,json_args   (json_args may itself contain commas)
    parts = line.split(",", 2)
    if len(parts) != 3:
        return None
    try:
        timestamp = float(parts[0])
        event_type = parts[1]
        # json_args is a JSON-encoded string (optionally double-quote-wrapped
        # because of CSV escaping); try to decode it, fall back to raw string.
        raw = parts[2].strip()
        if raw.startswith('"') and raw.endswith('"'):
            raw = raw[1:-1].replace('""', '"')
        try:
            event_args = json.loads(raw)
        except (json.JSONDecodeError, ValueError):
            event_args = raw
        return {
            "timestamp": timestamp,
            "event_type": event_type,
            "event_args": event_args,
        }
    except (ValueError, IndexError):
        return None


def read_input_log(log_path: str):
    """Read and parse an input log in JSONL or legacy CSV format."""
    events = []
    n_malformed = 0
    try:
        with open(log_path, "r", encoding="utf-8") as f:
            for line_num, raw_line in enumerate(f, 1):
                line = raw_line.strip()
                if not line:
                    continue
                # Format detection: JSONL lines start with '{', legacy CSV does not.
                if line.startswith("{"):
                    try:
                        parsed = _parse_jsonl_line(line)
                    except json.JSONDecodeError as e:
                        print(f"  WARNING: Line {line_num}: invalid JSON: {e}")
                        parsed = None
                else:
                    parsed = _parse_csv_line(line)

                if parsed is None:
                    print(f"  WARNING: Line {line_num}: malformed, skipping")
                    n_malformed += 1
                    continue

                parsed["line_num"] = line_num
                events.append(parsed)
    except FileNotFoundError:
        print(f"ERROR: Input log file not found: {log_path}")
        sys.exit(2)
    except Exception as e:
        print(f"ERROR: Failed to read input log: {e}")
        sys.exit(2)

    if n_malformed > 0:
        print(f"  ({n_malformed} malformed lines skipped)")
    return events


def validate_mouse_events(events):
    """Validate mouse movement and click events."""
    failures = []
    mouse_moves = [e for e in events if e["event_type"] == "MOUSE_MOVE"]
    mouse_buttons = [e for e in events if e["event_type"] == "MOUSE_BUTTON"]

    # Check mouse movement — look for at least one non-(0,0) delta
    if mouse_moves:
        coords_changed = False
        for e in mouse_moves:
            args = e.get("event_args")
            if isinstance(args, list) and len(args) >= 2:
                try:
                    dx, dy = args[0], args[1]
                    if dx != 0 or dy != 0:
                        coords_changed = True
                        break
                except (ValueError, TypeError):
                    continue

        if coords_changed:
            print(f"  ✓ Mouse movement: {len(mouse_moves)} events with coordinate changes")
        else:
            msg = "Mouse movement events exist but all deltas are (0, 0)"
            print(f"  ✗ {msg}")
            failures.append(msg)
    else:
        msg = "No mouse movement events found"
        print(f"  ✗ {msg}")
        failures.append(msg)

    # Check mouse buttons (clicks)
    if mouse_buttons:
        left_clicks = 0
        right_clicks = 0
        for e in mouse_buttons:
            args = e.get("event_args")
            if isinstance(args, list) and len(args) >= 2:
                try:
                    button = args[0]
                    pressed = args[1]
                    # Standard Windows virtual key codes: 1=left, 2=right, 4=middle
                    if button == 1 and pressed:
                        left_clicks += 1
                    elif button == 2 and pressed:
                        right_clicks += 1
                except (ValueError, TypeError):
                    continue

        if left_clicks > 0:
            print(f"  ✓ Left button clicks: {left_clicks} events")
        else:
            msg = "No left button click events found"
            print(f"  ✗ {msg}")
            failures.append(msg)

        if right_clicks > 0:
            print(f"  ✓ Right button clicks: {right_clicks} events")
        else:
            msg = "No right button click events found"
            print(f"  ✗ {msg}")
            failures.append(msg)
    else:
        msg = "No mouse button events found"
        print(f"  ✗ {msg}")
        failures.append(msg)

    return failures


def validate_keyboard_events(events):
    """Validate keyboard events."""
    failures = []
    keyboard_events = [e for e in events if e["event_type"] == "KEYBOARD"]

    if keyboard_events:
        keydown_count = 0
        keyup_count = 0
        keycodes_seen = set()

        for e in keyboard_events:
            args = e.get("event_args")
            if isinstance(args, list) and len(args) >= 2:
                try:
                    keycode = args[0]
                    pressed = args[1]
                    keycodes_seen.add(keycode)
                    if pressed:
                        keydown_count += 1
                    else:
                        keyup_count += 1
                except (ValueError, TypeError):
                    continue

        print(f"  ✓ Keyboard events: {len(keyboard_events)} total")
        print(f"    - Key down: {keydown_count}")
        print(f"    - Key up: {keyup_count}")
        print(f"    - Unique keys: {len(keycodes_seen)}")

        if keydown_count > 0 and keyup_count > 0:
            print("  ✓ Both keydown and keyup events present")
        else:
            msg = f"Missing keydown ({keydown_count}) or keyup ({keyup_count}) events"
            print(f"  ⚠ {msg} (may be intentional if keys are still held)")
    else:
        msg = "No keyboard events found"
        print(f"  ✗ {msg}")
        failures.append(msg)

    return failures


def validate_timestamps(events):
    """Validate event timestamps."""
    failures = []

    if not events:
        msg = "No events to validate timestamps"
        print(f"  ✗ {msg}")
        failures.append(msg)
        return failures

    timestamps = [e["timestamp"] for e in events]
    sorted_timestamps = sorted(timestamps)

    if timestamps != sorted_timestamps:
        msg = "Event timestamps are not monotonic (not in ascending order)"
        print(f"  ✗ {msg}")
        failures.append(msg)
    else:
        print(f"  ✓ Timestamps are monotonic ({len(events)} events)")

    if len(timestamps) >= 2:
        time_span = timestamps[-1] - timestamps[0]
        print(f"  ✓ Event time span: {time_span:.2f} seconds")

        if time_span > 0:
            events_per_second = len(events) / time_span
            print(f"  ✓ Event density: {events_per_second:.1f} events/second")

    return failures


def validate_metadata_markers(events):
    """Validate that required metadata markers exist."""
    failures = []

    event_types = {e["event_type"] for e in events}

    for marker in ("START", "END", "VIDEO_START", "VIDEO_END"):
        if marker in event_types:
            print(f"  ✓ {marker} marker present")
        else:
            print(f"  ⚠ Missing {marker} marker")

    return failures


def main():
    parser = argparse.ArgumentParser(description="CI input log validation")
    parser.add_argument("input_log", help="Path to the input log (inputs.jsonl or inputs.csv)")
    parser.add_argument(
        "--require-mouse",
        action="store_true",
        help="Require mouse events to be present",
    )
    parser.add_argument(
        "--require-keyboard",
        action="store_true",
        help="Require keyboard events to be present",
    )
    args = parser.parse_args()

    print(f"\n=== Checking input log: {args.input_log} ===\n")

    events = read_input_log(args.input_log)

    if not events:
        print("ERROR: No valid events found in input log")
        sys.exit(2)

    print(f"Total events parsed: {len(events)}\n")

    all_failures = []

    print("Metadata markers:")
    all_failures.extend(validate_metadata_markers(events))
    print()

    print("Timestamp validation:")
    all_failures.extend(validate_timestamps(events))
    print()

    print("Mouse event validation:")
    if args.require_mouse or any(e["event_type"].startswith("MOUSE") for e in events):
        all_failures.extend(validate_mouse_events(events))
    else:
        print("  (skipped -- no mouse events and not required)")
    print()

    print("Keyboard event validation:")
    if args.require_keyboard or any(e["event_type"] == "KEYBOARD" for e in events):
        all_failures.extend(validate_keyboard_events(events))
    else:
        print("  (skipped -- no keyboard events and not required)")
    print()

    if all_failures:
        print("=== FAILED ===")
        for f in all_failures:
            print(f"  • {f}")
        sys.exit(1)
    else:
        print("=== ALL CHECKS PASSED ===")
        sys.exit(0)


if __name__ == "__main__":
    main()
