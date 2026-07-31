#!/usr/bin/env python3
"""Regression test: a CI gate depends on neither ambient env nor an optional dep.

Two gates were red on main for the same underlying reason, and both were green
on a developer laptop, which is why neither was noticed from local runs:

  1. `signature-parity` ran `node runners/signature_test.mjs`, which imports the
     host-pure signatureOf/descriptorOf from each runner bundle. The React
     Native bundle imported `webdriverio` at TOP LEVEL, so loading those pure
     functions required the Appium driver. That job installs no npm packages at
     all, so it failed with "Cannot find package 'webdriverio'". Locally it
     passed only because runners/rn/node_modules happened to exist.

  2. `sdk-backend-core` ran the Python SDK suite, which pins an exact
     `deployment` shape. `Capture.resolve_commit` deliberately falls back to
     REPROIT_COMMIT and then GITHUB_SHA, and GITHUB_SHA is always set on a
     GitHub runner, so the batch carried an extra `commit` key in CI and none
     locally.

The shared rule: a gate must state the environment it needs rather than inherit
it, and must not require an optional dependency to load code that does not use
it. This check is deliberately structural, in the same spirit as
check-harness-integrity.py: it cannot prove either gate is correct, it refuses
the exact shapes that already cost this project two silently red jobs.
"""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

RN_BUNDLE = ROOT / "runners/rn/runner.mjs"
PY_CONFTEST = ROOT / "sdk/reproit-backend-py/tests/conftest.py"
WORKFLOW = ROOT / ".github/workflows/native-gates.yml"
AMBIENT_CODE_IDENTITY = ("REPROIT_COMMIT", "GITHUB_SHA")

# A static ESM import of webdriverio, e.g. `import{remote as Yt}from"webdriverio"`.
STATIC_WEBDRIVERIO = re.compile(r"(?<!await )\bimport\s*\{[^}]*\}\s*from\s*[\"']webdriverio[\"']")
DYNAMIC_WEBDRIVERIO = re.compile(r"import\s*\(\s*[\"']webdriverio[\"']\s*\)")


class GateEnvironmentIndependenceTests(unittest.TestCase):
    def test_the_rn_bundle_loads_without_the_appium_driver(self) -> None:
        bundle = RN_BUNDLE.read_text(encoding="utf-8", errors="replace")
        self.assertIsNone(
            STATIC_WEBDRIVERIO.search(bundle),
            "runners/rn/runner.mjs imports webdriverio statically, so its pure "
            "signature exports cannot load without the Appium driver and the "
            "signature-parity job (which installs no npm packages) fails",
        )

    def test_the_rn_bundle_still_reaches_the_driver_when_it_needs_one(self) -> None:
        # Guards the obvious wrong fix: deleting the import instead of deferring
        # it, which would pass the check above and break every real Appium run.
        bundle = RN_BUNDLE.read_text(encoding="utf-8", errors="replace")
        self.assertIsNotNone(
            DYNAMIC_WEBDRIVERIO.search(bundle),
            "runners/rn/runner.mjs no longer imports webdriverio at all; the "
            "session path needs it, so the static import must become a dynamic "
            "one, not disappear",
        )

    def test_the_python_capture_suite_clears_ambient_code_identity(self) -> None:
        self.assertTrue(
            PY_CONFTEST.is_file(),
            "sdk/reproit-backend-py/tests/conftest.py is missing, so the suite "
            "inherits GITHUB_SHA from the runner and its exact-shape deployment "
            "assertions fail in CI while passing locally",
        )
        conftest = PY_CONFTEST.read_text(encoding="utf-8", errors="replace")
        for name in AMBIENT_CODE_IDENTITY:
            self.assertIn(
                name,
                conftest,
                f"the conftest does not neutralize {name}, which the SDK reads "
                "as code identity",
            )
        self.assertIn("delenv", conftest, "the conftest names the variables but never clears them")

    def test_self_hosted_jobs_are_opt_in_so_a_push_can_conclude(self) -> None:
        # The third instance of this class, and the worst, because it produced no
        # red at all. `windows-uia` required [self-hosted, reproit-windows-bridge]
        # but ran on every push, and no self-hosted runner is registered, so the
        # job sat queued and native-backend-gates never reached a conclusion: 12
        # consecutive runs were still "queued" hours later. A gate that can never
        # report is indistinguishable from one that is merely slow, so nobody
        # learns it is dead. Any job pinned to a runner that is only online on
        # demand must be workflow_dispatch-gated.
        for job, body in _jobs(WORKFLOW.read_text(encoding="utf-8", errors="replace")):
            if "self-hosted" not in body:
                continue
            condition = _condition(body)
            self.assertIsNotNone(
                condition,
                f"job {job} needs a self-hosted runner but has no `if:`, so every "
                "push queues forever and the workflow never concludes",
            )
            self.assertIn(
                "workflow_dispatch",
                condition,
                f"job {job} needs a self-hosted runner but its condition "
                f"({condition!r}) still lets a push schedule it",
            )
            self.assertNotIn(
                "!=",
                condition,
                f"job {job} uses `event_name != 'workflow_dispatch'`, which runs "
                "it on exactly the events that have no runner and skips it on the "
                "one that does; this is the inverted form that caused the outage",
            )


def _jobs(text: str) -> list[tuple[str, str]]:
    """Split the `jobs:` mapping into (name, body) at two-space indentation."""
    jobs, name, lines = [], None, []
    for line in text.splitlines():
        header = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if header:
            if name is not None:
                jobs.append((name, "\n".join(lines)))
            name, lines = header.group(1), []
        elif name is not None:
            lines.append(line)
    if name is not None:
        jobs.append((name, "\n".join(lines)))
    return jobs


def _condition(body: str) -> str | None:
    match = re.search(r"^\s{4}if:\s*(.+)$", body, re.MULTILINE)
    return match.group(1).strip() if match else None


def main() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(GateEnvironmentIndependenceTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    # Case accounting: an empty or short run is the failure this file exists for.
    if result.testsRun != 4:
        print(f"expected 4 cases, ran {result.testsRun}", file=sys.stderr)
        return 1
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
