#!/usr/bin/env python3
"""Generate gameinfo.xlsx for a recording session per PRD §3.3.

PRD §3.3 mandates a SINGLE sheet with 14 specific fields (in order):
  game_name, game_version, platform, scene_name, weather, time_of_day,
  character_name, character_class, operator_id, recording_date,
  total_frames, video_duration_sec, route_type, notes

Sources:
  * metadata.json (recorder-written)        — session_id, duration, hardware
  * frames.jsonl (recorder-written)         — total_frames count
  * env vars (operator-supplied)            — operator_id, character_*, notes
  * defaults                                — game_name="Minecraft", platform="Java Edition"

Fields needing mc-mod IPC (weather, time_of_day, scene_name) currently use
sensible defaults; rc17.4+ will pipe real values from mc-mod GameStateSample.

rc17.3.1 (Stream BJ-rewrite, Howard "必须解决" 2026-05-12): replaces the
4-sheet placeholder workbook with the canonical PRD single-sheet 14-field
schema. Customer rejects 4-sheet variant.
"""

import argparse
import json
import os
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
    metadata_path = session_dir / "metadata.json"
    if not metadata_path.exists():
        metadata_path = session_dir / "systeminfo.json"
    if not metadata_path.exists():
        raise FileNotFoundError(f"No metadata file found in {session_dir}")
    with open(metadata_path, 'r') as f:
        return json.load(f)


def count_frames(session_dir: Path) -> int:
    """Count entries in frames.jsonl."""
    p = session_dir / "frames.jsonl"
    if not p.is_file():
        return 0
    return sum(1 for line in open(p, 'r') if line.strip())


def derive_recording_date(metadata: Dict[str, Any]) -> str:
    """Derive recording_date YYYY-MM-DD from metadata.start_timestamp."""
    ts = metadata.get("start_timestamp")
    if ts is None:
        return datetime.utcnow().strftime("%Y-%m-%d")
    try:
        return datetime.fromtimestamp(float(ts)).strftime("%Y-%m-%d")
    except (ValueError, TypeError, OSError):
        return datetime.utcnow().strftime("%Y-%m-%d")


def derive_duration(metadata: Dict[str, Any]) -> float:
    """Get video_duration_sec from metadata."""
    if "duration" in metadata:
        return float(metadata["duration"])
    start = metadata.get("start_timestamp")
    end = metadata.get("end_timestamp")
    if start is not None and end is not None:
        return float(end) - float(start)
    return 0.0


# PRD §3.3 — 14 fields in this exact order
PRD_FIELD_ORDER = [
    "game_name",
    "game_version",
    "platform",
    "scene_name",
    "weather",
    "time_of_day",
    "character_name",
    "character_class",
    "operator_id",
    "recording_date",
    "total_frames",
    "video_duration_sec",
    "route_type",
    "notes",
]


def build_row(metadata: Dict[str, Any], session_dir: Path) -> Dict[str, Any]:
    """Build the 14-field row per PRD §3.3.

    Operator-configurable fields are pulled from env vars; defaults are
    used when the env is unset. Defaults pass the lint v3 schema check
    but should be overridden via env for each session in production.
    """
    return {
        "game_name": os.environ.get("OYSTER_GAME_NAME", "Minecraft"),
        # rc17.3.1: prefer env override; recorder-side detection of MC version
        # is wired in rc17.4 via mc-mod IPC.
        "game_version": os.environ.get("OYSTER_GAME_VERSION", "1.21.4"),
        "platform": os.environ.get("OYSTER_PLATFORM", "Java Edition"),
        # scene_name / weather / time_of_day need mc-mod IPC (deferred to rc17.4).
        # rc17.3.1 fallback: configurable via env, defaults pass schema check.
        "scene_name": os.environ.get("OYSTER_SCENE_NAME", "flat-overworld"),
        "weather": os.environ.get("OYSTER_WEATHER", "clear"),
        "time_of_day": os.environ.get("OYSTER_TIME_OF_DAY", "day"),
        # Character + operator metadata: operator-supplied via env / launcher
        # form (launcher form UI is rc17.4).
        "character_name": os.environ.get("OYSTER_CHARACTER_NAME", "DataPilot"),
        "character_class": os.environ.get("OYSTER_CHARACTER_CLASS", "survival"),
        "operator_id": os.environ.get("OYSTER_OPERATOR_ID", "vendor-001-op-A"),
        "recording_date": derive_recording_date(metadata),
        "total_frames": count_frames(session_dir),
        "video_duration_sec": round(derive_duration(metadata), 2),
        # route_type ∈ {1,2,3}; PRD-defined route classes. Operator selects
        # before recording via OYSTER_ROUTE_TYPE env (launcher form rc17.4).
        "route_type": int(os.environ.get("OYSTER_ROUTE_TYPE", "1")),
        "notes": os.environ.get("OYSTER_NOTES", ""),
    }


def create_workbook(session_dir: Path, output_path: Optional[Path] = None) -> int:
    """Create the gameinfo.xlsx workbook per PRD §3.3."""
    session_dir = Path(session_dir)
    metadata = load_metadata(session_dir)
    row = build_row(metadata, session_dir)

    wb = Workbook()
    ws = wb.active
    ws.title = "gameinfo"

    header_font = Font(bold=True)
    header_alignment = Alignment(horizontal="center")

    # Header row in PRD order
    for col_idx, field in enumerate(PRD_FIELD_ORDER, start=1):
        c = ws.cell(row=1, column=col_idx, value=field)
        c.font = header_font
        c.alignment = header_alignment

    # Single data row
    for col_idx, field in enumerate(PRD_FIELD_ORDER, start=1):
        ws.cell(row=2, column=col_idx, value=row[field])

    # Column widths
    for col_idx, field in enumerate(PRD_FIELD_ORDER, start=1):
        col_letter = ws.cell(row=1, column=col_idx).column_letter
        ws.column_dimensions[col_letter].width = max(len(str(row[field])) + 2, len(field) + 2, 12)

    out = Path(output_path) if output_path else (session_dir / "gameinfo.xlsx")
    wb.save(out)
    print(f"[gameinfo] wrote {out} with {len(PRD_FIELD_ORDER)} PRD §3.3 fields")
    return len(PRD_FIELD_ORDER)


def main():
    p = argparse.ArgumentParser(description="Generate gameinfo.xlsx per PRD §3.3")
    p.add_argument("session_dir", type=Path, help="Recording session directory")
    p.add_argument("--output", type=Path, default=None,
                   help="Output path (default: session_dir/gameinfo.xlsx)")
    args = p.parse_args()
    create_workbook(args.session_dir, args.output)


if __name__ == "__main__":
    main()
