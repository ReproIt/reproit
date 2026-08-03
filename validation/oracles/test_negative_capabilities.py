#!/usr/bin/env python3
"""Negative capability gates for zero-authority native oracle exclusions."""

from __future__ import annotations

import json
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LEDGER = json.loads(
    (Path(__file__).with_name("coverage.json")).read_text(encoding="utf-8")
)


def source(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def unavailable(marker: str) -> dict[str, str]:
    row = next(row for row in LEDGER["oracles"] if row["marker"] == marker)
    return row["unavailable"]


class NegativeNativeCapabilityTests(unittest.TestCase):
    def test_every_removed_raw_gap_has_a_specific_authority_reason(self) -> None:
        expected = {
            "no_second_authority": {
                ("EXPLORE:A11YSTATESTATUS", "tauri"),
                ("EXPLORE:A11YSTATESTATUS", "flutter"),
            },
            "no_blank_failure_authority": {
                ("EXPLORE:BLANKSCREEN", runner)
                for runner in ("macos-ax", "uia", "atspi")
            },
            "no_choice_equivalence": {
                ("EXPLORE:CHOICEBUG", runner)
                for runner in ("appium", "flutter", "macos-ax", "uia", "atspi", "tui")
            },
            "no_input_event_stream": {
                ("EXPLORE:DEADINPUT", runner)
                for runner in ("appium", "flutter", "macos-ax", "uia", "atspi")
            },
            "no_presented_frame_stream": {
                ("EXPLORE:FLICKER", runner)
                for runner in ("macos-ax", "uia", "atspi")
            },
            "no_focus_retention_contract": {
                ("EXPLORE:FOCUSLOSS", runner)
                for runner in ("appium", "flutter", "tui")
            },
            "no_listener_tally": {
                ("EXPLORE:LISTENERLEAK", runner)
                for runner in ("appium", "flutter")
            },
            "no_native_hit_test": {("EXPLORE:OCCLUSION", "appium")},
            "no_occlusion_ownership": {
                ("EXPLORE:OCCLUSION", runner)
                for runner in ("flutter", "macos-ax", "uia", "atspi")
            },
            "no_clip_ownership": {
                ("EXPLORE:OVERFLOW", runner)
                for runner in ("appium", "macos-ax", "uia", "atspi", "tui")
            },
            "no_declared_relationships": {
                ("EXPLORE:RELATION", runner)
                for runner in ("macos-ax", "uia", "atspi", "tui")
            },
            "no_native_node_identity": {("EXPLORE:RERENDER", "appium")},
            "no_presented_rebuild_authority": {
                ("EXPLORE:RERENDER", runner)
                for runner in ("electron", "tauri", "flutter", "macos-ax", "uia", "atspi")
            },
            "no_scroll_offset_authority": {
                ("EXPLORE:SCROLLROUNDTRIP", runner)
                for runner in ("appium", "tui")
            },
        }
        self.assertEqual(sum(map(len, expected.values())), 47)
        for reason, pairs in expected.items():
            for marker, runner in pairs:
                with self.subTest(marker=marker, runner=runner):
                    self.assertEqual(unavailable(marker)[runner], reason)

    def test_tauri_and_flutter_have_no_independent_accessibility_authority(self) -> None:
        tauri = source("runners/source/tauri/part-01.mjs")
        flutter = source(
            "crates/reproit/assets/scaffolds/flutter/integration_test/"
            "reproit_explorer/semantics.dart"
        )
        self.assertNotIn("newCDPSession", tauri)
        self.assertNotIn("Accessibility.getFullAXTree", tauri)
        self.assertIn("SemanticsData", flutter)
        self.assertNotIn("Accessibility.getFullAXTree", flutter)
        self.assertEqual(unavailable("EXPLORE:A11YSTATESTATUS")["tauri"], "no_second_authority")
        self.assertEqual(
            unavailable("EXPLORE:A11YSTATESTATUS")["flutter"],
            "no_second_authority",
        )

    def test_appium_snapshot_has_no_event_hit_test_or_runtime_identity_channel(self) -> None:
        capture = source("runners/source/react-native/part-03.mjs")
        tree = source("runners/source/react-native/part-02.mjs")
        self.assertIn("driver.getPageSource()", capture)
        self.assertIn("function rectOfEl", tree)
        self.assertNotIn("elementFromPoint", capture + tree)
        self.assertNotIn("defaultPrevented", capture + tree)
        self.assertNotIn("runtimeObjectId", capture + tree)
        self.assertEqual(unavailable("EXPLORE:DEADINPUT")["appium"], "no_input_event_stream")
        self.assertEqual(unavailable("EXPLORE:OCCLUSION")["appium"], "no_native_hit_test")
        self.assertEqual(
            unavailable("EXPLORE:RERENDER")["appium"],
            "no_native_node_identity",
        )

    def test_accessibility_geometry_has_no_clip_ownership_contract(self) -> None:
        sources = {
            "macos-ax": source("runners/macos-ax/accessibility.swift"),
            "uia": source("crates/reproit/src/adapters/uia/capture.rs"),
            "atspi": source("crates/reproit/src/adapters/atspi/capture.rs"),
        }
        for runner, text in sources.items():
            with self.subTest(runner=runner):
                self.assertNotIn("data-reproit-indicator-container", text)
                self.assertNotIn("overflowX", text)
                self.assertEqual(
                    unavailable("EXPLORE:OVERFLOW")[runner],
                    "no_clip_ownership",
                )
                self.assertEqual(
                    unavailable("EXPLORE:RELATION")[runner],
                    "no_declared_relationships",
                )

    def test_terminal_grid_has_no_component_or_scroll_container_identity(self) -> None:
        screen = source("crates/reproit/src/adapters/tui/screen.rs")
        self.assertIn("vt100::Parser", screen)
        self.assertNotIn("ScrollPattern", screen)
        self.assertEqual(
            unavailable("EXPLORE:CHOICEBUG")["tui"],
            "no_choice_equivalence",
        )
        self.assertEqual(
            unavailable("EXPLORE:SCROLLROUNDTRIP")["tui"],
            "no_scroll_offset_authority",
        )

    def test_empty_native_tree_is_not_promoted_to_a_blank_screen(self) -> None:
        for runner in ("macos-ax", "uia", "atspi"):
            with self.subTest(runner=runner):
                self.assertEqual(
                    unavailable("EXPLORE:BLANKSCREEN")[runner],
                    "no_blank_failure_authority",
                )

    def test_flutter_does_not_infer_policy_from_input_hit_test_or_layout(self) -> None:
        self.assertEqual(
            unavailable("EXPLORE:DEADINPUT")["flutter"],
            "no_input_event_stream",
        )
        self.assertEqual(
            unavailable("EXPLORE:FOCUSLOSS")["flutter"],
            "no_focus_retention_contract",
        )
        self.assertEqual(
            unavailable("EXPLORE:OCCLUSION")["flutter"],
            "no_occlusion_ownership",
        )

    def test_settled_identity_and_hit_tests_are_not_promoted_to_visual_defects(self) -> None:
        sources = {
            "electron": source("runners/source/electron/part-06.mjs"),
            "tauri": source("runners/source/tauri/part-05.mjs"),
            "flutter": source(
                "crates/reproit/assets/scaffolds/flutter/integration_test/"
                "reproit_explorer/runner.dart"
            ),
            "macos-ax": source("runners/macos-ax/main.swift"),
            "uia": source("crates/reproit/src/adapters/uia/mod.rs"),
            "atspi": source("crates/reproit/src/adapters/atspi/session.rs"),
        }
        for runner, text in sources.items():
            with self.subTest(runner=runner):
                self.assertNotIn("EXPLORE:RERENDER", text)
                self.assertEqual(
                    unavailable("EXPLORE:RERENDER")[runner],
                    "no_presented_rebuild_authority",
                )
        for runner in ("macos-ax", "uia", "atspi"):
            self.assertNotIn("EXPLORE:OCCLUSION", sources[runner])
            self.assertEqual(
                unavailable("EXPLORE:OCCLUSION")[runner],
                "no_occlusion_ownership",
            )


if __name__ == "__main__":
    unittest.main()
