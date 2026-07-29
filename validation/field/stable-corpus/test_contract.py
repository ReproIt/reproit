#!/usr/bin/env python3
"""Static contracts for the native stable-corpus harness."""

from __future__ import annotations

import re
import unittest
from pathlib import Path


FIELD = Path(__file__).resolve().parent.parent
CORPUS = Path(__file__).resolve().parent


class StableCorpusContractTests(unittest.TestCase):
    def test_container_field_references_exist(self) -> None:
        scripts = [CORPUS / "prepare.sh", CORPUS / "run-web.sh"]
        references: set[str] = set()
        for script in scripts:
            references.update(
                re.findall(r"/field/([A-Za-z0-9_./-]+)", script.read_text(encoding="utf-8"))
            )

        self.assertIn("stable-corpus/probe-web.mjs", references)
        for reference in references:
            with self.subTest(reference=reference):
                self.assertTrue((FIELD / reference).is_file())

    def test_xvfb_declares_xauth(self) -> None:
        dockerfile = (CORPUS / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn("xvfb", dockerfile)
        self.assertIn("xauth", dockerfile)

    def test_prepare_supports_bounded_lane_reruns(self) -> None:
        prepare = (CORPUS / "prepare.sh").read_text(encoding="utf-8")
        self.assertIn('mode="${1:-all}"', prepare)
        self.assertIn('if [[ "$mode" != tui ]]', prepare)
        self.assertIn('if [[ "$mode" != web ]]', prepare)
        runner = (FIELD / "run-stable-corpus.sh").read_text(encoding="utf-8")
        self.assertIn('mode="${1:-all}"', runner)
        self.assertIn('write_args+=(--tui "$WORK/tui.json")', runner)

    def test_web_verdicts_are_bound_to_the_target_behavior(self) -> None:
        probe = (CORPUS / "probe-web.mjs").read_text(encoding="utf-8")
        self.assertEqual(probe.count("exceptions.length === 0"), 1)
        self.assertIn("vertAboutObservation.aboutContentPresent", probe)
        self.assertIn("vertRootObservation.homeContentPresent", probe)
        self.assertIn("slidevLegal", probe)

    def test_tui_screen_is_captured_before_cleanup(self) -> None:
        probe = (CORPUS / "probe-tui.py").read_text(encoding="utf-8")
        snapshot = probe.index("visible = render_screen(output)")
        cleanup = probe.index('os.write(master, b"q")')
        self.assertLess(snapshot, cleanup)
        self.assertIn('"indexing" not in fixed_empty["screen"]', probe)
        self.assertIn("ready_markers", probe)
        self.assertIn('"inputSent"', probe)


if __name__ == "__main__":
    unittest.main()
