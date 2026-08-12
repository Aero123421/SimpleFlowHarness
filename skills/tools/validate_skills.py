#!/usr/bin/env python3
"""Validate Agent Skill structure and frontmatter without loading a client."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

import yaml

NAME_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
REF_RE = re.compile(r"(?<![A-Za-z0-9_./-])((?:references|scripts|assets)/[A-Za-z0-9_.\-/]+)")


def parse_skill(path: Path) -> tuple[dict, str]:
    text = path.read_text(encoding="utf-8")
    if not text.startswith("---\n"):
        raise ValueError("SKILL.md must start with YAML frontmatter")
    end = text.find("\n---\n", 4)
    if end < 0:
        raise ValueError("SKILL.md has no closing frontmatter delimiter")
    meta = yaml.safe_load(text[4:end])
    if not isinstance(meta, dict):
        raise ValueError("frontmatter must be a mapping")
    return meta, text[end + 5 :]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, default=Path.cwd())
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve()
    skills_dir = root / "skills" if (root / "skills").is_dir() else root
    errors: list[dict[str, str]] = []
    warnings: list[dict[str, str]] = []
    skills = []

    for skill_file in sorted(skills_dir.glob("*/SKILL.md")):
        rel = str(skill_file.relative_to(root))
        try:
            meta, body = parse_skill(skill_file)
        except Exception as exc:
            errors.append({"file": rel, "message": str(exc)})
            continue
        name = meta.get("name")
        description = meta.get("description")
        if not isinstance(name, str) or not NAME_RE.fullmatch(name) or len(name) > 64:
            errors.append({"file": rel, "message": "invalid name"})
        if name != skill_file.parent.name:
            errors.append({"file": rel, "message": "name must match parent directory"})
        if not isinstance(description, str) or not description.strip() or len(description) > 1024:
            errors.append({"file": rel, "message": "description must be 1..1024 chars"})
        compatibility = meta.get("compatibility")
        if compatibility is not None and (not isinstance(compatibility, str) or len(compatibility) > 500):
            errors.append({"file": rel, "message": "compatibility must be <=500 chars"})
        if meta.get("metadata") is not None and not isinstance(meta.get("metadata"), dict):
            errors.append({"file": rel, "message": "metadata must be a mapping"})
        lines = skill_file.read_text(encoding="utf-8").count("\n") + 1
        if lines > 500:
            errors.append({"file": rel, "message": f"SKILL.md has {lines} lines (>500)"})
        if "allowed-tools" in meta:
            warnings.append({"file": rel, "message": "allowed-tools is experimental; verify client behavior"})
        for ref in sorted(set(REF_RE.findall(body))):
            if ".." in Path(ref).parts:
                errors.append({"file": rel, "message": f"unsafe relative reference: {ref}"})
                continue
            target = skill_file.parent / ref
            if not target.is_file():
                errors.append({"file": rel, "message": f"missing referenced file: {ref}"})
        skills.append({"name": name, "description": description, "file": rel, "lines": lines})

    if not skills:
        errors.append({"file": str(skills_dir), "message": "no */SKILL.md files found"})

    result = {"ok": not errors, "skills": skills, "warnings": warnings, "errors": errors}
    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        for skill in skills:
            print(f"OK    {skill['name']} ({skill['lines']} lines)")
        for item in warnings:
            print(f"WARN  {item['file']}: {item['message']}")
        for item in errors:
            print(f"ERROR {item['file']}: {item['message']}")
        print(f"{len(skills)} skill(s), {len(warnings)} warning(s), {len(errors)} error(s)")
    return 0 if not errors else 1


if __name__ == "__main__":
    raise SystemExit(main())
