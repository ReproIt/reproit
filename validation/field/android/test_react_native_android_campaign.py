#!/usr/bin/env python3
"""Unit checks for the React Native Android campaign oracle."""

from __future__ import annotations

import importlib.util
import os
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("react_native_android_campaign.py")
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC = importlib.util.spec_from_file_location(
    "react_native_android_campaign",
    MODULE_PATH,
)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
music_signature_flags = MODULE.music_signature_flags
music_observation_reached = MODULE.music_observation_reached

RUNTIME_PATH = MODULE_PATH.with_name("react_native_android_runtime.py")
RUNTIME_SPEC = importlib.util.spec_from_file_location(
    "react_native_android_runtime",
    RUNTIME_PATH,
)
assert RUNTIME_SPEC is not None and RUNTIME_SPEC.loader is not None
RUNTIME_MODULE = importlib.util.module_from_spec(RUNTIME_SPEC)
RUNTIME_SPEC.loader.exec_module(RUNTIME_MODULE)
has_default_route = RUNTIME_MODULE.has_default_route
configure_android_environment = RUNTIME_MODULE.configure_android_environment


def signature(*card_texts: tuple[str, ...]) -> dict:
    return {
        "texts": sorted({text for card in card_texts for text in card}),
        "clickableCards": [{"texts": list(card)} for card in card_texts],
    }


class MusicSignatureFlagsTest(unittest.TestCase):
    def test_split_album_is_the_defect_identity(self) -> None:
        flags = music_signature_flags(
            [
                signature(("Reproit Field Album", "2024")),
                signature(("Reproit Field Album", "————")),
                signature(("Reproit Control Alpha", "2021")),
                signature(("Reproit Control Beta", "2022")),
            ]
        )

        self.assertEqual(flags["fieldAlbumDescriptions"], ["2024", "————"])
        self.assertTrue(flags["fieldAlbumSplit"])
        self.assertTrue(flags["controlAlpha"])
        self.assertTrue(flags["controlBeta"])

    def test_grouped_album_is_not_the_defect_identity(self) -> None:
        flags = music_signature_flags(
            [
                signature(("Reproit Field Album", "2024")),
                signature(("Reproit Control Alpha", "2021")),
                signature(("Reproit Control Beta", "2022")),
            ]
        )

        self.assertEqual(flags["fieldAlbumDescriptions"], ["2024"])
        self.assertFalse(flags["fieldAlbumSplit"])
        self.assertTrue(flags["controlAlpha"])
        self.assertTrue(flags["controlBeta"])

    def test_one_card_with_both_descriptions_is_not_a_split(self) -> None:
        flags = music_signature_flags(
            [
                signature(("Reproit Field Album", "2024", "————")),
                signature(("Reproit Control Alpha", "2021")),
                signature(("Reproit Control Beta", "2022")),
            ]
        )

        self.assertEqual(flags["fieldAlbumDescriptions"], ["2024", "————"])
        self.assertFalse(flags["fieldAlbumSplit"])

    def test_observation_modes_require_their_independent_controls(self) -> None:
        benchmark = {
            "fieldAlbumDescriptions": ["2024", "————"],
            "controlAlpha": True,
            "controlBeta": True,
        }
        clean = {
            "fieldAlbumDescriptions": [],
            "controlAlpha": True,
            "controlBeta": True,
        }
        adversarial = {
            "fieldAlbumDescriptions": ["2024"],
            "controlAlpha": False,
            "controlBeta": False,
        }

        self.assertTrue(music_observation_reached(benchmark, "benchmark"))
        self.assertTrue(music_observation_reached(clean, "clean-distinct-albums"))
        self.assertTrue(
            music_observation_reached(
                adversarial,
                "adversarial-grouped-album",
            )
        )
        benchmark["controlBeta"] = False
        self.assertFalse(music_observation_reached(benchmark, "benchmark"))


class ContainerRouteTest(unittest.TestCase):
    HEADER = "Iface Destination Gateway Flags RefCnt Use Metric Mask"

    def test_loopback_route_has_no_default(self) -> None:
        route_table = f"{self.HEADER}\nlo 0000007F 00000000 0001 0 0 0 000000FF"

        self.assertFalse(has_default_route(route_table))

    def test_gateway_default_route_is_detected(self) -> None:
        route_table = (
            f"{self.HEADER}\n"
            "eth0 00000000 010011AC 0003 0 0 0 00000000"
        )

        self.assertTrue(has_default_route(route_table))


class AndroidEnvironmentTest(unittest.TestCase):
    def test_appium_inherits_both_android_sdk_environment_names(self) -> None:
        with tempfile.TemporaryDirectory() as sdk_directory:
            sdk = Path(sdk_directory)
            with mock.patch.dict(os.environ, {}, clear=True):
                evidence = configure_android_environment(sdk)

                self.assertEqual(os.environ["ANDROID_HOME"], str(sdk.resolve()))
                self.assertEqual(
                    os.environ["ANDROID_SDK_ROOT"],
                    str(sdk.resolve()),
                )
                self.assertEqual(evidence["androidHome"], str(sdk.resolve()))
                self.assertEqual(evidence["androidSdkRoot"], str(sdk.resolve()))

    def test_missing_android_sdk_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as parent:
            missing = Path(parent) / "missing"

            with self.assertRaisesRegex(RuntimeError, "does not exist"):
                configure_android_environment(missing)


if __name__ == "__main__":
    unittest.main()
