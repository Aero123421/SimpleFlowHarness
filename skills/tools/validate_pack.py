#!/usr/bin/env python3
"""Validate the skill pack structure and parse bundled YAML assets."""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

import yaml


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, default=Path.cwd())
    args = parser.parse_args()
    root = args.root.resolve()
    skill_check = subprocess.run([sys.executable, str(root / "tools/validate_skills.py"), str(root)])
    failures = int(skill_check.returncode != 0)
    skills_dir = root / "skills" if (root / "skills").is_dir() else root
    assets = sorted(skills_dir.glob("*/assets/*.yaml"))
    for path in assets:
        try:
            value = yaml.safe_load(path.read_text(encoding="utf-8"))
            assert isinstance(value, dict) and isinstance(value.get("steps"), list)
            print(f"YAML  {path.relative_to(root)}")
        except Exception as exc:
            failures += 1
            print(f"ERROR {path.relative_to(root)}: {exc}")
    for script in sorted(skills_dir.glob("*/scripts/*.py")):
        proc = subprocess.run([sys.executable, str(script), "--help"], capture_output=True, text=True)
        if proc.returncode != 0:
            failures += 1
            print(f"ERROR {script.relative_to(root)} --help: {proc.stderr.strip()}")
        else:
            print(f"SCRIPT {script.relative_to(root)}")
    print(f"{len(assets)} YAML asset(s), failures={failures}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
