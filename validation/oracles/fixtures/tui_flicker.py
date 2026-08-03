#!/usr/bin/env python3
"""Real PTY fixture for presented-frame flicker controls."""

from __future__ import annotations

import os
import sys
import termios
import time
import tty


def write(value: str) -> None:
    os.write(sys.stdout.fileno(), value.encode())


mode = sys.argv[1]
old = termios.tcgetattr(sys.stdin.fileno())
try:
    tty.setraw(sys.stdin.fileno())
    before = "┌──────┐\r\n│AAAABB│\r\n└──────┘"
    after = "┌──────┐\r\n│AAAACC│\r\n└──────┘"
    write("\x1b[2J\x1b[H" + before)
    os.read(sys.stdin.fileno(), 1)
    if mode == "positive":
        write("\x1b[2J\x1b[H")
        time.sleep(0.08)
        write(after)
    elif mode == "fixed":
        write("\x1b[H" + after)
    elif mode == "synchronized-adversarial":
        write("\x1b[?2026h\x1b[2J\x1b[H")
        time.sleep(0.08)
        write(after + "\x1b[?2026l")
    elif mode == "idle-redraw":
        write("\x1b[2J\x1b[H")
        time.sleep(0.08)
        write(before)
    else:
        raise SystemExit("unknown mode")
    time.sleep(0.5)
finally:
    termios.tcsetattr(sys.stdin.fileno(), termios.TCSADRAIN, old)
