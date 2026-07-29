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

    def test_accepts_windows_log_path_on_non_windows_host(self) -> None:
        self.write_result("windows-uia")
        result_path = self.directory / "windows-uia.json"
        result = json.loads(result_path.read_text(encoding="utf-8"))
        result["logPath"] = r"C:\lab\evidence\windows-uia.log"
        result_path.write_text(json.dumps(result), encoding="utf-8")
        validated = MODULE.validate_result("windows-uia", self.directory, self.commit)
        self.assertEqual(validated["gateId"], "windows-uia")

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
                "backend-contract": "linux-hosted",
                "compose-android": "android",
                "electron": "linux-hosted",
                "flutter-android": "android",
                "flutter-ios": "flutter",
                "linux-atspi-gtk": "linux-containers",
                "linux-atspi-toolkits": "linux-containers",
                "macos-ax": "macos",
                "react-native-android": "android",
                "react-native-ios": "swiftui",
                "swiftui-ios": "swiftui",
                "tauri": "linux-containers",
                "tui-pty": "linux-hosted",
                "web-chromium": "linux-hosted",
                "web-engines": "linux-hosted",
                "windows-uia": "windows",
            },
        )

    def test_support_manifest_is_well_formed(self) -> None:
        support = json.loads(MODULE.SUPPORT.read_text(encoding="utf-8"))
        known = set(json.loads(MODULE.MANIFEST.read_text(encoding="utf-8"))["gates"])
        self.assertEqual(support["schema"], 3)
        for target_id, target in support["targets"].items():
            self.assertIn(target["maturity"], {"stable", "preview", "experimental"}, target_id)
            self.assertTrue(target["scope"], target_id)
            self.assertTrue(target["displayName"], target_id)
            self.assertTrue(target["family"], target_id)
            for gate_id in target["ownedGates"]:
                self.assertIn(gate_id, known, target_id)
            for gate_id in target["releaseGates"]:
                self.assertIn(gate_id, target["ownedGates"], target_id)
            self.assertEqual(
                set(target["releaseGates"]),
                set(target["ownedGates"]),
                f"{target_id}: every owned gate must authorize releases",
            )
            promotion = target["promotion"]
            benchmark_path = promotion["fieldBenchmark"]
            if target["maturity"] != "stable":
                self.assertTrue(promotion["blockers"], target_id)
                if benchmark_path is not None:
                    benchmark = json.loads(
                        (MODULE.ROOT / benchmark_path).read_text(encoding="utf-8")
                    )
                    self.assertEqual(benchmark["target"], target_id)
                    self.assertIn(benchmark["status"], {"pending", "complete"})
                continue
            self.assertEqual(promotion["blockers"], [], target_id)
            self.assertIsInstance(benchmark_path, str, target_id)
            benchmark = json.loads(
                (MODULE.ROOT / benchmark_path).read_text(encoding="utf-8")
            )
            self.assertEqual(benchmark["target"], target_id)
            self.assertEqual(benchmark["status"], "complete")
            self.assertGreaterEqual(len(benchmark["applications"]), 2)


if __name__ == "__main__":
    unittest.main()
