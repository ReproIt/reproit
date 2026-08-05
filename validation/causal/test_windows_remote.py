#!/usr/bin/env python3
"""Regression contract for exact-source Windows validation."""

from __future__ import annotations

import unittest
from pathlib import Path

COLLECTOR = Path(__file__).resolve().parent / "run-windows-remote.sh"


class WindowsRemoteContractTests(unittest.TestCase):
    def test_requires_clean_exact_local_source(self) -> None:
        source = COLLECTOR.read_text(encoding="utf-8")
        self.assertIn("exact-commit validation requires a clean worktree", source)
        self.assertIn('git -C "$ROOT" status --porcelain=v1', source)
        self.assertIn("source HEAD does not match the requested commit", source)

    def test_transfers_a_bounded_digest_verified_archive(self) -> None:
        source = COLLECTOR.read_text(encoding="utf-8")
        self.assertIn("MAX_SOURCE_ARCHIVE_BYTES", source)
        self.assertIn('shasum -a 256 "$SOURCE_ARCHIVE"', source)
        self.assertIn("scp -q", source)
        self.assertIn("Join-Path $HOME", source)
        self.assertIn("Get-FileHash -Algorithm SHA256", source)
        self.assertNotIn("FromBase64String", source)
        self.assertIn("tar.exe -xzf", source)

    def test_windows_git_checks_ignore_host_conversion_settings(self) -> None:
        source = COLLECTOR.read_text(encoding="utf-8")
        self.assertIn("core.autocrlf=false", source)
        self.assertIn("core.filemode=false", source)

    def test_does_not_depend_on_published_source(self) -> None:
        source = COLLECTOR.read_text(encoding="utf-8")
        self.assertNotIn("github.com/ReproIt/reproit.git", source)
        self.assertNotIn("git fetch", source)
        self.assertNotIn("git clone", source)


if __name__ == "__main__":
    unittest.main()
