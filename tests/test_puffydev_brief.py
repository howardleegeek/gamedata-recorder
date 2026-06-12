"""Doc-presence tests for the 2026-05-01 puffydev brief.

Verifies the brief was written, is non-trivial in size, and contains the
section anchors a downstream reader (puffydev) needs to find. Stdlib-only,
no venv required:

    python3 tests/test_puffydev_brief.py

Exits 0 on pass, non-zero on fail. Each test prints its own outcome line.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
BRIEF = REPO_ROOT / "docs" / "PUFFYDEV_BRIEF_2026_05_01.md"

# Documents the brief references — must remain reachable for it to be useful.
REFERENCED_DOCS = [
    REPO_ROOT / "docs" / "RECORDER_BUYER_SPEC_FEATURES.md",
    REPO_ROOT / "crates" / "engine-telemetry" / "docs" / "CYBERPUNK_HOOK_RUNBOOK.md",
    REPO_ROOT / "crates" / "engine-telemetry" / "docs" / "GTA_V_HOOK_RUNBOOK.md",
]


def _fail(msg: str) -> None:
    print(f"FAIL: {msg}")
    sys.exit(1)


def _ok(msg: str) -> None:
    print(f"PASS: {msg}")


def test_brief_exists_and_nontrivial() -> None:
    """The brief file exists and is at least 4 KB (non-trivial)."""
    if not BRIEF.is_file():
        _fail(f"brief missing at {BRIEF}")
    size = BRIEF.stat().st_size
    if size < 4096:
        _fail(f"brief too small ({size} bytes; expected >= 4096)")
    _ok(f"brief exists, {size} bytes")


def test_brief_contains_required_sections() -> None:
    """The brief covers all sections mandated by the deliverable contract."""
    text = BRIEF.read_text(encoding="utf-8")
    required = [
        "TL;DR",
        "What Landed",
        "Queued for Puffydev",
        "Cross-Platform Tests",
        "Test Fixtures",
        "Where to Ask Questions",
        # Spec/runbook filenames the brief must point at:
        "RECORDER_BUYER_SPEC_FEATURES.md",
        "CYBERPUNK_HOOK_RUNBOOK.md",
        "GTA_V_HOOK_RUNBOOK.md",
        # Cross-references puffydev needs:
        "BUYER_SPEC_v1.md",
        "COORDINATE_SYSTEMS_GUIDE.md",
        # Test commands so puffydev can verify cross-platform CI locally:
        "cargo test -p action-camera-tests",
        "cargo test -p engine-telemetry",
    ]
    missing = [marker for marker in required if marker not in text]
    if missing:
        _fail(f"brief missing required markers: {missing}")
    _ok(f"brief contains all {len(required)} required markers")


def test_referenced_docs_exist() -> None:
    """Every spec/runbook the brief points puffydev at must actually exist."""
    missing = [str(p) for p in REFERENCED_DOCS if not p.is_file()]
    if missing:
        _fail(f"referenced docs missing: {missing}")
    _ok(f"all {len(REFERENCED_DOCS)} referenced docs exist")


def main() -> int:
    print(f"Repo root: {REPO_ROOT}")
    print(f"Brief:     {BRIEF}\n")
    test_brief_exists_and_nontrivial()
    test_brief_contains_required_sections()
    test_referenced_docs_exist()
    print("\nAll 3 tests passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
