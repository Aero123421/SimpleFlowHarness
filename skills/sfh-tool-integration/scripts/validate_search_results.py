#!/usr/bin/env python3
"""Validate a generic web-search JSON artifact.

Expected shape: an object with a non-empty `results` array. Each result must
contain non-empty `title` and `url` strings. Adapt this validator to the exact
CLI schema used by your project rather than weakening it to accept anything.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact", type=Path)
    args = parser.parse_args()
    try:
        value = json.loads(args.artifact.read_text(encoding="utf-8"))
    except Exception as exc:
        print(json.dumps({"ok": False, "error": f"cannot parse search JSON: {exc}"}))
        return 2
    results = value.get("results") if isinstance(value, dict) else None
    if not isinstance(results, list) or not results:
        print(json.dumps({"ok": False, "error": "results must be a non-empty array"}))
        return 2
    bad = []
    for i, item in enumerate(results):
        if not isinstance(item, dict):
            bad.append(i)
            continue
        if not isinstance(item.get("title"), str) or not item["title"].strip():
            bad.append(i)
            continue
        if not isinstance(item.get("url"), str) or not item["url"].strip():
            bad.append(i)
    if bad:
        print(json.dumps({"ok": False, "error": "invalid result entries", "indexes": bad}))
        return 2
    print(json.dumps({"ok": True, "result_count": len(results)}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
