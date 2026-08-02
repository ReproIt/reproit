"""CI capture mode for reproit-backend-py: the flaky-CI wedge.

`suite(name)` returns a pytest-compatible `test(name)` decorator factory whose
trigger identity is the TEST (suite + test id), not an inbound HTTP request.
With `REPROIT_CI_CAPTURE=1` every decorated test runs inside its own trace, so
the instrumented outbound clients (instrument.py) record dependency exchanges
and the determinism envelope exactly as production capture does; a FAILING
test emits a version-2 `reproit-backend-capture` capsule to a bounded on-disk
spool. With `REPROIT_REPLAY` set the SAME decorator re-runs only the capsule's
named test (every other decorated test is skipped) while the SDK serves the
recorded exchanges in process, and reports the observed result as a
structured stderr marker for `reproit check`. Without either env the
decorator is inert and the test function is returned untouched.

The wire is the existing capture payload: the test identity rides in the
`operation` field as `test:<suite>#<test>`, and the failed assertion is the
existing `backend-authored-invariant` registry oracle (a test IS an authored
invariant). No new protocol fields, no new oracle ids.

Runner note: run pytest with `-s` (capture disabled) in both modes. The
`REPROIT:CI-TEST` and `REPROIT:DIVERGENCE` markers are stderr lines
`reproit check` parses, and pytest's default output capture would swallow
them. This is the pytest analogue of the Node fixture's direct-invocation
note (run the file, not `node --test`, so no child process eats the markers).

Honest limit: replay pins the envelope and the recorded exchanges, which is
the whole boundary this SDK can see. A race the boundary cannot see
(scheduling, shared memory) is not reproduced by this capsule; `reproit
check` reports such runs Inconclusive, never a fake reproduction.

Python port of sdk/reproit-backend-node/ci.js.
"""

import asyncio
import functools
import hashlib
import inspect
import itertools
import json
import os
import sys
import time

from . import instrument
from .capture import CAPTURE_FORMAT, CAPTURE_VERSION_EXCHANGES, Capture, determinism_envelope
from .trace import BackendTrace, canonical_json, clear_trace, use_trace

# Test-trigger identity prefix inside the existing `operation` field.
TEST_TRIGGER_PREFIX = "test:"
# The registry oracle a failed test capsule carries: an authored invariant
# (the test's own assertion) was violated. Existing id, not a new one.
TEST_FAILURE_ORACLE = "backend-authored-invariant"
# Structured stderr markers `reproit check` parses, like REPROIT:DIVERGENCE.
RESULT_MARKER = "REPROIT:CI-TEST "
SPOOL_MARKER = "REPROIT:CI-CAPSULE "

# Spool bounds, same numbers as the Node reference. The cap covers the TOTAL
# bytes on disk; spilled capsules beyond it are dropped and counted (in-process
# stats plus the on-disk `dropped.count`), never silently.
DEFAULT_SPOOL_DIR = ".reproit/ci-spool"
DEFAULT_SPOOL_MAX_BYTES = 16 * 1024 * 1024
SPOOL_MAX_FLOOR_BYTES = 4 * 1024
SPOOL_MAX_CEIL_BYTES = 64 * 1024 * 1024
# Suite and test names share the operation field's 256-code-point bound.
MAX_NAME = 120
MAX_ERROR_CHARS = 2048

_STATE = {
    "trace_seq": itertools.count(1),
    "stats": {"spooled_capsules": 0, "dropped_capsules": 0, "failed_captures": 0},
}


def stats():
    return dict(_STATE["stats"])


def _replay_path():
    value = os.environ.get("REPROIT_REPLAY")
    return value if isinstance(value, str) and value else None


def _mode():
    if _replay_path() is not None:
        return "replay"
    if os.environ.get("REPROIT_CI_CAPTURE") == "1":
        return "capture"
    return "off"


def _bounded_name(value):
    return str(value).strip()[:MAX_NAME]


def _operation_for(suite_name, test_name):
    return TEST_TRIGGER_PREFIX + _bounded_name(suite_name) + "#" + _bounded_name(test_name)


def _bounded_error(error):
    return str(error)[:MAX_ERROR_CHARS]


def _is_outcome_skip(error):
    """A pytest skip/xfail outcome is not a test failure and must never spool
    a capsule or report a replayed failure. Detected structurally so the SDK
    keeps its zero-dependency runtime."""
    return type(error).__module__.startswith("_pytest") and type(error).__name__ in (
        "Skipped",
        "XFailed",
    )


def _ci_context():
    """Synthesized trace context: the CI job stands where production stood.
    Code identity resolves like production capture (REPROIT_COMMIT, then
    GITHUB_SHA, both validated)."""
    return {
        "trace_id": "ci-%d-%d" % (int(time.time() * 1000), next(_STATE["trace_seq"])),
        "actor": None,
        "action_index": 0,
        "build": Capture.resolve_commit(),
        "config_contract": None,
        "capture_envelope": True,
    }


def _envelope_for(trace):
    """Same envelope shape production capture records; the seed pins the
    REPLAY run's randomness, it does not reproduce the test run's."""
    events = trace.events()
    first = events[0] if events else {}
    at = first.get("at")
    observed = at if isinstance(at, int) and not isinstance(at, bool) else None
    return determinism_envelope(observed)


def _spool_dir():
    directory = os.environ.get("REPROIT_CI_SPOOL")
    return directory if isinstance(directory, str) and directory else DEFAULT_SPOOL_DIR


def _spool_max_bytes():
    try:
        parsed = int(os.environ.get("REPROIT_CI_SPOOL_MAX", ""))
    except ValueError:
        return DEFAULT_SPOOL_MAX_BYTES
    return min(SPOOL_MAX_CEIL_BYTES, max(SPOOL_MAX_FLOOR_BYTES, parsed))


def _record_drop(directory):
    counter = os.path.join(directory, "dropped.count")
    dropped = 0
    try:
        with open(counter, "r", encoding="utf-8") as handle:
            dropped = int(handle.read()) or 0
    except (OSError, ValueError):
        # First drop: the counter does not exist yet.
        pass
    with open(counter, "w", encoding="utf-8") as handle:
        handle.write(str(dropped + 1) + "\n")


def _spool(payload):
    """Write one capsule inside the byte cap; over-cap capsules are dropped
    and counted. Returns the file path or None."""
    raw = canonical_json(payload).encode("utf-8")
    directory = _spool_dir()
    os.makedirs(directory, exist_ok=True)
    used = 0
    for entry in os.listdir(directory):
        if not entry.endswith(".json"):
            continue
        try:
            used += os.path.getsize(os.path.join(directory, entry))
        except OSError:
            # A concurrently removed entry counts as zero.
            pass
    if used + len(raw) > _spool_max_bytes():
        _STATE["stats"]["dropped_capsules"] += 1
        _record_drop(directory)
        return None
    digest = hashlib.sha256(raw).hexdigest()[:12]
    file = os.path.join(directory, "capsule-" + digest + ".json")
    with open(file, "wb") as handle:
        handle.write(raw)
    _STATE["stats"]["spooled_capsules"] += 1
    detail = {"file": file, "operation": payload["operation"]}
    sys.stderr.write(SPOOL_MARKER + json.dumps(detail, separators=(",", ":")) + "\n")
    sys.stderr.flush()
    return file


def _finish_and_spool(trace, operation, error):
    try:
        trace.finish({"error": _bounded_error(error)}, None, False, False)
        _spool(
            {
                "format": CAPTURE_FORMAT,
                "version": CAPTURE_VERSION_EXCHANGES,
                "operation": operation,
                "oracle": TEST_FAILURE_ORACLE,
                "envelope": _envelope_for(trace),
                "events": trace.events(),
            }
        )
    except Exception:
        # Capture must never mask the test's own failure.
        _STATE["stats"]["failed_captures"] += 1


def _run(fn, args, kwargs):
    """Run a test function; a coroutine-returning test runs to completion on
    its own loop, so async tests work under plain pytest, with the ambient
    trace propagating through contextvars."""
    result = fn(*args, **kwargs)
    if inspect.iscoroutine(result):
        asyncio.run(result)


def _capture_test(suite_name):
    instrument.install()

    def ci_test(test_name):
        operation = _operation_for(suite_name, test_name)

        def decorate(fn):
            @functools.wraps(fn)
            def wrapper(*args, **kwargs):
                trace = BackendTrace.begin(
                    _ci_context(),
                    operation,
                    input={
                        "suite": _bounded_name(suite_name),
                        "test": _bounded_name(test_name),
                    },
                )
                token = use_trace(trace)
                try:
                    _run(fn, args, kwargs)
                except (KeyboardInterrupt, SystemExit, GeneratorExit):
                    raise
                except BaseException as error:
                    if not _is_outcome_skip(error):
                        _finish_and_spool(trace, operation, error)
                    raise
                finally:
                    clear_trace(token)
                try:
                    trace.finish(None, None, True, False)
                except Exception:
                    # An over-long passing trace has nothing to spool anyway.
                    pass

            return wrapper

        return decorate

    return ci_test


def _replay_target():
    """The capsule names exactly one test; everything else is skipped so the
    runner's exit code speaks for the named test alone."""
    with open(_replay_path(), "r", encoding="utf-8") as handle:
        payload = json.load(handle)
    operation = payload.get("operation")
    if not isinstance(operation, str) or not operation.startswith(TEST_TRIGGER_PREFIX):
        raise TypeError("REPROIT_REPLAY capsule does not carry a test trigger identity")
    return operation


def _report_result(operation, status, error):
    detail = {"operation": operation, "status": status}
    if error is not None:
        detail["failure"] = _bounded_error(error)
    sys.stderr.write(RESULT_MARKER + json.dumps(detail, separators=(",", ":")) + "\n")
    sys.stderr.flush()


def _mark_skip(stub, reason):
    """Skip under pytest when pytest exists; under direct invocation the stub
    is already a no-op, mirroring the Node wrapper's skip option."""
    try:
        import pytest
    except ImportError:
        return stub
    return pytest.mark.skip(reason=reason)(stub)


def _replay_test(suite_name):
    instrument.install()
    target = _replay_target()

    def ci_test(test_name):
        operation = _operation_for(suite_name, test_name)

        def decorate(fn):
            if operation != target:

                @functools.wraps(fn)
                def stub(*args, **kwargs):
                    return None

                return _mark_skip(stub, "reproit replay targets " + target)

            @functools.wraps(fn)
            def wrapper(*args, **kwargs):
                try:
                    _run(fn, args, kwargs)
                except (KeyboardInterrupt, SystemExit, GeneratorExit):
                    raise
                except BaseException as error:
                    if not _is_outcome_skip(error):
                        _report_result(operation, "failed", error)
                    raise
                _report_result(operation, "passed", None)

            return wrapper

        return decorate

    return ci_test


def suite(suite_name, **options):
    """The CI test decorator factory. Keyword options are reserved; there are
    none yet and unknown keys are rejected so a typo cannot silently change
    capture behavior."""
    if options:
        raise TypeError("reproit ci.suite: unknown option " + sorted(options)[0])
    active = _mode()
    if active == "capture":
        factory = _capture_test(suite_name)
    elif active == "replay":
        factory = _replay_test(suite_name)
    else:

        def factory(test_name):
            def decorate(fn):
                return fn

            return decorate

    # The factory is usually bound to a module-level name like `test`, which
    # pytest would otherwise collect as a test item itself.
    factory.__test__ = False
    return factory
