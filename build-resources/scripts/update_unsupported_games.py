#!/usr/bin/env python3
"""
Updates unsupported_games.json with new games from OWL Tube CSV exports.

Usage:
    python update_unsupported_games.py <csv_file>

The CSV file should have columns where:
- Column 1 (index 0): Some identifier (ignored)
- Column 2 (index 1): Game name
- Column 3 (index 2): Binaries (semicolon-separated)
"""

import json
import shutil
import subprocess
import sys
from pathlib import Path


def main():
    if len(sys.argv) < 2:
        print("Usage: python update_unsupported_games.py <csv_file>", file=sys.stderr)
        sys.exit(1)

    csv_path = Path(sys.argv[1])
    if not csv_path.exists():
        print(f"Error: File not found: {csv_path}", file=sys.stderr)
        sys.exit(1)

    # Path to unsupported_games.json relative to this script
    script_dir = Path(__file__).parent
    unsupported_games_path = (
        script_dir.parent.parent
        / "crates"
        / "constants"
        / "src"
        / "unsupported_games.json"
    )

    if not unsupported_games_path.exists():
        print(
            f"Error: unsupported_games.json not found at {unsupported_games_path}",
            file=sys.stderr,
        )
        sys.exit(1)

    # Read existing games
    with open(unsupported_games_path, "r", encoding="utf-8") as f:
        existing_games = json.load(f)

    # Build set of existing binaries (lowercased for comparison)
    existing_binaries = set()
    for game in existing_games:
        for binary in game.get("binaries", []):
            existing_binaries.add(binary.lower())

    # Read and parse CSV
    with open(csv_path, "r", encoding="utf-8") as f:
        contents = f.read()

    lines = contents.splitlines()[1:]  # Skip header
    games_from_csv = [line.split(",")[1:3] for line in lines if line.strip()]

    # Convert to JSON format
    new_games = []
    for game in games_from_csv:
        if len(game) < 2:
            continue
        name = game[0].strip()
        binaries_str = game[1].strip() if len(game) > 1 else ""
        if not name or not binaries_str:
            continue

        binaries = [
            b.strip().lower().removesuffix(".exe")
            for b in binaries_str.split(";")
            if b.strip()
        ]

        # Check if any binary already exists
        dominated_binaries = [b for b in binaries if b.lower() in existing_binaries]
        new_binaries = [b for b in binaries if b.lower() not in existing_binaries]

        if dominated_binaries:
            print(f"Skipping '{name}': binaries already exist: {dominated_binaries}")
            continue

        if not new_binaries:
            continue

        new_games.append(
            {"name": name, "binaries": new_binaries, "reason": "EnoughData"}
        )

    if not new_games:
        print("No new games to add.")
        return

    print(f"Adding {len(new_games)} new games:")
    for game in new_games:
        print(f"  - {game['name']}: {game['binaries']}")

    # Add new games to the list
    existing_games.extend(new_games)

    # Write back
    with open(unsupported_games_path, "w", encoding="utf-8") as f:
        json.dump(existing_games, f, indent=2)
        f.write("\n")

    print(f"Updated {unsupported_games_path}")

    # Format with prettier if npx is available
    npx_path = shutil.which("npx")
    if npx_path:
        print("Formatting with prettier...")
        try:
            subprocess.run(
                [npx_path, "-y", "prettier", "--write", str(unsupported_games_path)],
                check=True,
                capture_output=True,
            )
            print("Formatted successfully.")
        except subprocess.CalledProcessError as e:
            print(
                f"Warning: prettier formatting failed: {e.stderr.decode()}",
                file=sys.stderr,
            )


if __name__ == "__main__":
    main()
