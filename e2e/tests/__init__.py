"""Opaque-box E2E test suite packages (Tiers 1-4)."""

from __future__ import annotations

from pathlib import Path
import sys

_E2E_ROOT = str(Path(__file__).resolve().parents[1])
if _E2E_ROOT not in sys.path:
    sys.path.insert(0, _E2E_ROOT)
