#!/usr/bin/env python3
"""Run a command until it exits, stalls, or emits every required marker."""

from __future__ import annotations

import argparse
import os
import queue
import signal
import subprocess
import sys
import threading
import time
from collections.abc import Sequence

IDLE_TIMEOUT_EXIT_CODE = 124
MAX_IDLE_TIMEOUT_SECONDS = 3600
OUTPUT_QUEUE_CHUNKS = 128
READ_CHUNK_BYTES = 64 * 1024
TERMINATION_GRACE_SECONDS = 5


def positive_timeout(value: str) -> int:
    timeout_seconds = int(value)
    if not 1 <= timeout_seconds <= MAX_IDLE_TIMEOUT_SECONDS:
        raise argparse.ArgumentTypeError(
            f"expected a value from 1 through {MAX_IDLE_TIMEOUT_SECONDS}"
        )
    return timeout_seconds


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--idle-timeout-seconds",
        type=positive_timeout,
        required=True,
        help="stop when the command produces no output for this many seconds",
    )
    parser.add_argument(
        "--success-marker",
        action="append",
        default=[],
        help="stop successfully once every repeated marker has appeared",
    )
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.command[:1] == ["--"]:
        args.command = args.command[1:]
    if not args.command:
        parser.error("a command is required after --")
    return args


def stop_process_group(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    if os.name == "nt":
        process.terminate()
    else:
        os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=TERMINATION_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        if os.name == "nt":
            process.kill()
        else:
            os.killpg(process.pid, signal.SIGKILL)
        process.wait()


def read_output(
    stream: object,
    chunks: queue.Queue[bytes | None],
) -> None:
    read_chunk = getattr(stream, "read1", getattr(stream, "read"))
    while chunk := read_chunk(READ_CHUNK_BYTES):
        chunks.put(chunk)
    chunks.put(None)


def write_output(chunk: bytes) -> None:
    try:
        sys.stdout.buffer.write(chunk)
        sys.stdout.buffer.flush()
    except BrokenPipeError:
        pass


def run(
    command: Sequence[str],
    idle_timeout_seconds: int,
    success_markers: Sequence[str],
) -> int:
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=os.name != "nt",
    )
    assert process.stdout is not None
    chunks: queue.Queue[bytes | None] = queue.Queue(maxsize=OUTPUT_QUEUE_CHUNKS)
    reader = threading.Thread(
        target=read_output,
        args=(process.stdout, chunks),
        daemon=True,
    )
    reader.start()

    def relay_signal(signum: int, _frame: object) -> None:
        stop_process_group(process)
        raise SystemExit(128 + signum)

    previous_handlers: dict[int, object] = {}
    for signum in (signal.SIGINT, signal.SIGTERM):
        previous_handlers[signum] = signal.signal(signum, relay_signal)

    encoded_markers = [marker.encode() for marker in success_markers]
    marker_seen = [False] * len(encoded_markers)
    marker_overlap = max((len(marker) for marker in encoded_markers), default=1) - 1
    output_tail = b""
    last_output_at = time.monotonic()

    try:
        while True:
            remaining_seconds = idle_timeout_seconds - (
                time.monotonic() - last_output_at
            )
            if remaining_seconds <= 0:
                print(
                    f"\noutput contract idle timeout after "
                    f"{idle_timeout_seconds} seconds",
                    file=sys.stderr,
                    flush=True,
                )
                stop_process_group(process)
                return IDLE_TIMEOUT_EXIT_CODE
            try:
                chunk = chunks.get(timeout=min(1.0, remaining_seconds))
            except queue.Empty:
                continue
            if chunk is None:
                return process.wait()

            write_output(chunk)
            last_output_at = time.monotonic()
            searchable = output_tail + chunk
            for index, marker in enumerate(encoded_markers):
                if not marker_seen[index] and marker in searchable:
                    marker_seen[index] = True
            output_tail = searchable[-marker_overlap:] if marker_overlap else b""

            if marker_seen and all(marker_seen):
                print(
                    "\noutput contract satisfied; stopping owned command",
                    file=sys.stderr,
                    flush=True,
                )
                stop_process_group(process)
                return 0
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)
        if process.poll() is None:
            stop_process_group(process)
        process.stdout.close()
        reader.join(timeout=1)


def main() -> int:
    args = parse_args(sys.argv[1:])
    return run(
        args.command,
        args.idle_timeout_seconds,
        args.success_marker,
    )


if __name__ == "__main__":
    raise SystemExit(main())
