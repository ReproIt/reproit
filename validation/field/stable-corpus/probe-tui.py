#!/usr/bin/env python3
"""Run the retained TUI known-good cases through fresh bounded PTYs."""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
import pty
import select
import shlex
import signal
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path

import pyte

MAX_OUTPUT_BYTES = 1_048_576
READ_SECONDS = 3.0


def render_screen(output: bytes) -> str:
    screen = pyte.Screen(120, 40)
    stream = pyte.Stream(screen)
    stream.feed(output.decode("utf-8", errors="replace"))
    return "\n".join(line.rstrip() for line in screen.display).strip()


def run_pty(
    argv: list[str],
    keys: bytes = b"",
    cwd: Path | None = None,
    ready_markers: tuple[str, ...] = (),
) -> dict:
    environment = {
        "HOME": str(cwd or Path("/tmp")),
        "LANG": "C.UTF-8",
        "PATH": "/usr/local/go/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        "TERM": "xterm-256color",
    }
    process_id, master = pty.fork()
    if process_id == 0:
        if cwd is not None:
            os.chdir(cwd)
        fcntl.ioctl(1, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        os.execvpe(argv[0], argv, environment)

    output = bytearray()
    started = time.monotonic()
    visible = ""
    input_sent = not keys
    exit_status: int | None = None
    background_answered = False
    cursor_answered = False
    try:
        deadline = time.monotonic() + READ_SECONDS
        while time.monotonic() < deadline and len(output) < MAX_OUTPUT_BYTES:
            waited_id, waited_status = os.waitpid(process_id, os.WNOHANG)
            if waited_id == process_id:
                exit_status = waited_status
                break
            readable, _, _ = select.select([master], [], [], 0.1)
            if not readable:
                continue
            try:
                chunk = os.read(master, 65_536)
            except OSError:
                break
            if not chunk:
                break
            output.extend(chunk)
            if not background_answered and b"\x1b]11;?\x1b\\" in output:
                os.write(master, b"\x1b]11;rgb:0000/0000/0000\x1b\\")
                background_answered = True
            if not cursor_answered and b"\x1b[6n" in output:
                os.write(master, b"\x1b[1;1R")
                cursor_answered = True
            if not input_sent:
                current_screen = render_screen(output)
                if all(marker in current_screen for marker in ready_markers):
                    os.write(master, keys)
                    input_sent = True
                    deadline = time.monotonic() + READ_SECONDS
        # Capture the application screen before asking it to exit. Programs
        # using the alternate screen, including nnn, clear it during teardown.
        visible = render_screen(output)
    finally:
        try:
            os.write(master, b"q")
        except OSError:
            pass
        if exit_status is None:
            try:
                os.killpg(process_id, signal.SIGTERM)
            except ProcessLookupError:
                pass
            wait_deadline = time.monotonic() + 2.0
            while time.monotonic() < wait_deadline:
                waited_id, waited_status = os.waitpid(process_id, os.WNOHANG)
                if waited_id == process_id:
                    exit_status = waited_status
                    break
                time.sleep(0.05)
        if exit_status is None:
            try:
                os.killpg(process_id, signal.SIGKILL)
            except ProcessLookupError:
                pass
            _, exit_status = os.waitpid(process_id, 0)
        os.close(master)

    exit_code = os.waitstatus_to_exitcode(exit_status)
    return {
        "observationReached": bool(output),
        "inputSent": input_sent,
        "cleanLaunch": True,
        "exceptions": [],
        "exitCode": exit_code,
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "screen": visible,
        "ptyBytes": len(output),
        "ptyTailHex": output[-512:].hex(),
        "ptySha256": f"sha256:{hashlib.sha256(output).hexdigest()}",
    }


def run_tmux(
    argv: list[str],
    keys: str,
    cwd: Path,
    ready_markers: tuple[str, ...],
) -> dict:
    session = f"reproit-corpus-{os.getpid()}"
    exit_file = cwd / ".reproit-exit"
    environment = (
        "env -i "
        f"HOME={shlex.quote(str(cwd))} LANG=C.UTF-8 "
        "PATH=/usr/local/go/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin "
        "TERM=xterm-256color "
    )
    command = (
        f"{environment}{shlex.join(argv)}; "
        f"printf '%s\\n' $? > {shlex.quote(str(exit_file))}; sleep 60"
    )
    started = time.monotonic()
    subprocess.run(
        [
            "tmux",
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            "120",
            "-y",
            "40",
            "-c",
            str(cwd),
            command,
        ],
        check=True,
        timeout=2,
    )
    screen = ""
    input_sent = False
    try:
        deadline = time.monotonic() + READ_SECONDS
        while time.monotonic() < deadline:
            result = subprocess.run(
                ["tmux", "capture-pane", "-p", "-t", session],
                check=True,
                capture_output=True,
                text=True,
                timeout=1,
            )
            screen = result.stdout.rstrip()
            if all(marker in screen for marker in ready_markers):
                subprocess.run(
                    ["tmux", "send-keys", "-l", "-t", session, keys],
                    check=True,
                    timeout=1,
                )
                input_sent = True
                time.sleep(0.25)
                screen = subprocess.run(
                    ["tmux", "capture-pane", "-p", "-t", session],
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=1,
                ).stdout.rstrip()
                break
            time.sleep(0.05)
    finally:
        subprocess.run(
            ["tmux", "send-keys", "-t", session, "q"],
            check=False,
            timeout=1,
        )
        subprocess.run(
            ["tmux", "kill-session", "-t", session],
            check=False,
            capture_output=True,
            timeout=1,
        )

    encoded = screen.encode()
    return {
        "observationReached": bool(screen),
        "inputSent": input_sent,
        "cleanLaunch": True,
        "exceptions": [],
        "exitCode": (
            int(exit_file.read_text(encoding="utf-8").strip())
            if exit_file.exists()
            else None
        ),
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "screen": screen,
        "ptyBytes": len(encoded),
        "ptyTailHex": encoded[-512:].hex(),
        "ptySha256": f"sha256:{hashlib.sha256(encoded).hexdigest()}",
    }


def verdict(observation: dict, legal: bool, detail: str) -> dict:
    observation["legalBehaviorObserved"] = legal
    observation["legalBehavior"] = detail
    observation["identity"] = None if legal else "known-good-behavior-misclassified"
    return observation


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="reproit-tui-corpus-") as directory:
        work = Path(directory)
        valid = work / "valid.json"
        valid.write_text('{"alpha": 1}\n', encoding="utf-8")
        clean = run_pty(["/work/bin/fx", str(valid)], cwd=work)
        clean = verdict(clean, "alpha" in clean["screen"], "valid JSON rendered its key")

        empty = work / "empty.json"
        empty.touch()
        fixed_empty = run_pty(["/work/bin/fx", str(empty)], cwd=work)
        fixed_empty = verdict(
            fixed_empty,
            "empty.json" in fixed_empty["screen"]
            and "indexing" not in fixed_empty["screen"],
            "the fixed empty-file path completed instead of remaining in indexing",
        )

        files = work / "files"
        files.mkdir()
        for name in ("alpha.txt", "beta.txt", "gamma.txt"):
            (files / name).touch()
        all_match = run_tmux(
            ["/work/bin/nnn", "-d", str(files)],
            "/a",
            files,
            (("alpha.txt", "beta.txt", "gamma.txt")),
        )
        visible = all_match["screen"]
        all_match = verdict(
            all_match,
            all_match["inputSent"]
            and all(name in visible for name in ("alpha.txt", "beta.txt", "gamma.txt")),
            "filtering by a retained all three legitimate matching rows",
        )

    print(json.dumps({"cases": [clean, fixed_empty, all_match]}, indent=2))


if __name__ == "__main__":
    main()
