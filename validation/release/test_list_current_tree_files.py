#!/usr/bin/env python3
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("list-current-tree-files.sh")


class CurrentTreeFilesTests(unittest.TestCase):
    def test_excludes_deleted_and_ignored_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            subprocess.run(["git", "init", "--quiet", root], check=True)
            (root / ".gitignore").write_text("ignored.txt\n", encoding="utf-8")
            (root / "kept.txt").write_text("kept\n", encoding="utf-8")
            (root / "deleted.txt").write_text("deleted\n", encoding="utf-8")
            subprocess.run(
                ["git", "-C", root, "add", ".gitignore", "kept.txt", "deleted.txt"],
                check=True,
            )
            (root / "deleted.txt").unlink()
            (root / "untracked.txt").write_text("untracked\n", encoding="utf-8")
            (root / "ignored.txt").write_text("ignored\n", encoding="utf-8")

            result = subprocess.run(
                ["bash", SCRIPT, root],
                check=True,
                capture_output=True,
            )
            paths = set(result.stdout.rstrip(b"\0").split(b"\0"))

            self.assertEqual(
                paths,
                {b".gitignore", b"kept.txt", b"untracked.txt"},
            )


if __name__ == "__main__":
    unittest.main()
