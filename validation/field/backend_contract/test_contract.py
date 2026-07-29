#!/usr/bin/env python3
"""Contract tests for the bounded backend field campaign."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent
RUNNER_PATH = ROOT / "run_campaign.py"
SPEC = importlib.util.spec_from_file_location("backend_field_runner", RUNNER_PATH)
assert SPEC is not None and SPEC.loader is not None
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class BackendFieldContractTests(unittest.TestCase):
    def test_revision_pairs_are_full_and_exact(self) -> None:
        self.assertEqual(
            RUNNER.REVISIONS["gitea"]["affected"],
            "98c61942aa433342eacf08e4040ded80b1d0efe1",
        )
        self.assertEqual(
            RUNNER.REVISIONS["gitea"]["fixed"],
            "4812e354866a066dcb899af667b0fad5fa094065",
        )
        self.assertEqual(
            RUNNER.REVISIONS["memos"]["affected"],
            "14fb38f37560541bf2719647e7e8b1468937f8ef",
        )
        self.assertEqual(
            RUNNER.REVISIONS["memos"]["fixed"],
            "7c3fcc297d8e5a955d9c0bc4f3ca917854132e8e",
        )

    def test_probe_image_is_digest_pinned(self) -> None:
        self.assertRegex(RUNNER.CURL_IMAGE, r"@sha256:[0-9a-f]{64}$")

    def test_container_is_read_only_and_loopback_bound(self) -> None:
        arguments = RUNNER.container_arguments(
            "memos",
            "service",
            "network",
            "image",
            Path("/owned/data"),
            "session",
        )
        self.assertIn("--read-only", arguments)
        self.assertIn("127.0.0.1::5230", arguments)
        self.assertIn("type=bind,src=/owned/data,dst=/var/opt/memos", arguments)

    def test_curl_parser_handles_normalized_header_lines(self) -> None:
        output = (
            "HTTP/1.1 200 OK\n"
            "X-Total-Count: 4\n"
            "Content-Type: application/json\n\n"
            "[{}]\n"
            "__REPROIT_STATUS__:200"
        )
        with mock.patch.object(RUNNER, "execute", return_value=output):
            status, headers, body = RUNNER.curl_request(
                "network",
                "http://service/path",
            )
        self.assertEqual(status, 200)
        self.assertEqual(headers["x-total-count"], "4")
        self.assertEqual(body, "[{}]")

    def test_cleanup_is_unconditional(self) -> None:
        source = RUNNER_PATH.read_text(encoding="utf-8")
        self.assertIn('best_effort(["docker", "rm", "-f", name])', source)
        self.assertIn('best_effort(["docker", "network", "rm", network])', source)
        self.assertIn("shutil.rmtree(runtime_root, ignore_errors=True)", source)

    def test_cleanup_timeout_does_not_skip_later_cleanup(self) -> None:
        with mock.patch.object(
            RUNNER.subprocess,
            "run",
            side_effect=RUNNER.subprocess.TimeoutExpired(["docker"], 60),
        ):
            RUNNER.best_effort(["docker", "rm", "-f", "owned"])

    def test_timed_out_absence_check_fails_closed(self) -> None:
        with mock.patch.object(
            RUNNER.subprocess,
            "run",
            side_effect=RUNNER.subprocess.TimeoutExpired(["docker"], 30),
        ):
            self.assertFalse(RUNNER.resource_absent(["docker", "inspect", "owned"]))


if __name__ == "__main__":
    unittest.main()
