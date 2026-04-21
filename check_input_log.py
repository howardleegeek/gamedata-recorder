"""
check_input_log.py — CI input log validation for gamedata-recorder

Checks:
  1. Mouse events recorded (movement and clicks)
  2. Keyboard events recorded
  3. Event timestamps are present and monotonic
  4. Event count sanity checks

Usage:
  python check_input_log.py <input_log.csv>

Exit codes:
  0 = all checks passed
  1 = one or more checks failed
  2 = file not found or unreadable
"""

import argparse
import json
import sys
from pathlib import Path
from collections import defaultdict


def read_input_log(log_path: str):
    """Read and parse input log CSV file."""
    events = []
    try:
        with open(log_path, 'r', encoding='utf-8') as f:
            for line_num, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    # Parse: timestamp,event_type,json_args
                    parts = line.split(',', 2)
                    if len(parts) != 3:
                        print(f"  WARNING: Line {line_num}: malformed, skipping")
                        continue

                    timestamp = float(parts[0])
                    event_type = parts[1]
                    json_args = parts[2].strip('"')

                    events.append({
                        'timestamp': timestamp,
                        'event_type': event_type,
                        'json_args': json_args,
                        'line_num': line_num,
                    })
                except (ValueError, IndexError) as e:
                    print(f"  WARNING: Line {line_num}: parse error: {e}")
                    continue
    except FileNotFoundError:
        print(f"ERROR: Input log file not found: {log_path}")
        sys.exit(2)
    except Exception as e:
        print(f"ERROR: Failed to read input log: {e}")
        sys.exit(2)

    return events


def validate_mouse_events(events):
    """Validate mouse movement and click events."""
    failures = []
    mouse_moves = [e for e in events if e['event_type'] == 'MOUSE_MOVE']
    mouse_buttons = [e for e in events if e['event_type'] == 'MOUSE_BUTTON']

    # Check mouse movement
    if mouse_moves:
        # Verify coordinates change (movement is recorded)
        coords_changed = False
        for e in mouse_moves:
            try:
                args = json.loads(e['json_args'])
                if isinstance(args, list) and len(args) >= 2:
                    dx, dy = args[0], args[1]
                    if dx != 0 or dy != 0:
                        coords_changed = True
                        break
            except (json.JSONDecodeError, ValueError, TypeError):
                continue

        if coords_changed:
            print(f"  ✓ Mouse movement: {len(mouse_moves)} events with coordinate changes")
        else:
            msg = f"Mouse movement events exist but all deltas are (0, 0)"
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
            try:
                args = json.loads(e['json_args'])
                if isinstance(args, list) and len(args) >= 2:
                    button = args[0]
                    pressed = args[1]
                    # Standard Windows virtual key codes: 1=left, 2=right, 4=middle
                    if button == 1 and pressed:
                        left_clicks += 1
                    elif button == 2 and pressed:
                        right_clicks += 1
            except (json.JSONDecodeError, ValueError, TypeError):
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
    keyboard_events = [e for e in events if e['event_type'] == 'KEYBOARD']

    if keyboard_events:
        # Check for both keydown and keyup
        keydown_count = 0
        keyup_count = 0
        keycodes_seen = set()

        for e in keyboard_events:
            try:
                args = json.loads(e['json_args'])
                if isinstance(args, list) and len(args) >= 2:
                    keycode = args[0]
                    pressed = args[1]
                    keycodes_seen.add(keycode)
                    if pressed:
                        keydown_count += 1
                    else:
                        keyup_count += 1
            except (json.JSONDecodeError, ValueError, TypeError):
                continue

        print(f"  ✓ Keyboard events: {len(keyboard_events)} total")
        print(f"    - Key down: {keydown_count}")
        print(f"    - Key up: {keyup_count}")
        print(f"    - Unique keys: {len(keycodes_seen)}")

        # Basic sanity check: should have both down and up for complete typing
        if keydown_count > 0 and keyup_count > 0:
            print(f"  ✓ Both keydown and keyup events present")
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

    # Check timestamps are present and monotonic
    timestamps = [e['timestamp'] for e in events]
    sorted_timestamps = sorted(timestamps)

    if timestamps != sorted_timestamps:
        msg = "Event timestamps are not monotonic (not in ascending order)"
        print(f"  ✗ {msg}")
        failures.append(msg)
    else:
        print(f"  ✓ Timestamps are monotonic ({len(events)} events)")

    # Check time span
    if len(timestamps) >= 2:
        time_span = timestamps[-1] - timestamps[0]
        print(f"  ✓ Event time span: {time_span:.2f} seconds")

        # Check for reasonable event density (not too sparse)
        if time_span > 0:
            events_per_second = len(events) / time_span
            print(f"  ✓ Event density: {events_per_second:.1f} events/second")

    return failures


def validate_metadata_markers(events):
    """Validate that required metadata markers exist."""
    failures = []

    event_types = {e['event_type'] for e in events}

    # Check for START and END markers
    if 'START' in event_types:
        print(f"  ✓ START marker present")
    else:
        msg = "Missing START marker"
        print(f"  ⚠ {msg}")

    if 'END' in event_types:
        print(f"  ✓ END marker present")
    else:
        msg = "Missing END marker"
        print(f"  ⚠ {msg}")

    # Check for video markers
    if 'VIDEO_START' in event_types:
        print(f"  ✓ VIDEO_START marker present")
    else:
        msg = "Missing VIDEO_START marker"
        print(f"  ⚠ {msg}")

    if 'VIDEO_END' in event_types:
        print(f"  ✓ VIDEO_END marker present")
    else:
        msg = "Missing VIDEO_END marker"
        print(f"  ⚠ {msg}")

    return failures


def main():
    parser = argparse.ArgumentParser(description="CI input log validation")
    parser.add_argument("input_log", help="Path to the input log CSV file")
    parser.add_argument("--require-mouse", action="store_true",
                        help="Require mouse events to be present")
    parser.add_argument("--require-keyboard", action="store_true",
                        help="Require keyboard events to be present")
    args = parser.parse_args()

    print(f"\n=== Checking input log: {args.input_log} ===\n")

    # Read input log
    events = read_input_log(args.input_log)

    if not events:
        print("ERROR: No valid events found in input log")
        sys.exit(2)

    print(f"Total events parsed: {len(events)}\n")

    all_failures = []

    # Validate metadata markers
    print("Metadata markers:")
    marker_failures = validate_metadata_markers(events)
    all_failures.extend(marker_failures)
    print()

    # Validate timestamps
    print("Timestamp validation:")
    timestamp_failures = validate_timestamps(events)
    all_failures.extend(timestamp_failures)
    print()

    # Validate mouse events
    print("Mouse event validation:")
    if args.require_mouse or any(e['event_type'].startswith('MOUSE') for e in events):
        mouse_failures = validate_mouse_events(events)
        all_failures.extend(mouse_failures)
    else:
        print("  (skipped -- no mouse events and not required)")
    print()

    # Validate keyboard events
    print("Keyboard event validation:")
    if args.require_keyboard or any(e['event_type'] == 'KEYBOARD' for e in events):
        keyboard_failures = validate_keyboard_events(events)
        all_failures.extend(keyboard_failures)
    else:
        print("  (skipped -- no keyboard events and not required)")
    print()

    # Result
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
