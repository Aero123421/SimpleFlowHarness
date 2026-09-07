#!/usr/bin/env python3
"""Behavior checks for the bundled GitHub Actions polling helper."""

from __future__ import annotations

import json
import contextlib
import importlib.util
import io
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills/sfh-ci-monitoring/scripts/gh_ci_gate.py"

_SPEC = importlib.util.spec_from_file_location("gh_ci_gate", SCRIPT)
assert _SPEC is not None and _SPEC.loader is not None
GATE = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(GATE)


class GhCiGateChecks(unittest.TestCase):
    def run_gate(
        self,
        *extra: str,
        timeout: float = 5,
    ) -> tuple[subprocess.CompletedProcess[str], float]:
        command = [
            sys.executable,
            str(SCRIPT),
            "watch",
            "--repo",
            "owner/repo",
            "--run-id",
            "123",
            *extra,
        ]
        started = time.monotonic()
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        return result, time.monotonic() - started

    @staticmethod
    def payload(result: subprocess.CompletedProcess[str]) -> dict:
        return json.loads(result.stdout)

    def run_watch_mocked(
        self,
        response: str | bytes,
        *extra: str,
        monotonic: list[float],
        run_side_effect=None,
        interval: float = 0.01,
        watch_timeout: float = 0.1,
    ) -> tuple[int, dict, mock.Mock, mock.Mock]:
        args = GATE.argparse.Namespace(
            repo="owner/repo",
            run_id="123",
            expected_sha="",
            interval=interval,
            timeout=watch_timeout,
            output=None,
        )
        if extra:
            args.expected_sha = extra[-1]
        completed = subprocess.CompletedProcess(
            ["gh"], 0, stdout=response, stderr=""
        )
        fake_run = mock.Mock(
            side_effect=run_side_effect
            if run_side_effect is not None
            else [completed]
        )
        fake_sleep = mock.Mock()
        stdout = io.StringIO()
        with (
            mock.patch.object(GATE.subprocess, "run", fake_run),
            mock.patch.object(GATE.time, "monotonic", side_effect=monotonic),
            mock.patch.object(GATE.time, "sleep", fake_sleep),
            contextlib.redirect_stdout(stdout),
        ):
            code = GATE.watch(args)
        return code, json.loads(stdout.getvalue()), fake_run, fake_sleep

    def test_missing_gh_is_a_transient_json_failure(self) -> None:
        code, payload, _, _ = self.run_watch_mocked(
            "", monotonic=[0, 0], run_side_effect=FileNotFoundError("gh")
        )
        self.assertEqual(code, 10)
        self.assertEqual(payload["kind"], "ci_api_transient")

    def test_non_utf8_gh_output_is_a_transient_json_failure(self) -> None:
        error = UnicodeDecodeError("utf-8", b"\xff", 0, 1, "invalid byte")
        code, payload, _, _ = self.run_watch_mocked(
            "", monotonic=[0, 0], run_side_effect=error
        )
        self.assertEqual(code, 10)
        self.assertEqual(payload["kind"], "ci_api_transient")

    def test_non_utf8_payload_bytes_are_a_transient_json_failure(self) -> None:
        code, payload, _, _ = self.run_watch_mocked(
            b"\xff", monotonic=[0, 0]
        )
        self.assertEqual(code, 10)
        self.assertEqual(payload["kind"], "ci_api_transient")

    def test_hung_gh_is_cut_off_at_the_watch_deadline(self) -> None:
        code, payload, fake_run, _ = self.run_watch_mocked(
            '{"id":123,"head_sha":"abc","status":"in_progress","conclusion":null}',
            "--expected-sha",
            "abc",
            monotonic=[0, 0],
            run_side_effect=subprocess.TimeoutExpired(["gh"], 0.1),
        )
        self.assertEqual(code, 40)
        self.assertEqual(payload["kind"], "ci_watch_timeout")
        self.assertAlmostEqual(fake_run.call_args.kwargs["timeout"], 0.1, places=2)

    def test_real_hung_gh_child_is_cut_off_at_the_watch_deadline(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sleeper = Path(directory) / "hung_child.py"
            sleeper.write_text("import time\ntime.sleep(30)\n", encoding="utf-8")
            real_run = subprocess.run
            child_timeout: list[float] = []

            def run_hung(command, *args, **kwargs):
                self.assertEqual(command[0], "gh")
                child_timeout.append(kwargs["timeout"])
                return real_run(
                    [sys.executable, str(sleeper)], *args, **kwargs
                )

            args = GATE.argparse.Namespace(
                repo="owner/repo",
                run_id="123",
                expected_sha="",
                interval=0.01,
                timeout=0.1,
                output=None,
            )
            stdout = io.StringIO()
            started = time.monotonic()
            with (
                mock.patch.object(GATE.subprocess, "run", run_hung),
                contextlib.redirect_stdout(stdout),
            ):
                code = GATE.watch(args)
            elapsed = time.monotonic() - started

        payload = json.loads(stdout.getvalue())
        self.assertEqual(code, 40)
        self.assertEqual(payload["kind"], "ci_watch_timeout")
        self.assertEqual(len(child_timeout), 1)
        self.assertGreater(child_timeout[0], 0)
        self.assertLessEqual(child_timeout[0], args.timeout)
        self.assertLess(elapsed, 2)

    def test_delayed_success_after_deadline_cannot_pass(self) -> None:
        code, payload, _, _ = self.run_watch_mocked(
            '{"id":123,"head_sha":"abc","status":"completed","conclusion":"success"}',
            "--expected-sha",
            "abc",
            monotonic=[0, 0.01, 0.2],
        )
        self.assertEqual(code, 40)
        self.assertEqual(payload["kind"], "ci_watch_timeout")

    def test_poll_sleep_is_cut_off_at_the_watch_deadline(self) -> None:
        code, _, _, fake_sleep = self.run_watch_mocked(
            '{"id":123,"head_sha":"abc","status":"in_progress","conclusion":null}',
            "--expected-sha",
            "abc",
            monotonic=[0, 0.01, 0.02, 0.2],
            interval=1,
        )
        self.assertEqual(code, 40)
        self.assertLessEqual(fake_sleep.call_args.args[0], 0.1)

    def test_nan_and_infinity_are_rejected(self) -> None:
        for option, value in (("--interval", "nan"), ("--timeout", "inf")):
            with self.subTest(option=option):
                result, _ = self.run_gate(
                    option,
                    value,
                )
                self.assertEqual(result.returncode, 2)
                self.assertIn("finite", result.stderr)

    def test_malformed_status_and_conclusion_types_are_protocol_failures(self) -> None:
        responses = (
            '{"id":123,"head_sha":"abc","status":[],"conclusion":null}',
            '{"id":123,"head_sha":"abc","status":"completed","conclusion":[]}',
            '{"id":123,"head_sha":[],"status":"completed","conclusion":"success"}',
        )
        for response in responses:
            with self.subTest(response=response):
                code, payload, _, _ = self.run_watch_mocked(
                    response, monotonic=[0, 0.01]
                )
                self.assertEqual(code, 30)
                self.assertEqual(payload["kind"], "ci_identity_or_protocol_error")

    def test_optional_expected_sha_still_allows_run_id_only_watch(self) -> None:
        code, payload, _, _ = self.run_watch_mocked(
            '{"id":123,"head_sha":"abc","status":"completed","conclusion":"success"}',
            monotonic=[0, 0, 0.01],
        )
        self.assertEqual(code, 0)
        self.assertEqual(payload["kind"], "ci_passed")


if __name__ == "__main__":
    unittest.main()
