#!/usr/bin/env python3
"""Generate gameinfo.xlsx for a recording session.

This script creates an Excel workbook with the following sheets:
- Session: Session metadata (session_id, game, start_time, etc.)
- GameEvents: Placeholder for game-specific events
- BlockStats: Placeholder for block-level statistics
- BiomeVisits: Placeholder for biome visit data

The schema matches lint_v3_prd_grounded.py criteria #23-24.
"""

import argparse
import json
import sys
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, Optional

try:
    from openpyxl import Workbook
    from openpyxl.styles import Font, Alignment
except ImportError:
    print("ERROR: openpyxl not installed. Run: pip install openpyxl", file=sys.stderr)
    sys.exit(1)


def load_metadata(session_dir: Path) -> Dict[str, Any]:
    """Load metadata from metadata.json or systeminfo.json."""
    # Try metadata.json first
    metadata_path = session_dir / "metadata.json"
    if not metadata_path.exists():
        metadata_path = session_dir / "systeminfo.json"
    
    if not metadata_path.exists():
        raise FileNotFoundError(f"No metadata file found in {session_dir}")
    
    with open(metadata_path, 'r') as f:
        return json.load(f)


def load_frames(session_dir: Path) -> list[Dict[str, Any]]:
    """Load frame timestamps from frames.jsonl."""
    frames_path = session_dir / "frames.jsonl"
    if not frames_path.exists():
        return []
    
    frames = []
    with open(frames_path, 'r') as f:
        for line in f:
            if line.strip():
                frames.append(json.loads(line))
    return frames


def create_workbook(session_dir: Path, output_path: Optional[Path] = None) -> None:
    """Create the gameinfo.xlsx workbook."""
    session_dir = Path(session_dir)
    
    # Load metadata
    metadata = load_metadata(session_dir)
    
    # Determine output path
    if output_path is None:
        output_path = session_dir / "gameinfo.xlsx"
    
    # Create workbook
    wb = Workbook()
    
    # Remove default sheet
    wb.remove(wb.active)
    
    # === Session Sheet ===
    ws_session = wb.create_sheet("Session")
    
    # Header style
    header_font = Font(bold=True)
    header_alignment = Alignment(horizontal="center")
    
    # Session data rows
    session_data = [
        ("session_id", metadata.get("session_id", "")),
        ("game_process_name", metadata.get("gameProcessName", metadata.get("game_process_name", ""))),
        ("start_time", metadata.get("start_time", "")),
        ("end_time", metadata.get("end_time", "")),
        ("duration_seconds", metadata.get("duration_seconds", "")),
        ("resolution_width", metadata.get("width", "")),
        ("resolution_height", metadata.get("height", "")),
        ("fps_target", metadata.get("fps_target", "")),
        ("fps_actual", metadata.get("fps_actual", "")),
        ("encoder", metadata.get("encoder", "")),
        ("recording_drive", metadata.get("recording_drive", "")),
        ("gpu", metadata.get("gpu", "")),
        ("cpu", metadata.get("cpu", "")),
        ("ram_gb", metadata.get("ram_gb", "")),
        ("os", metadata.get("os", "")),
    ]
    
    for row_idx, (key, value) in enumerate(session_data, start=1):
        ws_session.cell(row=row_idx, column=1, value=key)
        ws_session.cell(row=row_idx, column=2, value=value)
        ws_session.cell(row=row_idx, column=1).font = header_font
    
    # === GameEvents Sheet (placeholder) ===
    ws_events = wb.create_sheet("GameEvents")
    ws_events.append(["event_type", "timestamp", "details"])
    ws_events.append(["placeholder_event", "0", "No game events recorded"])
    for cell in ws_events[1]:
        cell.font = header_font
    
    # === BlockStats Sheet (placeholder) ===
    ws_blocks = wb.create_sheet("BlockStats")
    ws_blocks.append(["block_id", "x", "y", "z", "interactions"])
    ws_blocks.append(["placeholder", "0", "0", "0", "0"])
    for cell in ws_blocks[1]:
        cell.font = header_font
    
    # === BiomeVisits Sheet (placeholder) ===
    ws_biomes = wb.create_sheet("BiomeVisits")
    ws_biomes.append(["biome_name", "entry_time", "duration_seconds", "visits"])
    ws_biomes.append(["placeholder_biome", "0", "0", "0"])
    for cell in ws_biomes[1]:
        cell.font = header_font
    
    # Auto-adjust column widths
    for ws in wb.worksheets:
        for column in ws.columns:
            max_length = 0
            column_letter = column[0].column_letter
            for cell in column:
                try:
                    if len(str(cell.value)) > max_length:
                        max_length = len(str(cell.value))
                except:
                    pass
            adjusted_width = min(max_length + 2, 50)
            ws.column_dimensions[column_letter].width = adjusted_width
    
    # Save workbook
    wb.save(output_path)
    print(f"Created {output_path}")


def main():
    parser = argparse.ArgumentParser(description="Generate gameinfo.xlsx for a recording session")
    parser.add_argument("session_dir", type=Path, help="Path to the recording session directory")
    parser.add_argument("--output", "-o", type=Path, help="Output path (default: session_dir/gameinfo.xlsx)")
    args = parser.parse_args()
    
    if not args.session_dir.exists():
        print(f"ERROR: Session directory does not exist: {args.session_dir}", file=sys.stderr)
        sys.exit(1)
    
    try:
        create_workbook(args.session_dir, args.output)
    except Exception as e:
        print(f"ERROR: Failed to create gameinfo.xlsx: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
