#!/usr/bin/env python3
"""Regression tests for the self-dogfood bug-fix declaration policy."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("check-fix-policy.py")
WORKFLOW = SCRIPT.parents[2] / ".github/workflows/ci.yml"
SPEC = importlib.util.spec_from_file_location("check_fix_policy", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {SCRIPT}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

GUARD = "rep_b1ab0f0eb617"
RAW_GUARD = "b1ab0f0eb617"


class FixPolicyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.repo = Path(self.temporary.name)
        self.git("init", "--initial-branch=main")
        self.git("config", "user.email", "gate@reproit.test")
        self.git("config", "user.name", "Gate")
        self.write("README.md", "base\n")
        self.commit("Base commit")
        self.base = self.git("rev-parse", "HEAD").strip()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def git(self, *args: str) -> str:
        result = subprocess.run(
            ["git", "-C", str(self.repo), *args],
            capture_output=True,
            check=True,
            text=True,
        )
        return result.stdout

    def write(self, path: str, text: str) -> None:
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self, message: str) -> None:
        self.git("add", "--all")
        self.git("commit", "--quiet", "--message", message)

    def write_guard(self, status: str = "required", signature: str = "cli:x") -> None:
        self.write(
            f".reproit/repros/{RAW_GUARD}/meta.json",
            json.dumps({"id": RAW_GUARD, "status": status, "trigger_sig": signature}),
        )

    def write_exception(self, identifier: str, code: str) -> None:
        self.write(
            f"validation/self-dogfood/exceptions/{identifier}.json",
            json.dumps(
                {
                    "schemaVersion": 1,
                    "id": identifier,
                    "code": code,
                    "detail": "the vendor SDK cannot be installed offline",
                    "issue": "DOGFOOD-042",
                    "missingCapability": "offline vendor SDK acquisition",
                    "retainedEvidence": ["validation/self-dogfood/evidence/x.log"],
                }
            ),
        )

    def review(self) -> dict[str, object]:
        return MODULE.review(self.repo, self.base, "HEAD")

    def test_source_change_without_a_declaration_fails(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit("Fix a dispatch defect")
        with self.assertRaisesRegex(MODULE.PolicyError, "exactly one"):
            self.review()

    def test_two_declarations_are_ambiguous_and_fail(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit("Fix\n\nReproit-Dogfood: not-a-fix\nReproit-Dogfood: not-a-fix\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "exactly one"):
            self.review()

    def test_every_commit_in_a_multi_commit_push_is_reviewed(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn one() {}\n")
        self.commit("Change one\n\nReproit-Dogfood: not-a-fix\n")
        self.write("crates/reproit/src/lib.rs", "fn two() {}\n")
        self.commit("Fix two without a declaration")

        with self.assertRaisesRegex(MODULE.PolicyError, "exactly one"):
            self.review()

    def test_documentation_only_changes_need_no_declaration(self) -> None:
        self.write("docs/guide.md", "text\n")
        self.commit("Document the guard policy")
        report = self.review()
        self.assertEqual(report["declared"], [])
        self.assertEqual(report["commits"], 1)

    def test_a_required_guard_declaration_is_accepted(self) -> None:
        self.write_guard()
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit(f"Fix a dispatch defect\n\nReproit-Dogfood: guard:{GUARD}\n")
        report = self.review()
        self.assertEqual(report["declared"][0]["declaration"]["guard"], GUARD)

    def test_a_guard_declaration_without_that_guard_fails(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit(f"Fix\n\nReproit-Dogfood: guard:{GUARD}\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "not a required guard"):
            self.review()

    def test_a_quarantined_guard_cannot_satisfy_a_declaration(self) -> None:
        self.write_guard(status="quarantined")
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit(f"Fix\n\nReproit-Dogfood: guard:{GUARD}\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "not a required guard"):
            self.review()

    def test_a_typed_exception_requires_a_retained_record(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit(
            "Fix\n\nReproit-Dogfood: exception:unsupported-capability:vendor-sdk\n"
        )
        with self.assertRaisesRegex(MODULE.PolicyError, "is missing"):
            self.review()

        self.write_exception("vendor-sdk", "unsupported-capability")
        self.write("crates/reproit/src/other.rs", "fn other() {}\n")
        self.commit(
            "Fix again\n\nReproit-Dogfood: exception:unsupported-capability:vendor-sdk\n"
        )
        report = MODULE.review(self.repo, "HEAD~1", "HEAD")
        self.assertEqual(report["declared"][0]["declaration"]["id"], "vendor-sdk")

    def test_an_untyped_exception_code_is_rejected(self) -> None:
        self.write_exception("vendor-sdk", "made-up")
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit("Fix\n\nReproit-Dogfood: exception:made-up:vendor-sdk\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "not one of"):
            self.review()

    def test_no_repro_requires_the_test_to_land_with_the_fix(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit("Fix\n\nReproit-Dogfood: no-repro:crates/reproit/tests/x.rs\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "does not change"):
            self.review()

    def test_no_repro_with_the_regression_test_is_accepted(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.write("crates/reproit/tests/x.rs", "#[test] fn t() {}\n")
        self.commit("Fix\n\nReproit-Dogfood: no-repro:crates/reproit/tests/x.rs\n")
        report = self.review()
        self.assertEqual(
            report["declared"][0]["declaration"]["test"], "crates/reproit/tests/x.rs"
        )

    def test_deleting_a_required_guard_fails_the_gate(self) -> None:
        self.write_guard()
        self.write("docs/a.md", "a\n")
        self.commit("Add the guard")
        self.base = self.git("rev-parse", "HEAD").strip()
        self.git("rm", "-r", "--quiet", f".reproit/repros/{RAW_GUARD}")
        self.commit("Fix\n\nReproit-Dogfood: not-a-fix\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "weakened"):
            self.review()

    def test_changing_a_guard_trigger_signature_fails_the_gate(self) -> None:
        self.write_guard()
        self.commit("Add the guard")
        self.base = self.git("rev-parse", "HEAD").strip()
        self.write_guard(signature="cli:something-else")
        self.commit("Weaken\n\nReproit-Dogfood: not-a-fix\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "weakened"):
            self.review()

    def test_a_declared_retirement_with_a_record_is_accepted(self) -> None:
        self.write_guard()
        self.commit("Add the guard")
        self.base = self.git("rev-parse", "HEAD").strip()
        self.git("rm", "-r", "--quiet", f".reproit/repros/{RAW_GUARD}")
        self.write(
            f"validation/self-dogfood/retirements/{GUARD}.json",
            json.dumps(
                {
                    "schemaVersion": 1,
                    "guard": GUARD,
                    "reason": "the defect class is now covered by an exact oracle",
                    "replacement": "rep_40f619ef4a2c",
                }
            ),
        )
        self.commit(
            f"Retire the guard\n\nReproit-Dogfood: not-a-fix\n"
            f"Reproit-Guard-Retire: {GUARD}\n"
        )
        report = self.review()
        self.assertEqual(report["retiredGuards"], [GUARD])
        self.assertEqual(len(report["weakenedGuards"]), 1)

    def test_a_retirement_without_a_record_still_fails(self) -> None:
        self.write_guard()
        self.commit("Add the guard")
        self.base = self.git("rev-parse", "HEAD").strip()
        self.git("rm", "-r", "--quiet", f".reproit/repros/{RAW_GUARD}")
        self.commit(
            f"Retire\n\nReproit-Dogfood: not-a-fix\nReproit-Guard-Retire: {GUARD}\n"
        )
        with self.assertRaisesRegex(MODULE.PolicyError, "retirement record"):
            self.review()

    def test_an_unknown_declaration_kind_is_rejected(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit("Fix\n\nReproit-Dogfood: trust-me\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "unknown declaration"):
            self.review()


class FixPolicyWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = WORKFLOW.read_text(encoding="utf-8")
        start = self.workflow.index("  dogfood-policy:")
        end = self.workflow.index("\n  windows-build:", start)
        self.job = self.workflow[start:end]

    def test_policy_runs_for_pull_requests_and_pushes(self) -> None:
        self.assertIn(
            "if: github.event_name == 'pull_request' || "
            "github.event_name == 'push'",
            self.job,
        )

    def test_policy_uses_immutable_event_ranges_with_full_history(self) -> None:
        for expression in (
            "github.event.pull_request.base.sha",
            "github.event.pull_request.head.sha",
            "github.event.before",
            "github.event.after",
            "fetch-depth: 0",
            '--base "$POLICY_BASE"',
            '--head "$POLICY_HEAD"',
        ):
            with self.subTest(expression=expression):
                self.assertIn(expression, self.job)
        self.assertNotIn("github.base_ref", self.job)


if __name__ == "__main__":
    unittest.main()
