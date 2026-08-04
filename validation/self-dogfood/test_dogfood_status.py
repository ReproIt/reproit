#!/usr/bin/env python3
"""Keep the self-dogfood scoreboard aligned with the required corpus."""

from __future__ import annotations

import json
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
GUARDS = ROOT / ".reproit/repros"
STATUS = ROOT / "validation/self-dogfood/DOGFOOD-STATUS.md"
DECISION = ROOT / "docs/decisions/self-dogfood.md"


class DogfoodStatusTests(unittest.TestCase):
    def test_every_required_guard_is_named_by_current_id_and_alias(self) -> None:
        status = STATUS.read_text(encoding="utf-8")
        decision = DECISION.read_text(encoding="utf-8")
        required = 0
        for path in sorted(GUARDS.glob("*/meta.json")):
            metadata = json.loads(path.read_text(encoding="utf-8"))
            if metadata["status"] != "required":
                continue
            required += 1
            guard = f"rep_{metadata['id']}"
            alias = metadata.get("alias")
            self.assertIn(guard, status)
            self.assertIn(guard, decision)
            self.assertIsInstance(alias, str)
            self.assertIn(alias, status)
        self.assertGreater(required, 0)

    def test_process_capsule_status_matches_the_live_keep_route(self) -> None:
        keep = (ROOT / "crates/reproit/src/workflows/keep_command.rs").read_text(
            encoding="utf-8"
        )
        status = STATUS.read_text(encoding="utf-8")
        self.assertIn("is_process_capsule", keep)
        self.assertNotIn("Can a process capsule be kept as a guard? Not yet", status)


if __name__ == "__main__":
    unittest.main()
