"""Production capture mode: config-gated self-sampling upload of finished
operation traces to the Reproit Cloud ingest endpoint (`/v1/events`).

Python port of sdk/reproit-backend-rs/src/capture.rs. Scan-time tracing stays
untouched: this module only adds a place to hand a finished BackendTrace when
no `x-reproit-trace` header exists. Operations that end in a server error
(HTTP 5xx) or report `success == False` are always captured; healthy
operations only under an optional per-mille baseline sample (default 0).

Everything is bounded and capture failure is invisible to the host app: a
fixed-depth queue drops oldest on overflow, batches and retries are capped,
uploads run on one daemon thread via stdlib urllib, and `record` never blocks
or raises.
"""

import itertools
import random
import threading
import time
import urllib.error
import urllib.request
from collections import deque

from .trace import canonical_json

# Payload format identifier of the replayable capture object attached to the
# finding context (`context.reproitCapture`).
CAPTURE_FORMAT = "reproit-backend-capture"
CAPTURE_VERSION = 1
# First-class registry oracle id for an operation that returned HTTP 5xx.
SERVER_ERROR_ORACLE = "backend-server-error"

# Bounds. Queue overflow drops the OLDEST pending operation; an oversized
# capture payload drops trailing effect events before it drops itself.
MAX_QUEUE_OPERATIONS = 64
MAX_BATCH_OPERATIONS = 16
MAX_CAPTURE_JSON_BYTES = 48 * 1024
MIN_FLUSH_INTERVAL_MS = 100
MAX_RETRY_LIMIT = 5

_TOKEN_CHARS = frozenset(
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.:"
)


def _valid_token(value):
    """The ingest protocol token charset (`validate_token` in reproit-protocol)."""
    return (
        isinstance(value, str)
        and 0 < len(value) <= 128
        and all(ch in _TOKEN_CHARS for ch in value)
    )


def _capture_payload(operation):
    """The replayable capture object (`reproit debug replay-capture` input).
    Trailing effect events are dropped first when the payload exceeds the
    context budget; a payload that stays oversized with only start/return
    left is omitted entirely (None)."""
    events = list(operation["events"])
    dropped = 0
    while True:
        payload = {
            "format": CAPTURE_FORMAT,
            "version": CAPTURE_VERSION,
            "operation": operation["operation"],
            "oracle": SERVER_ERROR_ORACLE,
            "events": events,
        }
        if len(canonical_json(payload).encode("utf-8")) <= MAX_CAPTURE_JSON_BYTES:
            return payload, dropped
        last_effect = None
        for index in range(len(events) - 1, -1, -1):
            if isinstance(events[index], dict) and events[index].get("kind") == "effect":
                last_effect = index
                break
        if last_effect is None:
            return None, dropped
        del events[last_effect]
        dropped += 1


class Capture:
    """Handle to the capture worker. Thread-safe; one queue, one upload thread."""

    @classmethod
    def create(
        cls,
        endpoint,
        api_key,
        app_id,
        build=None,
        healthy_sample_per_mille=0,
        flush_interval_ms=3000,
        request_timeout_ms=5000,
        retry_limit=2,
    ):
        """Start capture mode. Returns None (capture disabled, host unaffected)
        when the config is unusable: empty endpoint/key or identifiers the
        ingest protocol would reject."""
        if not isinstance(endpoint, str) or not endpoint.strip():
            return None
        if not isinstance(api_key, str) or not api_key.strip():
            return None
        if not _valid_token(app_id):
            return None
        if build is not None and not _valid_token(build):
            return None
        try:
            return cls(
                endpoint,
                api_key,
                app_id,
                build,
                max(0, int(healthy_sample_per_mille)),
                max(MIN_FLUSH_INTERVAL_MS, int(flush_interval_ms)),
                int(request_timeout_ms),
                min(MAX_RETRY_LIMIT, max(0, int(retry_limit))),
            )
        except (ValueError, TypeError, RuntimeError):
            return None

    def __init__(
        self,
        endpoint,
        api_key,
        app_id,
        build,
        healthy_sample_per_mille,
        flush_interval_ms,
        request_timeout_ms,
        retry_limit,
    ):
        self._endpoint = endpoint
        self._api_key = api_key
        self._app_id = app_id
        self._build = build
        self._healthy_sample_per_mille = healthy_sample_per_mille
        self._flush_interval = flush_interval_ms / 1000.0
        self._request_timeout = request_timeout_ms / 1000.0
        self._retry_limit = retry_limit
        self._lock = threading.Lock()
        self._signal = threading.Condition(self._lock)
        self._queue = deque()
        self._sending = False
        self._flush_now = False
        self._trace_seq = itertools.count(1)
        self._batch_seq = itertools.count(1)
        self._stats = {
            "captured_operations": 0,
            "dropped_operations": 0,
            "sent_batches": 0,
            "failed_batches": 0,
        }
        worker = threading.Thread(target=self._run_worker, name="reproit-capture", daemon=True)
        worker.start()

    def context(self):
        """Synthesized trace context for capture-mode operations, replacing the
        scan-time `x-reproit-trace` header requirement."""
        return {
            "trace_id": "cap-%d-%d" % (int(time.time() * 1000), next(self._trace_seq)),
            "actor": None,
            "action_index": 0,
            "build": self._build,
            "config_contract": None,
        }

    def record(self, trace):
        """Hand a finished trace to the sampler. Unfinished traces are ignored.
        Never blocks and never fails visibly; overflow drops the oldest
        queued operation."""
        try:
            events = trace.events()
            returned = next(
                (
                    event
                    for event in reversed(events)
                    if isinstance(event, dict) and event.get("kind") == "return"
                ),
                None,
            )
            if returned is None:
                return
            success = returned.get("success", True)
            status = returned.get("status")
            if isinstance(status, bool) or not (
                isinstance(status, int) and 0 <= status <= 0xFFFF
            ):
                status = None
            error = success is False or (status is not None and status >= 500)
            if not error and not self._sample_healthy():
                return
            operation = events[0].get("operation") if events else None
            if not isinstance(operation, str):
                return
            captured = {"operation": operation, "status": status, "events": list(events)}
            with self._signal:
                self._stats["captured_operations"] += 1
                self._queue.append(captured)
                if len(self._queue) > MAX_QUEUE_OPERATIONS:
                    self._queue.popleft()
                    self._stats["dropped_operations"] += 1
                self._signal.notify_all()
        except Exception:
            # Capture must never surface errors into the host app.
            pass

    def flush(self, timeout):
        """Block up to `timeout` seconds until every queued operation has been
        sent (or dropped). Returns False on timeout. Intended for tests,
        examples, and graceful shutdown."""
        deadline = time.monotonic() + timeout
        with self._signal:
            self._flush_now = True
            self._signal.notify_all()
            while self._queue or self._sending:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return False
                self._signal.wait(remaining)
            return True

    def stats(self):
        with self._lock:
            return dict(self._stats)

    def _sample_healthy(self):
        per_mille = self._healthy_sample_per_mille
        if per_mille <= 0:
            return False
        if per_mille >= 1000:
            return True
        return random.random() * 1000 < per_mille

    def _run_worker(self):
        while True:
            operations = self._next_batch()
            batch = self._build_batch(operations)
            sent = self._send(batch)
            with self._signal:
                if sent:
                    self._stats["sent_batches"] += 1
                else:
                    self._stats["failed_batches"] += 1
                    self._stats["dropped_operations"] += len(operations)
                self._sending = False
                self._signal.notify_all()

    def _next_batch(self):
        """Wait for work, gather up to the batch cap within one flush interval,
        then drain. `_flush_now` (set by `flush`) cuts the gather short."""
        with self._signal:
            while True:
                if self._queue:
                    deadline = time.monotonic() + self._flush_interval
                    while len(self._queue) < MAX_BATCH_OPERATIONS and not self._flush_now:
                        remaining = deadline - time.monotonic()
                        if remaining <= 0:
                            break
                        if not self._signal.wait(remaining):
                            break
                    self._flush_now = False
                    take = min(len(self._queue), MAX_BATCH_OPERATIONS)
                    self._sending = True
                    return [self._queue.popleft() for _ in range(take)]
                self._flush_now = False
                self._signal.wait()

    def _build_batch(self, operations):
        """Build one event-batch-v1 payload: every captured event ships as a
        `backend` frame, and each 5xx operation additionally ships a `finding`
        frame tagged `backend-server-error` whose context carries the full
        replayable capture object."""
        batch_id = "cap-%d-%d" % (int(time.time() * 1000), next(self._batch_seq))
        frames = []

        def frame(event):
            frames.append(
                {
                    "runId": batch_id,
                    "sequence": len(frames) + 1,
                    "scope": {"domain": "shared"},
                    "event": event,
                }
            )

        for operation in operations:
            for event in operation["events"]:
                frame({"kind": "backend", "evidence": event})
            status = operation["status"]
            if status is None or status < 500:
                continue
            signature = "backend:" + operation["operation"]
            message = "backend operation %s returned HTTP %d" % (operation["operation"], status)
            context = {"capture": "reproit-backend-py"}
            if self._build is not None:
                context["build"] = {"version": self._build}
            payload, dropped = _capture_payload(operation)
            if payload is None:
                context["captureOmitted"] = True
            else:
                context["reproitCapture"] = payload
                if dropped > 0:
                    context["captureDroppedEffects"] = dropped
            frame(
                {
                    "kind": "finding",
                    "signature": signature,
                    "message": message,
                    "identity": {
                        "oracle": SERVER_ERROR_ORACLE,
                        "invariant": "backend:server-error",
                        "kind": "server-error",
                        "message": message,
                        "frame": "",
                        "trigger": signature,
                        "boundary": signature,
                    },
                    "path": [],
                    "context": context,
                }
            )
        batch = {
            "version": 1,
            "batchId": batch_id,
            "appId": self._app_id,
            "frames": frames,
            "evidence": [],
        }
        if self._build is not None:
            batch["deployment"] = {"version": self._build}
        return batch

    def _send(self, batch):
        body = canonical_json(batch).encode("utf-8")
        for attempt in range(self._retry_limit + 1):
            try:
                request = urllib.request.Request(
                    self._endpoint,
                    data=body,
                    headers={
                        "Authorization": "Bearer " + self._api_key,
                        "Content-Type": "application/json",
                    },
                    method="POST",
                )
                with urllib.request.urlopen(request, timeout=self._request_timeout):
                    return True
            except urllib.error.HTTPError as error:
                # A definitive client-side rejection cannot improve on retry.
                if 400 <= error.code < 500:
                    return False
            except Exception:
                pass
            if attempt < self._retry_limit:
                time.sleep((200 * attempt + 200) / 1000.0)
        return False
