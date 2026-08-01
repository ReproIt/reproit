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

UI_PATH = MODULE_PATH.with_name("react_native_android_ui.py")
UI_SPEC = importlib.util.spec_from_file_location(
    "react_native_android_ui",
    UI_PATH,
)
assert UI_SPEC is not None and UI_SPEC.loader is not None
UI_MODULE = importlib.util.module_from_spec(UI_SPEC)
UI_SPEC.loader.exec_module(UI_MODULE)
find_node = UI_MODULE.find_node

NOTESNOOK_PATH = MODULE_PATH.with_name("notesnook_link_notebooks.py")
NOTESNOOK_SPEC = importlib.util.spec_from_file_location(
    "notesnook_link_notebooks",
    NOTESNOOK_PATH,
)
assert NOTESNOOK_SPEC is not None and NOTESNOOK_SPEC.loader is not None
NOTESNOOK = importlib.util.module_from_spec(NOTESNOOK_SPEC)
NOTESNOOK_SPEC.loader.exec_module(NOTESNOOK)

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


class NodeAddressingTest(unittest.TestCase):
    """The Joplin sidebar row is what these two cases are drawn from.

    A React Native touchable often exposes no clickable attribute, so walking
    up for a clickable ancestor runs off the top of the tree and lands on the
    root, which has no bounds. The first executed joplin campaign failed on
    exactly that with 'UI node has no usable bounds'.
    """

    CLICKABLE = (
        '<hierarchy><node bounds="[0,0][1080,2400]">'
        '<node clickable="true" bounds="[0,100][1080,220]">'
        '<node text="Welcome!" bounds="[40,130][400,190]"/>'
        "</node></node></hierarchy>"
    )
    TOUCHABLE = (
        "<hierarchy><node>"
        '<node bounds="[0,100][1080,220]">'
        '<node text="Welcome!" bounds="[40,130][400,190]"/>'
        "</node></node></hierarchy>"
    )
    BOUNDLESS = (
        "<hierarchy><node><node>"
        '<node text="Welcome!"/>'
        "</node></node></hierarchy>"
    )

    def test_a_clickable_ancestor_is_still_preferred(self) -> None:
        node = find_node(self.CLICKABLE, "Welcome!")

        self.assertEqual(node.attrib["bounds"], "[0,100][1080,220]")
        self.assertEqual(node.attrib["clickable"], "true")

    def test_a_touchable_without_clickable_is_still_addressable(self) -> None:
        node = find_node(self.TOUCHABLE, "Welcome!")

        self.assertEqual(node.attrib["bounds"], "[40,130][400,190]")

    def test_a_subtree_with_no_bounds_at_all_is_reported(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "no usable bounds"):
            find_node(self.BOUNDLESS, "Welcome!")


class WelcomeNotebookPresenceTest(unittest.TestCase):
    """The trigger cannot continue while the notebook it deleted is still there.

    An executed run passed this check with the notebook still listed, because
    it compared one exact attribute string against the raw dump and the row had
    gained a nesting suffix. Presence is read from the parsed tree by prefix.
    """

    PREFIX = MODULE.SIDEBAR_NOTEBOOK

    def sidebar(self, *descriptions: str) -> str:
        rows = "".join(f'<node content-desc="{value}"/>' for value in descriptions)
        return f"<hierarchy><node>{rows}</node></hierarchy>"

    def test_the_row_is_seen_through_its_nesting_suffix(self) -> None:
        source = self.sidebar(f"{self.PREFIX}  (level 1)")

        self.assertTrue(MODULE.welcome_notebook_present(source))

    def test_the_row_is_seen_without_a_suffix(self) -> None:
        self.assertTrue(MODULE.welcome_notebook_present(self.sidebar(self.PREFIX)))

    def test_a_deleted_notebook_is_absent(self) -> None:
        source = self.sidebar("All notes", "Trash", "Configuration")

        self.assertFalse(MODULE.welcome_notebook_present(source))

    def test_either_spelling_of_the_sync_section_is_the_settings_screen(self) -> None:
        for label in ("Synchronisation", "Synchronization"):
            with self.subTest(label=label):
                self.assertTrue(
                    MODULE.configuration_screen_shown(f"Configuration {label}")
                )

    def test_the_sidebar_alone_is_not_the_settings_screen(self) -> None:
        self.assertFalse(MODULE.configuration_screen_shown("Configuration Trash"))


class NotesnookObservableTest(unittest.TestCase):
    """The note row's own label is the whole observable.

    The row joins every part of its accessibility label with a comma, and each
    linked notebook is preceded by a notebook glyph the raw dump escapes as a
    private-use codepoint, so the linked set is read from the parsed tree.
    """

    GLYPH = "\U000f0b64"

    def row(self, description: str) -> str:
        return (
            "<hierarchy><node>"
            f'<node resource-id="note-item-0" content-desc="{description}" '
            'bounds="[0,591][1080,814]"/>'
            "</node></hierarchy>"
        )

    def test_both_notebooks_are_read_off_the_affected_row(self) -> None:
        source = self.row(
            f"02:15 PM, TriggerNote, {self.GLYPH}, Alpha, {self.GLYPH}, Beta"
        )

        self.assertEqual(
            NOTESNOOK.linked_notebooks(source),
            frozenset({"Alpha", "Beta"}),
        )

    def test_the_fixed_row_lists_the_new_notebook_alone(self) -> None:
        source = self.row(f"02:28 PM, TriggerNote, {self.GLYPH}, Beta")

        self.assertEqual(NOTESNOOK.linked_notebooks(source), frozenset({"Beta"}))

    def test_an_unlinked_row_lists_no_notebook(self) -> None:
        source = self.row("02:28 PM, TriggerNote")

        self.assertEqual(NOTESNOOK.linked_notebooks(source), frozenset())

    def test_the_relink_verdict_follows_the_row_and_the_subject(self) -> None:
        affected = frozenset({"Alpha", "Beta"})

        self.assertEqual(
            NOTESNOOK.relink_identity("benchmark", affected),
            NOTESNOOK.NOTESNOOK_IDENTITY,
        )
        self.assertIsNone(
            NOTESNOOK.relink_identity("benchmark", frozenset({"Beta"}))
        )
        self.assertIsNone(
            NOTESNOOK.relink_identity(
                "adversarial-multi-select",
                frozenset({"Alpha", "Beta", "Gamma"}),
            )
        )

    def test_an_outcome_the_subject_cannot_reach_is_refused(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "not an outcome"):
            NOTESNOOK.relink_identity("benchmark", frozenset({"Alpha"}))
        with self.assertRaisesRegex(RuntimeError, "not an outcome"):
            NOTESNOOK.relink_identity(
                "adversarial-restored-selection",
                frozenset({"Alpha", "Beta"}),
            )


class NotesnookAddressingTest(unittest.TestCase):
    """Two nodes the editor and the link screen really publish.

    The editor toolbar lists buttons with [0,0][0,0] bounds, and tapping the
    centre of one of those is a tap at the top left corner of the screen. The
    link screen's notebook row carries the name, a comma and a checkbox glyph
    that changes when it is selected, so only the prefix is stable.
    """

    SOURCE = (
        "<hierarchy><node>"
        '<node resource-id="tool-link" bounds="[0,0][0,0]"/>'
        '<node resource-id="editor-title" bounds="[0,364][1080,472]"/>'
        '<node content-desc="Alpha, \U000f0131" bounds="[42,713][1038,801]"/>'
        '<node content-desc="Alphabet soup" bounds="[42,900][1038,988]"/>'
        "</node></hierarchy>"
    )

    def test_a_zero_area_node_is_not_addressable(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "not found by resource id"):
            NOTESNOOK.resource_node(self.SOURCE, "tool-link")

    def test_a_test_id_addresses_its_own_node(self) -> None:
        node = NOTESNOOK.resource_node(self.SOURCE, "editor-title")

        self.assertEqual(node.attrib["bounds"], "[0,364][1080,472]")

    def test_the_notebook_row_matches_the_name_and_its_comma(self) -> None:
        row = NOTESNOOK.notebook_row(self.SOURCE, "Alpha")

        self.assertIsNotNone(row)
        assert row is not None
        self.assertEqual(row.attrib["bounds"], "[42,713][1038,801]")

    def test_a_notebook_that_is_not_listed_is_not_invented(self) -> None:
        self.assertIsNone(NOTESNOOK.notebook_row(self.SOURCE, "Gamma"))


if __name__ == "__main__":
    unittest.main()
