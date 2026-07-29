#!/usr/bin/env python3
"""Regression tests for the self-dogfood bug-fix declaration policy."""

from __future__ import annotations

import importlib.util
import hashlib
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
REPLACEMENT_GUARD = "rep_40f619ef4a2c"
RAW_REPLACEMENT_GUARD = "40f619ef4a2c"


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

    def evidence(self, path: str) -> dict[str, str]:
        digest = hashlib.sha256((self.repo / path).read_bytes()).hexdigest()
        return {"path": path, "sha256": f"sha256:{digest}"}

    def write_exception(self, identifier: str, code: str) -> None:
        evidence_path = f"validation/self-dogfood/evidence/{identifier}.log"
        self.write(evidence_path, "retained blocker evidence\n")
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
                    "retainedEvidence": [self.evidence(evidence_path)],
                }
            ),
        )

    def write_not_a_fix(
        self,
        identifier: str,
        evidence_path: str,
        change_type: str = "maintenance",
    ) -> None:
        self.write(
            f"validation/self-dogfood/not-a-fix/{identifier}.json",
            json.dumps(
                {
                    "schemaVersion": 1,
                    "id": identifier,
                    "changeType": change_type,
                    "detail": "this change adds behavior and does not correct a defect",
                    "evidence": [self.evidence(evidence_path)],
                }
            ),
        )

    def write_no_repro(
        self,
        identifier: str,
        test_path: str,
        test_source: str = (
            "from pathlib import Path\n"
            "source = Path('crates/reproit/src/lib.rs')\n"
            "raise SystemExit(0 if source.is_file() else 1)\n"
        ),
    ) -> None:
        evidence_path = f"validation/self-dogfood/evidence/{identifier}.log"
        self.write(evidence_path, "affected revision exited 1\n")
        self.write(test_path, test_source)
        self.write(
            f"validation/self-dogfood/no-repro/{identifier}.json",
            json.dumps(
                {
                    "schemaVersion": 1,
                    "id": identifier,
                    "detail": "the independent unit test is the stable authority",
                    "test": test_path,
                    "command": ["python3", test_path],
                    "timeoutSeconds": 30,
                    "affectedExitCode": 1,
                    "affectedEvidence": [self.evidence(evidence_path)],
                }
            ),
        )

    def review(self, *, execute_no_repro: bool = False) -> dict[str, object]:
        return MODULE.review(
            self.repo,
            self.base,
            "HEAD",
            execute_no_repro=execute_no_repro,
        )

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
        self.write_not_a_fix("change-one", "crates/reproit/src/lib.rs")
        self.commit("Change one\n\nReproit-Dogfood: not-a-fix:change-one\n")
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

    def test_workflow_build_and_additional_language_files_need_declarations(self) -> None:
        for path in (
            ".github/workflows/release.yml",
            "sdk/reproit-dotnet/Runtime.cs",
            "runners/native/bridge.cpp",
            "package.json",
        ):
            with self.subTest(path=path):
                self.write(path, "production\n")
                self.commit(f"Change {path}")
                with self.assertRaisesRegex(MODULE.PolicyError, "exactly one"):
                    MODULE.review(self.repo, "HEAD~1", "HEAD")

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

    def test_exception_evidence_digest_must_match(self) -> None:
        self.write_exception("vendor-sdk", "unsupported-capability")
        self.write("validation/self-dogfood/evidence/vendor-sdk.log", "tampered\n")
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit(
            "Fix\n\nReproit-Dogfood: exception:unsupported-capability:vendor-sdk\n"
        )
        with self.assertRaisesRegex(MODULE.PolicyError, "digest"):
            self.review()

    def test_no_repro_requires_a_changed_record_and_test(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.commit("Fix\n\nReproit-Dogfood: no-repro:dispatch-test\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "record"):
            self.review()

    def test_no_repro_executes_the_fixed_regression_test(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.write_no_repro("dispatch-test", "tests/dispatch_regression.py")
        self.commit("Fix\n\nReproit-Dogfood: no-repro:dispatch-test\n")
        report = self.review(execute_no_repro=True)
        self.assertEqual(
            report["declared"][0]["declaration"]["test"],
            "tests/dispatch_regression.py",
        )

    def test_no_repro_fails_when_the_fixed_regression_test_fails(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.write_no_repro(
            "dispatch-test",
            "tests/dispatch_regression.py",
            "raise SystemExit(2)\n",
        )
        self.commit("Fix\n\nReproit-Dogfood: no-repro:dispatch-test\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "fixed no-repro test exited 2"):
            self.review(execute_no_repro=True)

    def test_no_repro_must_fail_with_the_declared_result_on_the_parent(self) -> None:
        self.write("crates/reproit/src/lib.rs", "fn main() {}\n")
        self.write_no_repro("dispatch-test", "tests/dispatch_regression.py")
        record_path = (
            self.repo
            / "validation/self-dogfood/no-repro/dispatch-test.json"
        )
        record = json.loads(record_path.read_text(encoding="utf-8"))
        record["affectedExitCode"] = 2
        record_path.write_text(json.dumps(record), encoding="utf-8")
        self.commit("Fix\n\nReproit-Dogfood: no-repro:dispatch-test\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "expected 2"):
            self.review(execute_no_repro=True)

    def test_not_a_fix_requires_a_changed_evidence_record(self) -> None:
        self.write("crates/reproit/src/lib.rs", "pub fn feature() {}\n")
        self.commit("Feature\n\nReproit-Dogfood: not-a-fix\n")
        with self.assertRaisesRegex(MODULE.PolicyError, "not-a-fix id"):
            self.review()

        self.write("crates/reproit/src/feature.rs", "pub fn feature() {}\n")
        self.write_not_a_fix("new-feature", "crates/reproit/src/feature.rs", "feature")
        self.commit("Feature\n\nReproit-Dogfood: not-a-fix:new-feature\n")
        report = MODULE.review(self.repo, "HEAD~1", "HEAD")
        self.assertEqual(
            report["declared"][0]["declaration"]["changeType"], "feature"
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
            f".reproit/repros/{RAW_REPLACEMENT_GUARD}/meta.json",
            json.dumps(
                {
                    "id": RAW_REPLACEMENT_GUARD,
                    "status": "required",
                    "trigger_sig": "cli:replacement",
                }
            ),
        )
        self.write(
            f"validation/self-dogfood/retirements/{GUARD}.json",
            json.dumps(
                {
                    "schemaVersion": 1,
                    "guard": GUARD,
                    "reason": "the defect class is now covered by an exact oracle",
                    "replacement": REPLACEMENT_GUARD,
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

    def test_a_retirement_replacement_must_be_a_live_required_guard(self) -> None:
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
                    "reason": "the defect class moved",
                    "replacement": REPLACEMENT_GUARD,
                }
            ),
        )
        self.commit(
            f"Retire\n\nReproit-Dogfood: not-a-fix\n"
            f"Reproit-Guard-Retire: {GUARD}\n"
        )
        with self.assertRaisesRegex(MODULE.PolicyError, "not a required guard"):
            self.review()

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
