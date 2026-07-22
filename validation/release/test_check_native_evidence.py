#!/usr/bin/env python3
"""Regression tests for release-native evidence verification."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("check-native-evidence.py")
SPEC = importlib.util.spec_from_file_location("check_native_evidence", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {SCRIPT}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class NativeEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary.name)
        self.commit = "a" * 40

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_result(self, gate_id: str) -> None:
        manifest = json.loads(MODULE.MANIFEST.read_text(encoding="utf-8"))
        gate = manifest["gates"][gate_id]
        log = ("\n".join(gate["requiredOutput"]) + "\n").encode()
        log_path = self.directory / f"{gate_id}.log"
        log_path.write_bytes(log)
        result = {
            "schema": 1,
            "gateId": gate_id,
            "commit": self.commit,
            "startedAt": "2026-07-22T10:00:00+00:00",
            "finishedAt": "2026-07-22T10:01:00+00:00",
            "executor": {"os": "test", "architecture": "test"},
            "targetOs": gate["targetOs"],
            "architectures": gate["architectures"],
            "fixture": gate["fixture"],
            "command": gate["command"],
            "status": "passed",
            "exitCode": 0,
            "checks": {marker: True for marker in gate["requiredOutput"]},
            "resetStrategy": gate["resetStrategy"],
            "cleanupStrategy": gate["cleanupStrategy"],
            "logSha256": hashlib.sha256(log).hexdigest(),
            "logPath": str(log_path),
        }
        (self.directory / f"{gate_id}.json").write_text(
            json.dumps(result), encoding="utf-8"
        )

    def test_accepts_matching_result_and_log(self) -> None:
        self.write_result("macos-ax")
        result = MODULE.validate_result("macos-ax", self.directory, self.commit)
        self.assertEqual(result["gateId"], "macos-ax")

    def test_rejects_tampered_log(self) -> None:
        self.write_result("windows-uia")
        (self.directory / "windows-uia.log").write_bytes(b"tampered\n")
        with self.assertRaisesRegex(ValueError, "captured log digest"):
            MODULE.validate_result("windows-uia", self.directory, self.commit)

    def test_rejects_forged_marker_checks(self) -> None:
        self.write_result("macos-ax")
        log = b"unrelated successful command\n"
        (self.directory / "macos-ax.log").write_bytes(log)
        result_path = self.directory / "macos-ax.json"
        result = json.loads(result_path.read_text(encoding="utf-8"))
        result["logSha256"] = hashlib.sha256(log).hexdigest()
        result_path.write_text(json.dumps(result), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "missing required output markers"):
            MODULE.validate_result("macos-ax", self.directory, self.commit)

    def test_rejects_wrong_commit(self) -> None:
        self.write_result("macos-ax")
        with self.assertRaisesRegex(ValueError, "commit"):
            MODULE.validate_result("macos-ax", self.directory, "b" * 40)


class SupportManifestTests(unittest.TestCase):
    def test_release_gates_derive_from_support_manifest(self) -> None:
        gates = MODULE.release_gates()
        self.assertEqual(
            gates,
            {
                "web-chromium": "linux-hosted",
                "react-native-android": "android",
                "flutter-ios": "flutter",
                "macos-ax": "macos",
                "windows-uia": "windows",
            },
        )

    def test_support_manifest_is_well_formed(self) -> None:
        support = json.loads(MODULE.SUPPORT.read_text(encoding="utf-8"))
        known = set(json.loads(MODULE.MANIFEST.read_text(encoding="utf-8"))["gates"])
        self.assertEqual(support["schema"], 1)
        for target_id, target in support["targets"].items():
            self.assertIn(target["maturity"], {"stable", "preview", "experimental"}, target_id)
            self.assertTrue(target["scope"], target_id)
            for gate_id in target["ownedGates"]:
                self.assertIn(gate_id, known, target_id)
            for gate_id in target["releaseGates"]:
                self.assertIn(gate_id, target["ownedGates"], target_id)


if __name__ == "__main__":
    unittest.main()
