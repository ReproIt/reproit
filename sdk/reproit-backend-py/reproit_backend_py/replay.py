"""Hermetic replay mode for reproit-backend-py.

When `REPROIT_REPLAY` names a `reproit-backend-capture` payload, the same
hooks that record exchanges at capture time SERVE them instead: outbound HTTP
is answered in process from the recorded exchanges, and the generic database
hook returns recorded results, so the application code re-executes against
exactly what production saw, with no live dependencies.

Determinism is a contract here, not a similarity score. Matching is strict:
the next unconsumed exchange of the same protocol, compared on method and
path plus query (recorded `$reproit` redaction placeholders match any value
at their position). The first unmatched call is a DIVERGENCE: it is reported
as a structured `REPROIT:DIVERGENCE` line on stderr and the call fails with
status 599 (HTTP) or a raised error (database), never a fuzzy match.

The envelope pins the replay's determinism: `TZ` from the capture, and
`random` seeded from `replaySeed`. Honesty note: the seed makes REPLAY runs
deterministic; it does not reproduce the randomness the app drew in
production.

Python port of sdk/reproit-backend-node/replay.js.
"""

import json
import os
import random
import sys
import time
import urllib.parse

DIVERGENCE_MARKER = "REPROIT:DIVERGENCE "
CAPTURE_FORMAT = "reproit-backend-capture"
# Version 1 carries events only; version 2 additionally carries dependency
# `exchange` records on effect events (the hermetic-replay inputs).
SUPPORTED_VERSIONS = (1, 2)


class ReplaySession:
    """Recorded exchanges plus the strict matcher that serves them."""

    @classmethod
    def load(cls, path):
        with open(path, "r", encoding="utf-8") as handle:
            payload = json.load(handle)
        if payload.get("format") != CAPTURE_FORMAT:
            raise TypeError("REPROIT_REPLAY file is not a reproit-backend-capture payload")
        version = payload.get("version")
        if version not in SUPPORTED_VERSIONS:
            raise TypeError("unsupported capture version %r" % (version,))
        return cls(payload)

    def __init__(self, payload):
        self.payload = payload
        self.envelope = payload.get("envelope")
        self.exchanges = [
            {"exchange": event["exchange"], "consumed": False}
            for event in payload.get("events") or []
            if isinstance(event, dict)
            and event.get("kind") == "effect"
            and isinstance(event.get("exchange"), dict)
        ]
        self.diverged = False

    def match(self, protocol, probe):
        """Strict next-unconsumed match. Returns the exchange or None."""
        matcher = _http_request_matcher if protocol == "http" else _db_request_matcher
        for entry in self.exchanges:
            if entry["consumed"] or entry["exchange"].get("protocol") != protocol:
                continue
            if matcher(entry["exchange"].get("request") or {}, probe):
                entry["consumed"] = True
                return entry["exchange"]
            # Strict ordering within a protocol: the first unconsumed
            # exchange is the only candidate; skipping it would be a fuzzy
            # match.
            break
        self.diverge(protocol, probe)
        return None

    def diverge(self, protocol, probe):
        self.diverged = True
        expected = next(
            (
                entry["exchange"].get("request")
                for entry in self.exchanges
                if not entry["consumed"] and entry["exchange"].get("protocol") == protocol
            ),
            None,
        )
        report = {
            "protocol": protocol,
            "got": probe,
            "expected": expected,
            "consumed": sum(1 for entry in self.exchanges if entry["consumed"]),
            "total": len(self.exchanges),
        }
        sys.stderr.write(DIVERGENCE_MARKER + json.dumps(report, sort_keys=True) + "\n")
        sys.stderr.flush()


def matches(recorded, live):
    """A recorded value matches a live one when equal, or when the recorded
    side is a `$reproit` redaction placeholder (any value stood here at
    capture). Mappings compare per key."""
    if recorded is None:
        return True
    if isinstance(recorded, dict):
        if "$reproit" in recorded:
            return True
        if not isinstance(live, dict):
            return False
        return all(matches(value, live.get(key)) for key, value in recorded.items())
    if isinstance(recorded, (list, tuple)):
        if not isinstance(live, (list, tuple)) or len(live) != len(recorded):
            return False
        return all(matches(item, live[index]) for index, item in enumerate(recorded))
    return recorded == live


def url_path_and_query(url):
    try:
        parsed = urllib.parse.urlsplit(str(url))
        if not parsed.path and not parsed.query:
            return str(url)
        return parsed.path + (("?" + parsed.query) if parsed.query else "")
    except ValueError:
        return str(url)


def _http_request_matcher(recorded_request, probe):
    """Method and path plus query of the original URL, and body modulo
    redaction placeholders. Recorded headers are deliberately not matched:
    they carry per-run noise (dates, connection management) that would turn
    every replay into a divergence."""
    if recorded_request.get("method") != probe.get("method"):
        return False
    if url_path_and_query(recorded_request.get("url")) != url_path_and_query(probe.get("url")):
        return False
    return matches(recorded_request.get("body"), probe.get("body"))


def _db_request_matcher(recorded_request, probe):
    """Exact statement text, values modulo placeholders."""
    if recorded_request.get("text") != probe.get("text"):
        return False
    return matches(recorded_request.get("values"), probe.get("values"))


def serve_http(session, probe):
    """Resolve a live HTTP probe against the session, entirely in process.
    Returns `{status, headers, body_text}`; a divergence and a body that was
    truncated at capture both serve a hard 599 so the application observes an
    attributable failure instead of a guess."""
    recorded = session.match("http", probe)
    if recorded is None:
        return _diverged_599("diverged")
    response = recorded.get("response") or {}
    if response.get("truncated") is True:
        # The capture kept identity but not bytes; serving a guessed body
        # would be a silent lie. Fail closed with the named reason.
        diverged_probe = dict(probe)
        diverged_probe["truncated"] = True
        session.diverge("http", diverged_probe)
        return _diverged_599("truncated-exchange-body")
    headers = {
        name: value
        for name, value in (response.get("headers") or {}).items()
        if name.lower() not in ("content-length", "transfer-encoding", "content-encoding")
    }
    body = response.get("body")
    if body is None:
        body_text = ""
    elif isinstance(body, str):
        body_text = body
    else:
        body_text = json.dumps(body)
    return {
        "status": response.get("status") or 200,
        "headers": headers,
        "body_text": body_text,
    }


def _diverged_599(reason):
    return {
        "status": 599,
        "headers": {"content-type": "application/json"},
        "body_text": json.dumps({"reproit": reason}),
    }


def try_json(text, content_type):
    if isinstance(content_type, str) and "application/json" in content_type:
        try:
            return json.loads(text)
        except ValueError:
            return text
    return text


class _SeededRandom(random.Random):
    """xorshift64* over the recorded seed: a deterministic stream that makes
    REPLAY runs repeatable. It does not reproduce the randomness the app drew
    in production, and the module docstring says so."""

    def __init__(self, seed_hex):
        super().__init__()
        self._state = (int(seed_hex[:16].ljust(16, "0"), 16) | 1) & 0xFFFFFFFFFFFFFFFF

    def random(self):
        state = self._state
        state ^= (state << 13) & 0xFFFFFFFFFFFFFFFF
        state ^= state >> 7
        state ^= (state << 17) & 0xFFFFFFFFFFFFFFFF
        self._state = state
        return ((state * 0x2545F4914F6CDD1D & 0xFFFFFFFFFFFFFFFF) >> 11) / float(1 << 53)

    def getrandbits(self, k):
        if k <= 0:
            raise ValueError("number of bits must be greater than zero")
        bits = 0
        collected = 0
        while collected < k:
            bits = (bits << 32) | int(self.random() * (1 << 32))
            collected += 32
        return bits >> (collected - k)

    def seed(self, *args, **kwargs):
        # The envelope owns this stream; a library reseeding it would silently
        # break replay determinism.
        return None


def pin_envelope(envelope):
    """Pin process determinism from the capture envelope. Runs once at
    install. Returns the seeded Random when one was installed."""
    if not isinstance(envelope, dict):
        return None
    timezone = envelope.get("tz")
    if isinstance(timezone, str) and timezone:
        os.environ["TZ"] = timezone
        if hasattr(time, "tzset"):
            time.tzset()
    seed_hex = envelope.get("replaySeed")
    if isinstance(seed_hex, str) and seed_hex:
        try:
            seeded = _SeededRandom(seed_hex)
        except ValueError:
            return None
        # Rebind the module-level functions the stdlib exposes, which is what
        # application code reaches for.
        random.random = seeded.random
        random.getrandbits = seeded.getrandbits
        random.randint = seeded.randint
        random.choice = seeded.choice
        random.uniform = seeded.uniform
        random.shuffle = seeded.shuffle
        return seeded
    return None
