#!/usr/bin/env python3
"""Boundary tests for the clean and adversarial corpus validator."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("check_corpus", ROOT / "check-corpus.py")
check_corpus = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check_corpus)


def case(identifier: str, kind: str, identity=None, reached: bool = True) -> dict:
    return {
        "id": identifier,
        "kind": kind,
        "application": "some-application",
        "repository": "https://github.com/owner/repo",
        "revision": "0" * 40,
        "fixture": None,
        "variant": "default",
        "why": "a stated reason to be in the corpus",
        "observationReached": reached,
        "identity": identity,
        "falsePositive": identity is not None,
        "observation": {"identity": identity},
    }


def document(cases: list[dict], **overrides) -> dict:
    record = {
        "schemaVersion": 1,
        "target": "electron-linux",
        "worker": {"image": "worker:amd64", "platform": "linux/amd64", "network": "none"},
        "cleanCases": sum(1 for entry in cases if entry["kind"] == "clean"),
        "adversarialCases": sum(1 for entry in cases if entry["kind"] == "adversarial"),
        "confirmedFalsePositives": sum(1 for entry in cases if entry["falsePositive"]),
        "unreachedObservations": sum(1 for entry in cases if not entry["observationReached"]),
        "containersRemaining": 0,
        "cases": cases,
    }
    record.update(overrides)
    return record


SUFFICIENT = [
    case("clean-base", "clean"),
    case("adversarial-one", "adversarial"),
    case("adversarial-two", "adversarial"),
]


class CheckCorpusTest(unittest.TestCase):
    def test_accepts_a_corpus_with_no_false_positive(self):
        check_corpus.validate(document(SUFFICIENT))

    def test_rejects_a_confirmed_false_positive(self):
        cases = [
            case("clean-base", "clean"),
            case("adversarial-one", "adversarial", "some:identity"),
            case("adversarial-two", "adversarial"),
        ]
        with self.assertRaises(ValueError):
            check_corpus.validate(document(cases))

    def test_rejects_too_few_adversarial_subjects(self):
        with self.assertRaises(ValueError):
            check_corpus.validate(document(SUFFICIENT[:2]))

    def test_rejects_a_subject_that_never_reached_observation(self):
        cases = [case("clean-base", "clean", None, False), *SUFFICIENT[1:]]
        with self.assertRaises(ValueError):
            check_corpus.validate(document(cases))

    def test_rejects_a_leaked_container(self):
        with self.assertRaises(ValueError):
            check_corpus.validate(document(SUFFICIENT, containersRemaining=1))

    def test_rejects_a_subject_that_was_not_run_offline(self):
        record = document(SUFFICIENT)
        record["worker"]["network"] = "bridge"
        with self.assertRaises(ValueError):
            check_corpus.validate(record)

    def test_rejects_duplicate_case_ids(self):
        cases = [case("clean-base", "clean"), case("clean-base", "adversarial"),
                 case("adversarial-two", "adversarial")]
        with self.assertRaises(ValueError):
            check_corpus.validate(document(cases))

    def test_rejects_a_count_that_disagrees_with_the_cases(self):
        with self.assertRaises(ValueError):
            check_corpus.validate(document(SUFFICIENT, cleanCases=5))


if __name__ == "__main__":
    unittest.main()
