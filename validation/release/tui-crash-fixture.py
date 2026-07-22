#!/usr/bin/env python3
"""Deterministic PTY fixture for the CLI finding-output contract."""

import sys
import termios
import tty


def main() -> None:
    original = termios.tcgetattr(sys.stdin.fileno())
    try:
        tty.setraw(sys.stdin.fileno())
        sys.stdout.write("Reproit release output fixture\r\nPress any key\r\n")
        sys.stdout.flush()
        sys.stdin.read(1)
        raise RuntimeError("release-output-contract")
    finally:
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSADRAIN, original)


if __name__ == "__main__":
    main()
