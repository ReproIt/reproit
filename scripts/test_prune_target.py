#!/usr/bin/env python3
"""Regression test: prune-target.sh must never touch build-script output.

A build-script OUT_DIR file can be far older than the prune age while its
fingerprint stays fresh; deleting it leaves cargo consuming a half-missing
build without re-running the script (a pruned stdlib-symbols.txt broke the
next tree-sitter compile exactly so). The prune may delete only ordinary
artifacts (deps, incremental); OUT_DIRs and fingerprints must survive.
"""

import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(
    subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip()
)
BASE = ROOT / "target" / "debug"
UNIT = "reproit-prune-selftest"
OUT_FILE = BASE / "build" / UNIT / "out" / "stdlib-symbols.txt"
FINGERPRINT = BASE / ".fingerprint" / UNIT / "lib-stale"
ARTIFACT = BASE / "deps" / f"lib{UNIT}-stale.rlib"
STALE = time.time() - 30 * 86_400


def make_stale(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("selftest\n")
    os.utime(path, (STALE, STALE))


def cleanup() -> None:
    for path in (OUT_FILE, FINGERPRINT, ARTIFACT):
        path.unlink(missing_ok=True)


def main() -> int:
    for path in (OUT_FILE, FINGERPRINT, ARTIFACT):
        make_stale(path)
    try:
        subprocess.run(
            ["sh", str(ROOT / "scripts" / "prune-target.sh")],
            cwd=ROOT,
            check=True,
            stdout=subprocess.DEVNULL,
            timeout=240,
        )
        failures = []
        if not OUT_FILE.exists():
            failures.append(f"{OUT_FILE} was pruned; OUT_DIRs must survive")
        if not FINGERPRINT.exists():
            failures.append(f"{FINGERPRINT} was pruned; fingerprints must survive")
        if ARTIFACT.exists():
            failures.append(f"{ARTIFACT} survived; stale artifacts must be pruned")
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        if failures:
            return 1
        print("PASS: prune preserves build-script output and fingerprints")
        return 0
    finally:
        cleanup()


if __name__ == "__main__":
    sys.exit(main())
