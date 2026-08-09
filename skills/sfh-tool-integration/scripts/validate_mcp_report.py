#!/usr/bin/env python3
"""Check that an MCP research report names its evidence boundary.

The report is plain text. This deliberately validates only required mechanical
markers; it does not decide whether the research conclusion is correct.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

REQUIRED = ("server", "tool", "source", "limitation")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact", type=Path)
    args = parser.parse_args()
    try:
        text = args.artifact.read_text(encoding="utf-8")
    except Exception as exc:
        print(json.dumps({"ok": False, "error": str(exc)}))
        return 2
    lower = text.lower()
    missing = [word for word in REQUIRED if word not in lower]
    if missing:
        print(json.dumps({"ok": False, "missing_markers": missing}, sort_keys=True))
        return 2
    print(json.dumps({"ok": True, "chars": len(text)}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
