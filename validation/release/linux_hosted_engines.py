#!/usr/bin/env python3
"""Select Playwright engines required by Linux hosted release gates."""

from __future__ import annotations

import sys

CHROMIUM_GATES = frozenset({"backend-contract", "electron", "web-chromium"})


def required_engines(gate_csv: str) -> list[str]:
    gates = {gate for gate in gate_csv.split(",") if gate}
    engines = []
    if gates & CHROMIUM_GATES:
        engines.append("chromium")
    if "web-engines" in gates:
        engines.extend(("firefox", "webkit"))
    return engines


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: linux_hosted_engines.py GATE_CSV", file=sys.stderr)
        return 2
    print(*required_engines(sys.argv[1]), sep="\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
