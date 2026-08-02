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
STALL_TIMEOUT_EXIT_CODE = 121
MAX_IDLE_TIMEOUT_SECONDS = 3600
OUTPUT_QUEUE_CHUNKS = 128
READ_CHUNK_BYTES = 64 * 1024
TERMINATION_GRACE_SECONDS = 5
DIAGNOSTIC_TIMEOUT_SECONDS = 60


def run_stall_diagnostics(diagnostic_command: str | None, child_pid: int) -> None:
    if not diagnostic_command:
        return
    environment = dict(os.environ, REPROIT_STALLED_PID=str(child_pid))
    try:
        completed = subprocess.run(
            ["bash", "-c", diagnostic_command],
            env=environment,
            capture_output=True,
            timeout=DIAGNOSTIC_TIMEOUT_SECONDS,
        )
        sys.stderr.buffer.write(completed.stdout + completed.stderr)
        sys.stderr.flush()
    except (OSError, subprocess.TimeoutExpired) as error:
        print(f"stall diagnostics failed: {error}", file=sys.stderr, flush=True)


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
    # A stall marker names the point after which the NEXT output is the thing
    # being waited for. Once the marker appears, that gap is bounded by the
    # tighter stall timeout instead of the generic idle timeout, and a trip is
    # reported by name with exit code 121 so callers can retry the specific
    # stall. Any later output re-arms nothing: the generic idle timeout governs
    # again until the marker reappears.
    parser.add_argument(
        "--stall-marker",
        help="after this marker appears, bound the wait for the next output",
    )
    parser.add_argument(
        "--stall-timeout-seconds",
        type=positive_timeout,
        help="bound on the next-output gap once the stall marker has appeared",
    )
    parser.add_argument(
        "--stall-name",
        default="post-marker",
        help="name used when reporting a stall timeout",
    )
    # The stalled process is the evidence; killing it first destroys the only
    # chance to see WHERE it is stuck. This hook runs while the child is still
    # alive, bounded, best-effort, and can never change the contract verdict.
    parser.add_argument(
        "--stall-diagnostic-command",
        help="bash command run before terminating a timed-out child; "
        "receives the child pid as REPROIT_STALLED_PID",
    )
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if (args.stall_marker is None) != (args.stall_timeout_seconds is None):
        parser.error("--stall-marker and --stall-timeout-seconds go together")
    if args.command[:1] == ["--"]:
        args.command = args.command[1:]
    if not args.command:
        parser.error("a command is required after --")
    return args


def signal_process_group(process: subprocess.Popen[bytes], signum: int) -> None:
    # A child that exits between poll() and killpg() leaves a zombie group
    # leader; on macOS killpg then raises EPERM (not ESRCH). That crashed the
    # contract mid-success and reported a pass as exit 1, so fall back to
    # signalling the leader directly and let wait() reap it.
    try:
        os.killpg(process.pid, signum)
    except (ProcessLookupError, PermissionError):
        try:
            process.send_signal(signum)
        except (ProcessLookupError, PermissionError):
            pass


def stop_process_group(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    if os.name == "nt":
        process.terminate()
    else:
        signal_process_group(process, signal.SIGTERM)
    try:
        process.wait(timeout=TERMINATION_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        if os.name == "nt":
            process.kill()
        else:
            signal_process_group(process, signal.SIGKILL)
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
    stall_marker: str | None = None,
    stall_timeout_seconds: int | None = None,
    stall_name: str = "post-marker",
    stall_diagnostic_command: str | None = None,
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
    encoded_stall_marker = stall_marker.encode() if stall_marker else None
    overlap_sources = list(encoded_markers)
    if encoded_stall_marker:
        overlap_sources.append(encoded_stall_marker)
    marker_overlap = max((len(marker) for marker in overlap_sources), default=1) - 1
    stall_armed = False
    output_tail = b""
    last_output_at = time.monotonic()

    try:
        while True:
            timeout_seconds = (
                stall_timeout_seconds
                if stall_armed and stall_timeout_seconds is not None
                else idle_timeout_seconds
            )
            remaining_seconds = timeout_seconds - (
                time.monotonic() - last_output_at
            )
            if remaining_seconds <= 0:
                if stall_armed and stall_timeout_seconds is not None:
                    print(
                        f"\noutput contract {stall_name} stall after "
                        f"{stall_timeout_seconds} seconds",
                        file=sys.stderr,
                        flush=True,
                    )
                    run_stall_diagnostics(stall_diagnostic_command, process.pid)
                    stop_process_group(process)
                    return STALL_TIMEOUT_EXIT_CODE
                print(
                    f"\noutput contract idle timeout after "
                    f"{idle_timeout_seconds} seconds",
                    file=sys.stderr,
                    flush=True,
                )
                run_stall_diagnostics(stall_diagnostic_command, process.pid)
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
            # This chunk IS the next output the armed stall bound was waiting
            # for; disarm before scanning so a fresh marker in the same chunk
            # re-arms for the gap that follows it.
            stall_armed = False
            searchable = output_tail + chunk
            for index, marker in enumerate(encoded_markers):
                if not marker_seen[index] and marker in searchable:
                    marker_seen[index] = True
            if encoded_stall_marker and encoded_stall_marker in searchable:
                stall_armed = True
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
        args.stall_marker,
        args.stall_timeout_seconds,
        args.stall_name,
        args.stall_diagnostic_command,
    )


if __name__ == "__main__":
    raise SystemExit(main())
