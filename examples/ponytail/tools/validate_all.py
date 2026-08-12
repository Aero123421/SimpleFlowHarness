#!/usr/bin/env python3
"""Run the installed sfh validator over every numbered flow in this directory."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sfh", default="sfh", help="sfh executable")
    parser.add_argument("--profiles", type=Path)
    parser.add_argument("--strict", action="store_true")
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[1]
    flows = sorted(root.glob("[0-9][0-9]-*.yaml"))
    failures: list[dict[str, object]] = []

    for flow in flows:
        cmd = [args.sfh, "validate", str(flow), "--json"]
        if args.strict:
            cmd.append("--strict")
        if args.profiles:
            cmd.extend(["--profiles", str(args.profiles.resolve())])
        proc = subprocess.run(cmd, text=True, capture_output=True)
        try:
            payload = json.loads(proc.stdout)
        except json.JSONDecodeError:
            payload = {"stdout": proc.stdout, "stderr": proc.stderr}
        print(f"{'OK' if proc.returncode == 0 else 'FAIL'}  {flow.name}")
        if proc.returncode != 0:
            failures.append({"flow": flow.name, "returncode": proc.returncode, "result": payload})

    if failures:
        print(json.dumps({"failures": failures}, indent=2, ensure_ascii=False))
        return 1
    print(f"Validated {len(flows)} flows.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
