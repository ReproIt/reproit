"""Production capture mode: config-gated upload of complete failed operation
traces to the Repro It Cloud ingest endpoint
(`/v1/capture-batches`).

Python port of sdk/reproit-backend-rs/src/capture.rs. Scan-time tracing stays
untouched: this module only adds a place to hand a finished BackendTrace when
no `x-reproit-trace` header exists. A stable 5xx or marked agent oracle,
complete effects, and a pre-operation replay seed are required before queueing.

Everything is bounded and capture failure is invisible to the host app: a
fixed-depth queue drops oldest on overflow, batches and retries are capped,
uploads run on one daemon thread via stdlib urllib, and `record` never blocks
or raises.
"""

import itertools
import os
import platform
import random
import secrets
import sys
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
# Version stamped when any event carries a captured dependency `exchange` or
# an envelope stamp. Older readers reject it with a named version error
# instead of silently evaluating a payload whose replay semantics they do not
# understand.
CAPTURE_VERSION_EXCHANGES = 2
# First-class registry oracle id for an operation that returned HTTP 5xx.
SERVER_ERROR_ORACLE = "backend-server-error"
# Agent oracle vocabulary (registry ids, lowest confidence tier): authored
# assertions an LLM/agent operation marks on its own trace via
# `trace.oracle(id, detail)`. A marked operation is always captured and its
# failure observation carries the marked id instead of the 5xx default.
AGENT_RESPONSE_ORACLE = "agent-response-content"
AGENT_GUARDRAIL_ORACLE = "agent-guardrail-violation"
AGENT_LOOP_BOUND_ORACLE = "agent-loop-bound-exceeded"
AGENT_ORACLES = (
    AGENT_RESPONSE_ORACLE,
    AGENT_GUARDRAIL_ORACLE,
    AGENT_LOOP_BOUND_ORACLE,
)
# The effect resource that carries an oracle marker on the trace. A marker is
# an `emit` effect so the scan-time wire shape stays inside the existing
# event vocabulary.
ORACLE_MARKER_RESOURCE = "reproit-oracle"


def marked_oracle(events):
    """First agent oracle marked on a finished trace's events, or None."""
    for event in events or []:
        if (
            isinstance(event, dict)
            and event.get("kind") == "effect"
            and event.get("resource") == ORACLE_MARKER_RESOURCE
            and event.get("key") in AGENT_ORACLES
        ):
            return event["key"]
    return None


def _portable_operation(events, returned, status):
    marked = marked_oracle(events)
    if marked is None and (status is None or status < 500):
        return False
    if returned.get("effectsComplete") is not True:
        return False
    first = events[0] if events else {}
    replay_seed = first.get("replaySeed") if isinstance(first, dict) else None
    if not isinstance(replay_seed, str) or len(replay_seed) != 16:
        return False
    if any(character not in "0123456789abcdef" for character in replay_seed):
        return False
    for event in events:
        if not isinstance(event, dict) or event.get("kind") != "effect":
            continue
        if event.get("effect") not in ("call", "read", "write", "delete"):
            continue
        if not isinstance(event.get("exchange"), dict):
            return False
    return True

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


def determinism_envelope(observed_at_ms=None, replay_seed=None):
    """Where and when the capture happened, and a seed that makes REPLAY runs
    deterministic. Honesty note: the seed does not reproduce the randomness
    the app drew in production; it pins the replay's."""
    envelope = {
        "observedAtMs": observed_at_ms
        if isinstance(observed_at_ms, int)
        else int(time.time() * 1000),
        "tz": time.tzname[0] if time.tzname else "UTC",
        "runtime": "python " + platform.python_version(),
        "os": sys.platform,
        "arch": platform.machine(),
        "replaySeed": replay_seed
        if isinstance(replay_seed, str) and len(replay_seed) == 16
        else secrets.token_hex(8),
    }
    image_digest = os.environ.get("REPROIT_IMAGE_DIGEST")
    if _valid_token(image_digest):
        envelope["imageDigest"] = image_digest
    return envelope


def _payload_version(events):
    for event in events:
        if not isinstance(event, dict):
            continue
        if event.get("exchange") or "at" in event or "monoNs" in event:
            return CAPTURE_VERSION_EXCHANGES
    return CAPTURE_VERSION


def _capture_payload(operation, envelope=None):
    """The replayable capture object (`reproit debug replay-capture` input).
    Trailing effect events are dropped first when the payload exceeds the
    context budget; a payload that stays oversized with only start/return
    left is omitted entirely (None)."""
    events = list(operation["events"])
    oracle = marked_oracle(events) or SERVER_ERROR_ORACLE
    dropped = 0
    while True:
        payload = {
            "format": CAPTURE_FORMAT,
            "version": _payload_version(events),
            "operation": operation["operation"],
            "oracle": oracle,
            "events": events,
        }
        if envelope is not None:
            payload["envelope"] = envelope
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
    def resolve_commit(cls, commit=None, env=None):
        """Code identity, in priority order: explicit config, then the common
        CI and platform environment. Never shells out to git."""
        environment = os.environ if env is None else env
        for candidate in (commit, environment.get("REPROIT_COMMIT"), environment.get("GITHUB_SHA")):
            if _valid_token(candidate):
                return candidate
        return None

    @classmethod
    def create(
        cls,
        endpoint,
        api_key,
        app_id,
        build=None,
        commit=None,
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
        if commit is not None and not _valid_token(commit):
            return None
        try:
            return cls(
                endpoint,
                api_key,
                app_id,
                build,
                cls.resolve_commit(commit),
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
        commit,
        healthy_sample_per_mille,
        flush_interval_ms,
        request_timeout_ms,
        retry_limit,
    ):
        self._endpoint = endpoint
        self._api_key = api_key
        self._app_id = app_id
        self._build = build
        self._commit = commit
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
            # Capture-mode traces stamp per-event wall-clock and monotonic
            # offsets (the determinism envelope); scan-time traces never do.
            "capture_envelope": True,
            "replay_seed": secrets.token_hex(8),
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
            status = returned.get("status")
            if isinstance(status, bool) or not (
                isinstance(status, int) and 0 <= status <= 0xFFFF
            ):
                status = None
            if not _portable_operation(events, returned, status):
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
                    while len(self._queue) < 1 and not self._flush_now:
                        remaining = deadline - time.monotonic()
                        if remaining <= 0:
                            break
                        if not self._signal.wait(remaining):
                            break
                    self._flush_now = False
                    take = min(len(self._queue), 1)
                    self._sending = True
                    return [self._queue.popleft() for _ in range(take)]
                self._flush_now = False
                self._signal.wait()

    def _build_batch(self, operations):
        """Build one source-neutral capture-batch-v1 payload."""
        if len(operations) != 1:
            raise ValueError("a causal capture batch must contain exactly one operation")
        operation = operations[0]
        batch_id = "cb-python-%d-%d" % (int(time.time() * 1000), next(self._batch_seq))
        source_events = operation["events"]
        first = source_events[0] if source_events else {}
        session_id = first.get("traceId") or batch_id
        events = []
        parent = None

        def event(kind, source=None):
            nonlocal parent
            sequence = len(events) + 1
            event_id = "evt_backend-python_%d" % sequence
            # Real monotonic offsets from the trace's envelope stamps; the
            # ordinal fallback only applies to traces recorded without
            # capture mode.
            stamped = (source or {}).get("monoNs")
            item = {
                "id": event_id,
                "sequence": sequence,
                "monotonicNs": stamped if isinstance(stamped, int) else sequence,
                "causalParentIds": [] if parent is None else [parent],
                "event": kind,
            }
            trace_id = first.get("traceId")
            if isinstance(trace_id, str) and trace_id:
                item["traceId"] = trace_id
            events.append(item)
            parent = event_id

        event({"kind": "operation-start", "name": operation["operation"]}, first)
        input_value = first.get("input")
        value = {
            "representation": "replayable",
            "value": input_value,
            "redaction": "redacted-at-source",
        }
        event(
            {
                "kind": "trigger",
                "trigger": "http-request",
                "subject": operation["operation"],
                "value": value,
            },
            first,
        )
        # Determinism envelope: where and when the capture happened, and the
        # seed that makes a replay of it repeatable.
        event(
            {
                "kind": "checkpoint",
                "name": "determinism-envelope",
                "attributes": determinism_envelope(
                    first.get("at") if isinstance(first.get("at"), int) else None,
                    first.get("replaySeed"),
                ),
            },
            first,
        )
        for source in source_events:
            if source.get("kind") != "effect":
                continue
            effect = source.get("effect") or "backend-effect"
            subject = source.get("resource") or source.get("service") or operation["operation"]
            exchange = source.get("exchange")
            value = (
                {
                    "representation": "replayable",
                    "value": source,
                    "redaction": "redacted-at-source",
                }
                if isinstance(exchange, dict)
                else {
                    "representation": "structural",
                    "shape": {"effect": effect, "subject": subject},
                }
            )
            if effect == "call":
                causal = {
                    "kind": "dependency",
                    "system": "service",
                    "operation": "call",
                    "subject": subject,
                    "value": value,
                }
            elif effect in ("read", "write", "delete"):
                causal = {
                    "kind": "state-access",
                    "state": "database",
                    "operation": effect,
                    "subject": subject,
                    "value": value,
                }
            else:
                causal = {
                    "kind": "effect",
                    "effect": effect,
                    "subject": subject,
                    "value": value,
                }
            event(
                causal,
                source,
            )
        returned = next(
            (item for item in reversed(source_events) if item.get("kind") == "return"),
            {},
        )
        # Nest the raw return event exactly like the raw effect events, so
        # the batch can be projected back to a replayable backend capture.
        # The subject names the carrier: `backend_capture_from_batch` in
        # reproit-protocol keys the inversion on "operation-return".
        if returned:
            event(
                {
                    "kind": "effect",
                    "effect": "operation-return",
                    "subject": "operation-return",
                    "value": {
                        "representation": "replayable",
                        "value": returned,
                        "redaction": "redacted-at-source",
                    },
                },
                returned,
            )
        event(
            {
                "kind": "operation-end",
                "name": operation["operation"],
                "outcome": "succeeded" if returned.get("success") is True else "failed",
            },
            returned,
        )
        status = operation["status"]
        marked = marked_oracle(source_events)
        if marked is not None or (status is not None and status >= 500):
            oracle = marked or SERVER_ERROR_ORACLE
            if marked is None:
                message = "backend operation %s returned HTTP %d" % (
                    operation["operation"],
                    status,
                )
            else:
                message = "agent oracle %s fired on %s" % (oracle, operation["operation"])
            event(
                {
                    "kind": "observation",
                    "failure": {
                        # A marked agent oracle is an authored assertion (a
                        # declared contract the trace itself violated); a bare
                        # 5xx stays the runtime exception it always was.
                        "observation": "exception" if marked is None else "contract-violation",
                        "authority": "runtime-diagnosis",
                        "summary": message,
                        "signature": oracle + ":" + operation["operation"],
                        "observationPoint": operation["operation"],
                        "artifactIds": [],
                    },
                },
                returned,
            )
        batch = {
            "version": 1,
            "batchId": batch_id,
            "projectId": self._app_id,
            "sessionId": session_id,
            "emitter": {
                "id": "backend-python",
                "kind": "runtime-sdk",
                "component": "backend",
                "runtime": "python",
            },
            "observedAt": "%d" % int(time.time() * 1000),
            "policy": {
                "consent": "application-telemetry",
                "retentionClass": "standard",
            },
            "capabilities": [{"capability": "http", "completeness": "complete"}],
            "events": events,
            "artifacts": [],
        }
        # Declared only when the instrument layer actually recorded
        # exchanges, so the capsule completeness model never over-claims on
        # captures from apps without the outbound hooks installed.
        if any(
            isinstance(item, dict)
            and item.get("effect") == "call"
            and isinstance(item.get("exchange"), dict)
            for item in source_events
        ):
            batch["capabilities"].append(
                {
                    "capability": "network",
                    "completeness": "complete",
                    "detail": "outbound dependency exchanges recorded with responses",
                }
            )
        if any(
            isinstance(item, dict)
            and item.get("effect") in ("read", "write", "delete")
            and isinstance(item.get("exchange"), dict)
            for item in source_events
        ):
            batch["capabilities"].append(
                {"capability": "database", "completeness": "complete"}
            )
        deployment = {}
        if self._build is not None:
            deployment["version"] = self._build
        if self._commit is not None:
            deployment["commit"] = self._commit
        if deployment:
            batch["deployment"] = deployment
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
