#!/usr/bin/env python3
"""Run one registered native backend gate and emit bounded evidence."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import signal
import subprocess
import sys
import threading
from collections import deque
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
MANIFEST_PATH = ROOT / "validation/backends/evidence.json"
DEFAULT_OUTPUT_DIR = ROOT / "target/reproit-validation"
MAX_CAPTURE_BYTES = 16 * 1024 * 1024


def load_gate(gate_id: str) -> dict[str, Any]:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    if manifest.get("schema") != 2:
        raise ValueError("unsupported backend evidence manifest schema")
    gates = manifest.get("gates")
    if not isinstance(gates, dict) or gate_id not in gates:
        known = ", ".join(sorted(gates or {}))
        raise ValueError(f"unknown gate {gate_id!r}; expected one of: {known}")
    gate = gates[gate_id]
    if not isinstance(gate, dict):
        raise ValueError(f"gate {gate_id!r} is not an object")
    return gate


def git_commit() -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    )
    commit = result.stdout.strip()
    if len(commit) != 40 or any(character not in "0123456789abcdef" for character in commit):
        raise ValueError(f"git returned invalid commit {commit!r}")
    return commit


def timestamp() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


class BoundedCapture:
    def __init__(self) -> None:
        self.chunks: deque[bytes] = deque()
        self.total_bytes = 0
        self.captured_bytes = 0

    def append(self, chunk: bytes) -> None:
        self.total_bytes += len(chunk)
        self.captured_bytes += len(chunk)
        self.chunks.append(chunk)
        while self.captured_bytes > MAX_CAPTURE_BYTES:
            overflow = self.captured_bytes - MAX_CAPTURE_BYTES
            first = self.chunks[0]
            if len(first) <= overflow:
                self.captured_bytes -= len(self.chunks.popleft())
            else:
                self.chunks[0] = first[overflow:]
                self.captured_bytes -= overflow

    def output(self) -> bytes:
        combined = b"".join(self.chunks)
        if self.total_bytes <= MAX_CAPTURE_BYTES:
            return combined
        marker = b"\n[reproit gate output truncated to final 16 MiB]\n"
        return marker + combined[-(MAX_CAPTURE_BYTES - len(marker)) :]


def stream_output(stream: Any, capture: BoundedCapture) -> None:
    read_chunk = getattr(stream, "read1", stream.read)
    while chunk := read_chunk(64 * 1024):
        capture.append(chunk)
        try:
            sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()
        except BrokenPipeError:
            pass


def stop_process_group(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    if os.name == "nt":
        process.kill()
        return
    os.killpg(process.pid, signal.SIGKILL)


def execute(command: list[str], timeout_seconds: int) -> tuple[str, int | None, bytes]:
    process = subprocess.Popen(
        command,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=os.name != "nt",
    )
    assert process.stdout is not None
    capture = BoundedCapture()
    reader = threading.Thread(
        target=stream_output,
        args=(process.stdout, capture),
        daemon=True,
    )
    reader.start()
    try:
        process.wait(timeout=timeout_seconds)
        status = "passed" if process.returncode == 0 else "failed"
    except subprocess.TimeoutExpired:
        stop_process_group(process)
        process.wait()
        status = "timed-out"
    except KeyboardInterrupt:
        stop_process_group(process)
        process.wait()
        raise
    finally:
        reader.join(timeout=10)
        if reader.is_alive():
            process.stdout.close()
            reader.join(timeout=1)
    exit_code = process.returncode if status != "timed-out" else None
    return status, exit_code, capture.output()


def validate_gate_fields(gate_id: str, gate: dict[str, Any]) -> tuple[list[str], int]:
    command = gate.get("command")
    if not isinstance(command, list) or not command or not all(
        isinstance(part, str) and part for part in command
    ):
        raise ValueError(f"gate {gate_id!r} has an invalid command")
    timeout_seconds = gate.get("timeoutSeconds")
    if not isinstance(timeout_seconds, int) or not 1 <= timeout_seconds <= 7200:
        raise ValueError(f"gate {gate_id!r} has an invalid timeout")
    required = gate.get("requiredOutput")
    if not isinstance(required, list) or not required or not all(
        isinstance(marker, str) and marker for marker in required
    ):
        raise ValueError(f"gate {gate_id!r} has invalid required output markers")
    return command, timeout_seconds


def write_result(
    gate_id: str,
    gate: dict[str, Any],
    architectures: list[str],
    output_dir: Path,
    started_at: str,
    finished_at: str,
    status: str,
    exit_code: int | None,
    output: bytes,
) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    log_path = output_dir / f"{gate_id}.log"
    result_path = output_dir / f"{gate_id}.json"
    log_path.write_bytes(output)
    decoded = output.decode("utf-8", errors="replace")
    required = gate["requiredOutput"]
    checks = {marker: marker in decoded for marker in required}
    if not all(checks.values()) and status == "passed":
        status = "failed"
    try:
        recorded_log_path = str(log_path.relative_to(ROOT))
    except ValueError:
        recorded_log_path = str(log_path)
    result = {
        "schema": 1,
        "gateId": gate_id,
        "commit": git_commit(),
        "startedAt": started_at,
        "finishedAt": finished_at,
        "executor": {
            "os": platform.system().lower(),
            "architecture": platform.machine().lower(),
        },
        "targetOs": gate["targetOs"],
        "architectures": architectures,
        "fixture": gate["fixture"],
        "command": gate["command"],
        "status": status,
        "exitCode": exit_code,
        "checks": checks,
        "resetStrategy": gate["resetStrategy"],
        "cleanupStrategy": gate["cleanupStrategy"],
        "logSha256": hashlib.sha256(output).hexdigest(),
        "logPath": recorded_log_path,
    }
    temporary = result_path.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    temporary.replace(result_path)
    return result_path


def run(gate_id: str, architectures: list[str] | None, output_dir: Path) -> int:
    gate = load_gate(gate_id)
    command, timeout_seconds = validate_gate_fields(gate_id, gate)
    recorded_architectures = architectures or gate["architectures"]
    started_at = timestamp()
    status, exit_code, output = execute(command, timeout_seconds)
    finished_at = timestamp()
    result_path = write_result(
        gate_id,
        gate,
        recorded_architectures,
        output_dir,
        started_at,
        finished_at,
        status,
        exit_code,
        output,
    )
    result = json.loads(result_path.read_text(encoding="utf-8"))
    print(f"native gate result: {result_path}")
    return 0 if result["status"] == "passed" else 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("gate_id")
    parser.add_argument(
        "--architecture",
        action="append",
        dest="architectures",
        help="target architecture exercised; repeat for a multi-architecture gate",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(os.environ.get("REPROIT_GATE_OUTPUT_DIR", DEFAULT_OUTPUT_DIR)),
    )
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    try:
        architectures = arguments.architectures
        if architectures is not None and (
            len(architectures) > 8
            or any(not value or len(value) > 32 for value in architectures)
        ):
            raise ValueError("architecture overrides must contain 1 to 8 short values")
        return run(arguments.gate_id, architectures, arguments.output_dir.resolve())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"native gate configuration error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
