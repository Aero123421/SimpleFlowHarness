#!/usr/bin/env python3
"""Poll one exact GitHub Actions run and map it to stable exit codes.

Exit codes:
  0  completed successfully
  10 transient `gh api`/JSON acquisition failure
  20 completed with a non-success conclusion
  30 identity mismatch or unknown protocol/status
  40 watch timeout while still queued/running

The script never reruns, cancels, triggers, or mutates a workflow.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

RUNNING = {"queued", "in_progress", "waiting", "pending", "requested"}
SUCCESS = {"success"}
KNOWN_FAILURE = {
    "failure",
    "cancelled",
    "timed_out",
    "action_required",
    "stale",
    "startup_failure",
    "skipped",
    "neutral",
}


def emit(payload: dict[str, Any], output: Path | None) -> None:
    text = json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    if output is not None:
        output.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(prefix=output.name + ".", dir=output.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as f:
                f.write(text)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, output)
        finally:
            try:
                os.unlink(tmp)
            except FileNotFoundError:
                pass
    sys.stdout.write(text)
    sys.stdout.flush()


def fetch(repo: str, run_id: str) -> tuple[dict[str, Any] | None, str | None]:
    proc = subprocess.run(
        ["gh", "api", f"repos/{repo}/actions/runs/{run_id}"],
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        return None, proc.stderr.strip() or f"gh api exited {proc.returncode}"
    try:
        value = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        return None, f"GitHub API response was not JSON: {exc}"
    if not isinstance(value, dict):
        return None, "GitHub API response was not an object"
    return value, None


def watch(args: argparse.Namespace) -> int:
    deadline = time.monotonic() + args.timeout
    output = Path(args.output).resolve() if args.output else None
    last_state: tuple[str, object] | None = None

    while True:
        value, error = fetch(args.repo, args.run_id)
        now = int(time.time())
        if error is not None or value is None:
            payload = {
                "schema_version": 1,
                "kind": "ci_api_transient",
                "repo": args.repo,
                "run_id": args.run_id,
                "expected_sha": args.expected_sha,
                "observed_utc_epoch": now,
                "error": error,
            }
            emit(payload, output)
            return 10

        observed_id = str(value.get("id", ""))
        observed_sha = str(value.get("head_sha", ""))
        status = str(value.get("status", ""))
        conclusion = value.get("conclusion")
        payload = {
            "schema_version": 1,
            "kind": "github_actions_run",
            "repo": args.repo,
            "run_id": args.run_id,
            "observed_id": observed_id,
            "expected_sha": args.expected_sha,
            "head_sha": observed_sha,
            "status": status,
            "conclusion": conclusion,
            "run_attempt": value.get("run_attempt"),
            "workflow_id": value.get("workflow_id"),
            "name": value.get("name"),
            "event": value.get("event"),
            "html_url": value.get("html_url"),
            "created_at": value.get("created_at"),
            "updated_at": value.get("updated_at"),
            "observed_utc_epoch": now,
        }

        if observed_id != str(args.run_id):
            payload["kind"] = "ci_identity_or_protocol_error"
            payload["error"] = "response run id does not match requested run id"
            emit(payload, output)
            return 30
        if args.expected_sha and observed_sha.lower() != args.expected_sha.lower():
            payload["kind"] = "ci_identity_or_protocol_error"
            payload["error"] = "head_sha does not match the intended commit"
            emit(payload, output)
            return 30

        state = (status, conclusion)
        if state != last_state:
            print(f"sfh-ci: run {args.run_id} status={status} conclusion={conclusion}", file=sys.stderr)
            last_state = state

        if status in RUNNING:
            if time.monotonic() >= deadline:
                payload["kind"] = "ci_watch_timeout"
                payload["error"] = "deadline reached before the run completed"
                emit(payload, output)
                return 40
            time.sleep(args.interval)
            continue

        if status == "completed":
            if conclusion in SUCCESS:
                payload["kind"] = "ci_passed"
                emit(payload, output)
                return 0
            if conclusion in KNOWN_FAILURE:
                payload["kind"] = "ci_failed"
                emit(payload, output)
                return 20
            payload["kind"] = "ci_identity_or_protocol_error"
            payload["error"] = f"unknown completed conclusion: {conclusion!r}"
            emit(payload, output)
            return 30

        payload["kind"] = "ci_identity_or_protocol_error"
        payload["error"] = f"unknown workflow run status: {status!r}"
        emit(payload, output)
        return 30


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("watch", help="poll one exact run until terminal or timeout")
    p.add_argument("--repo", required=True, help="OWNER/REPO")
    p.add_argument("--run-id", required=True)
    p.add_argument("--expected-sha", default="")
    p.add_argument("--interval", type=float, default=10.0)
    p.add_argument("--timeout", type=float, default=3600.0)
    p.add_argument("--output", help="optional atomic JSON snapshot path")
    p.set_defaults(func=watch)
    args = parser.parse_args()
    if args.interval <= 0 or args.timeout <= 0:
        parser.error("--interval and --timeout must be > 0")
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
