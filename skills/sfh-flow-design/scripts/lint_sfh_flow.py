#!/usr/bin/env python3
"""Heuristic design lint for sfh YAML.

This does not replace `sfh validate --strict`. It finds legal-looking but
fragile patterns that AI-authored flows commonly produce.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any, Iterable

import yaml

PRESET_TOOLS = {"codex", "claude", "opencode", "grok", "agy", "pi", "cursor"}
SHELLS = {"sh", "bash", "cmd", "cmd.exe", "powershell", "powershell.exe", "pwsh", "pwsh.exe"}
MUTATING_REMOTE_PREFIXES = {
    ("gh", "issue", "create"),
    ("gh", "pr", "create"),
    ("gh", "release", "create"),
    ("gh", "workflow", "run"),
}
TERMINALS = {"end", "fail", "stuck"}


def effective(step: dict[str, Any], flow: dict[str, Any]) -> tuple[str | None, str | None]:
    profile = {}
    use = step.get("use")
    if isinstance(use, str):
        profile = (flow.get("profiles") or {}).get(use) or {}
    defaults = flow.get("defaults") or {}
    tool = step.get("tool", profile.get("tool", defaults.get("tool")))
    access = step.get("access", profile.get("access", defaults.get("access")))
    return tool, access


def effect(step: dict[str, Any], flow: dict[str, Any]) -> str:
    if isinstance(step.get("effects"), str):
        return step["effects"]
    if "parallel" in step and isinstance(step["parallel"], list):
        ranks = {"read": 0, "workspace": 1, "external": 2, "unknown": 3}
        values = [effect(c, flow) for c in step["parallel"] if isinstance(c, dict)]
        return max(values, key=lambda x: ranks.get(x, 3), default="read")
    if "cmd" in step:
        return "unknown"
    _, access = effective(step, flow)
    return "read" if access == "read" else "workspace" if access in {"write", "full"} else "unknown"


def replay(step: dict[str, Any], flow: dict[str, Any]) -> str:
    own = step.get("replay") or {}
    defaults = (flow.get("defaults") or {}).get("replay") or {}
    return own.get("unfinished", defaults.get("unfinished", "rerun"))


def walk(flow: dict[str, Any]) -> Iterable[tuple[dict[str, Any], str, int]]:
    for i, step in enumerate(flow.get("steps") or []):
        if not isinstance(step, dict):
            continue
        yield step, step.get("id", f"<step-{i}>"), i
        for child in step.get("parallel") or []:
            if isinstance(child, dict):
                yield child, f"{step.get('id', i)}.{child.get('id', '?')}", i


def add(findings: list[dict[str, str]], severity: str, code: str, step: str, message: str) -> None:
    findings.append({"severity": severity, "code": code, "step": step, "message": message})


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("flow", type=Path)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    try:
        flow = yaml.safe_load(args.flow.read_text(encoding="utf-8"))
    except Exception as exc:
        print(json.dumps({"ok": False, "error": str(exc)}) if args.json else f"ERROR: {exc}")
        return 2
    if not isinstance(flow, dict):
        print("ERROR: flow root must be a mapping")
        return 2

    findings: list[dict[str, str]] = []
    if flow.get("api_version") != 1:
        add(findings, "warning", "SFH001", "<flow>", "set api_version: 1 explicitly")

    steps = [s for s in flow.get("steps") or [] if isinstance(s, dict)]
    positions = {s.get("id"): i for i, s in enumerate(steps) if isinstance(s.get("id"), str)}
    seen: dict[str, str] = {}
    for step, label, top_index in walk(flow):
        sid = step.get("id")
        if isinstance(sid, str):
            key = sid.lower()
            if key in seen:
                add(findings, "error", "SFH002", label, f"duplicate id ignoring case; first seen as {seen[key]}")
            seen[key] = label
            if key in TERMINALS:
                add(findings, "error", "SFH003", label, "terminal name cannot be a step id")
        tool, access = effective(step, flow)
        is_ai = isinstance(tool, str) and tool in PRESET_TOOLS and "cmd" not in step
        if is_ai and access not in {"read", "write", "full"}:
            add(findings, "error", "SFH010", label, "AI step has no effective access: read/write/full")

        eff = effect(step, flow)
        rep = replay(step, flow)
        if eff == "unknown" and rep == "rerun":
            add(findings, "warning", "SFH020", label, "effects=unknown with replay.unfinished=rerun may duplicate effects")
        elif eff == "external" and rep == "rerun":
            explicit_safe_read = (
                step.get("effects") == "external"
                and isinstance(step.get("replay"), dict)
                and step["replay"].get("unfinished") == "rerun"
            )
            add(
                findings,
                "info" if explicit_safe_read else "warning",
                "SFH020",
                label,
                "external replay is rerun; confirm every remote operation is observational and safe to repeat",
            )
        if is_ai and step.get("retry_on") == "any":
            add(findings, "warning", "SFH021", label, "retry_on:any may resample a logical/model rejection as if it were transport failure")
        if is_ai and step.get("outcomes"):
            add(findings, "warning", "SFH022", label, "outcomes maps process exit codes; it usually cannot distinguish PASS/REVISE prose from a normal AI CLI")

        cmd = step.get("cmd")
        if isinstance(cmd, str):
            add(findings, "warning", "SFH030", label, "string cmd uses a platform shell; prefer argv form")
        if isinstance(cmd, list) and cmd:
            program = str(cmd[0])
            base = Path(program).name.lower()
            if "{{" in program:
                add(findings, "error", "SFH031", label, "templated argv[0] selects an executable dynamically")
            if base in SHELLS and len(cmd) >= 3 and str(cmd[1]).lower() in {"-c", "/c", "-command"}:
                script = str(cmd[2])
                if "{{" in script:
                    add(findings, "warning", "SFH032", label, "template expansion appears inside shell-parsed text")
            lowered = [str(arg).lower() for arg in cmd]
            mutates_remote = any(
                tuple(lowered[: len(prefix)]) == prefix for prefix in MUTATING_REMOTE_PREFIXES
            )
            mutates_remote |= base == "curl" and any(
                raw == "-F"
                or arg in {"--data", "--data-binary", "--data-raw", "--form", "-d"}
                or arg.startswith(("--data=", "--data-binary=", "--data-raw=", "--form="))
                or arg in {"post", "put", "patch", "delete"}
                for raw, arg in zip((str(item) for item in cmd[1:]), lowered[1:])
            )
            mutates_remote |= base == "wget" and any(
                arg.startswith(("--post-data", "--post-file"))
                or arg in {"--method=post", "--method=put", "--method=patch", "--method=delete"}
                for arg in lowered[1:]
            )
            if mutates_remote and eff == "read":
                add(findings, "warning", "SFH033", label, "command appears to mutate remote state but effects is read; declare effects: external")
            if len(cmd) >= 3 and base == "gh" and str(cmd[1:3]) == "['run', 'watch']":
                tail = [str(x) for x in cmd[3:]]
                if not tail or tail[0].startswith("-"):
                    add(findings, "error", "SFH034", label, "gh run watch has no explicit run id and may watch the wrong run")

        routes = step.get("route") or []
        backward_targets: list[str] = []
        saw_ai_trailer = False
        for route in routes:
            if not isinstance(route, dict):
                continue
            goto = route.get("goto")
            if isinstance(goto, str) and goto in positions and positions[goto] <= top_index:
                backward_targets.append(goto)
            if "when_members" in route:
                for child in step.get("parallel") or []:
                    if isinstance(child, dict) and child.get("on_error") != "continue":
                        add(findings, "warning", "SFH040", label, f"member {child.get('id')} lacks on_error: continue; one failure may bypass group voting")
            if is_ai and "when_last_line_is" in route:
                saw_ai_trailer = True
        if saw_ai_trailer:
            add(findings, "info", "SFH041", label, "exact AI trailer is a semantic fallback; add a catch-all to stuck and prefer a deterministic wrapper when possible")
        for target in sorted(set(backward_targets)):
            target_step = steps[positions[target]]
            # A cycle is bounded if ANY node on it is bounded, not specifically
            # the node the backward edge points at. Checking only the target
            # made this rule fire on every review/fix loop in the bundled
            # examples, where the bound sits on the fixer that jumps back -
            # which is the natural place for it, since that is the step whose
            # repeated failure is the thing worth giving up on.
            defaults = flow.get("defaults") or {}
            bounded = (
                defaults.get("max_visits") is not None
                or target_step.get("max_visits") is not None
                or step.get("max_visits") is not None
            )
            if not bounded:
                add(findings, "warning", "SFH042", target, f"loop through '{target}' relies on the implicit visit default; set max_visits on '{target}' or on '{label}'")
            if target_step.get("on_max_visits") is None and step.get("on_max_visits") is None:
                add(findings, "warning", "SFH043", target, f"loop through '{target}' has no explicit on_max_visits handoff/stuck policy on either '{target}' or '{label}'")

        for source in step.get("context") or []:
            if source not in (flow.get("contexts") or {}):
                add(findings, "error", "SFH050", label, f"undefined context: {source}")

    writers = [(label, effect(step, flow)) for step, label, _ in walk(flow) if effect(step, flow) != "read"]
    if writers and "workspace" not in flow:
        add(findings, "warning", "SFH060", "<flow>", "potential writers exist but workspace is implicit caller cwd")

    for step in steps:
        children = [c for c in step.get("parallel") or [] if isinstance(c, dict)]
        child_writers = [c.get("id", "?") for c in children if effect(c, flow) != "read"]
        if len(child_writers) > 1 and not ((flow.get("workspace") or {}).get("allow_concurrent_writers")):
            add(findings, "error", "SFH061", str(step.get("id")), f"parallel potential writers share one workspace: {', '.join(map(str, child_writers))}")
        if step.get("foreach") and effect(step, flow) != "read":
            cap = step.get("max_parallel", (flow.get("defaults") or {}).get("max_parallel", 4))
            if isinstance(cap, int) and cap > 1:
                add(findings, "error", "SFH062", str(step.get("id")), "writing foreach has max_parallel > 1 in one workspace")

    contexts = flow.get("contexts") or {}
    for name, source in contexts.items():
        if isinstance(source, dict) and isinstance(source.get("template"), str):
            text = source["template"]
            includes_body = re.search(r"steps\.[A-Za-z0-9_-]+\.output(?!_file)", text) is not None
            if includes_body and not any(f in text for f in ("truncate:", "tail:", "head:")):
                add(findings, "warning", "SFH070", f"contexts.{name}", "upstream output body is included without an explicit bound")

    order = {"error": 0, "warning": 1, "info": 2}
    findings.sort(key=lambda x: (order.get(x["severity"], 9), x["code"], x["step"]))
    result = {
        "ok": not any(x["severity"] == "error" for x in findings),
        "flow": str(args.flow),
        "findings": findings,
        "note": "heuristic lint only; run sfh validate --strict, preflight, and plan",
    }
    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        for item in findings:
            print(f"{item['severity'].upper():7} {item['code']} {item['step']}: {item['message']}")
        print(f"{len(findings)} finding(s). Run sfh validate --strict next.")
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
