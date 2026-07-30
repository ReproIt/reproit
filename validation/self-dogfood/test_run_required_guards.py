#!/usr/bin/env python3
"""Regression tests for required self-dogfood guard replay."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).with_name("run-required-guards.py")
SPEC = importlib.util.spec_from_file_location("run_required_guards", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {SCRIPT}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class RequiredGuardReplayTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.repo = Path(self.temporary.name)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_guard(self, raw_id: str, status: str) -> None:
        directory = self.repo / ".reproit/repros" / raw_id
        directory.mkdir(parents=True)
        (directory / "meta.json").write_text(
            json.dumps({"id": raw_id, "status": status}),
            encoding="utf-8",
        )

    def test_only_required_guards_are_selected_in_content_id_order(self) -> None:
        self.write_guard("bbbbbbbbbbbb", "required")
        self.write_guard("aaaaaaaaaaaa", "quarantined")
        self.write_guard("111111111111", "required")

        self.assertEqual(
            MODULE.required_guard_references(self.repo),
            ["rep_111111111111", "rep_bbbbbbbbbbbb"],
        )

    def test_an_empty_required_corpus_fails(self) -> None:
        self.write_guard("aaaaaaaaaaaa", "quarantined")

        with self.assertRaisesRegex(MODULE.CorpusError, "no required guards"):
            MODULE.required_guard_references(self.repo)

    def test_invalid_guard_metadata_cannot_be_silently_skipped(self) -> None:
        self.write_guard("aaaaaaaaaaaa", "typo")

        with self.assertRaisesRegex(MODULE.CorpusError, "invalid status"):
            MODULE.required_guard_references(self.repo)

    def test_guard_directory_without_metadata_cannot_be_silently_skipped(self) -> None:
        directory = self.repo / ".reproit/repros/aaaaaaaaaaaa"
        directory.mkdir(parents=True)

        with self.assertRaisesRegex(MODULE.CorpusError, "missing meta.json"):
            MODULE.required_guard_references(self.repo)

    def test_replay_is_explicit_strict_and_runs_every_guard_three_times(self) -> None:
        guards = ["rep_111111111111", "rep_bbbbbbbbbbbb"]
        completed = [
            mock.Mock(returncode=1),
            mock.Mock(returncode=0),
        ]

        with (
            mock.patch.object(MODULE.subprocess, "run", side_effect=completed) as run,
            mock.patch.object(MODULE.sys, "stderr"),
        ):
            result = MODULE.replay_required_guards(self.repo, "reproit", guards)

        self.assertEqual(result, 1)
        self.assertEqual(run.call_count, 2)
        for call, guard in zip(run.call_args_list, guards, strict=True):
            self.assertEqual(
                call.args[0],
                [
                    "reproit",
                    "--json",
                    "--yes",
                    "check",
                    guard,
                    "--strict",
                    "--runs",
                    "3",
                ],
            )
            self.assertEqual(
                call.kwargs,
                {
                    "cwd": self.repo,
                    "check": False,
                    "timeout": MODULE.REPLAY_TIMEOUT_SECONDS,
                },
            )

    def test_a_timed_out_guard_fails_but_does_not_stop_the_corpus(self) -> None:
        guards = ["rep_111111111111", "rep_bbbbbbbbbbbb"]
        completed = [
            MODULE.subprocess.TimeoutExpired("reproit", MODULE.REPLAY_TIMEOUT_SECONDS),
            mock.Mock(returncode=0),
        ]

        with (
            mock.patch.object(MODULE.subprocess, "run", side_effect=completed) as run,
            mock.patch.object(MODULE.sys, "stderr") as stderr,
        ):
            result = MODULE.replay_required_guards(self.repo, "reproit", guards)

        self.assertEqual(result, 1)
        self.assertEqual(run.call_count, 2)
        self.assertIn("rep_111111111111 (timed out)", stderr.write.call_args.args[0])


if __name__ == "__main__":
    unittest.main()
