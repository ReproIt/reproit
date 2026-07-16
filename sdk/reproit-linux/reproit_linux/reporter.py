"""Cloud reporter + crash handler for the Linux SDK.

Emits the SAME state-graph + error events the reproit test runners and the other
production SDKs emit, so the production graph aligns 1:1 with test-time graphs.
Mirrors the React Native / web / Flutter SDKs' event contract exactly: a batch

    { appId, sentAt, ctx?, events }

is POSTed to `<endpoint>/v1/events` (JSON, Bearer apiKey). Each event is either
an `edge` (a state transition: from -> to with an action label) or an `error`
(an uncaught crash carrying the graph path that produced it). The transport has
no third-party dependency (urllib only) so the SDK adds nothing to a GTK/Qt
app's wheel.

This module has no GTK/AT-SPI import; the capture layer feeds it Node trees.
"""

import json
import sys
import signal
import threading
import time
import urllib.request

from .signature import signature
from .causal import install_causal_urllib

# Tier-1 auto context dimensions (zero-PII), mirroring the other SDKs' context.
try:
    import locale as _locale
    import platform as _platform
except Exception:  # pragma: no cover
    _locale = None
    _platform = None


def _now_ms():
    return int(time.time() * 1000)


def auto_context():
    """Tier-1 auto dimensions (docs: the "which users" answer), best-effort and
    zero-PII: platform, OS version, locale, release flag. Each is omitted (never
    raised) when its source is unavailable. Mirrors the RN/Flutter `ctx` shape."""
    ctx = {}
    try:
        if _platform is not None:
            ctx["platform"] = "linux"
            ver = _platform.release()
            if ver:
                ctx["osVersion"] = str(ver)
    except Exception:
        pass
    try:
        if _locale is not None:
            loc = _locale.getlocale()[0] or _locale.getdefaultlocale()[0]
            if loc:
                ctx["locale"] = str(loc)
    except Exception:
        pass
    # A release build is the default assumption (no __debug__-style toggle on a
    # shipped desktop app); callers can override via set_context.
    ctx["release"] = not sys.flags.dev_mode
    return ctx


class Reporter:
    """Buffers events and flushes batches to the cloud. Thread-safe: the flush
    timer and a crashing thread can both touch the buffer.

    The event shapes match the other SDKs byte-for-byte:
      edge:  {kind:"edge", from?, action, to, labels?, t}
      error: {kind:"error", sig, path:[{sig,action}...], message, stack?, t}
    """

    BATCH_FLUSH_AT = 50

    def __init__(self, app_id, endpoint=None, api_key=None, on_event=None,
                 flush_ms=5000, path_cap=60, redact_labels=False,
                 build_version=None, build_commit=None):
        if not app_id:
            raise ValueError("Reporter: app_id is required")
        self.app_id = app_id
        self.endpoint = endpoint
        self.api_key = api_key
        self.on_event = on_event
        self.flush_ms = flush_ms
        self.path_cap = path_cap
        self.redact_labels = redact_labels

        self._lock = threading.RLock()
        self._buf = []
        self._path = []          # the graph trail: list of {sig, action}
        self._cur = None         # current state signature
        self._ctx = auto_context()
        build = {}
        if build_version:
            build["version"] = str(build_version)
        if build_commit:
            build["commit"] = str(build_commit)
        if build:
            self._ctx["build"] = build
        self._flush_timer = None
        self._on = True
        self._causal_restore = install_causal_urllib(endpoint)

    # ---- context -----------------------------------------------------------

    def set_context(self, key, value):
        with self._lock:
            self._ctx[key] = value
        return self

    def identify(self, uid_hash, context=None):
        """Attach a pre-hashed user id (and optional dims). The SDK does NOT hash
        here: callers pass an already-hashed id so raw identity never enters the
        SDK, matching the cloud's PII-safe `uid` contract."""
        with self._lock:
            self._ctx["uid"] = uid_hash
            if context:
                self._ctx.update(context)
        return self

    def context(self):
        with self._lock:
            return dict(self._ctx)

    # ---- timer -------------------------------------------------------------

    def start_timer(self):
        """Start the periodic flush. Uses a daemon Timer so it never keeps a
        process (or a test run) alive."""
        if self._flush_timer is not None:
            return

        def tick():
            self.flush()
            with self._lock:
                if self._on:
                    self._flush_timer = threading.Timer(self.flush_ms / 1000.0, tick)
                    self._flush_timer.daemon = True
                    self._flush_timer.start()

        self._flush_timer = threading.Timer(self.flush_ms / 1000.0, tick)
        self._flush_timer.daemon = True
        self._flush_timer.start()

    # ---- state graph -------------------------------------------------------

    def record_edge(self, anchor, root, action="auto", labels=None):
        """Sign the current Node tree and record an edge if the signature
        changed. The action defaults to 'auto'; the initial edge is labeled
        'load'. Returns the (possibly unchanged) current signature."""
        sig = signature(anchor, root)
        with self._lock:
            if sig == self._cur:
                return sig
            prev = self._cur
            self._cur = sig
            self._path.append({"sig": sig, "action": action})
            if len(self._path) > self.path_cap:
                self._path.pop(0)
            ev = {
                "kind": "edge",
                "action": "load" if prev is None else (action or "auto"),
                "to": sig,
                "t": _now_ms(),
            }
            if prev is not None:
                ev["from"] = prev
            if labels and not self.redact_labels:
                ev["labels"] = list(labels)
        self._emit(ev)
        return sig

    def record_error(self, exc, message=None, action=None):
        """Record an uncaught-error event carrying the graph path that produced
        it, then flush promptly (errors are worth shipping immediately). Matches
        the other SDKs' error shape (sig + path + message + stack). `action` is
        the in-flight action (the one that threw); it is appended to the path so a
        path-based replay fires the bug, not stop one step short of it."""
        import traceback
        if message is None:
            message = "%s: %s" % (type(exc).__name__, exc) if exc else "unknown error"
        stack = []
        try:
            tb = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__)) if exc else ""
            stack = [ln.strip() for ln in tb.splitlines() if ln.strip()][-8:]
        except Exception:
            pass
        with self._lock:
            err_path = [dict(p) for p in self._path]
            if action:
                err_path.append({"sig": self._cur or "", "action": action})
            ev = {
                "kind": "error",
                # A genuine uncaught exception / fatal native signal IS the
                # `crash` oracle firing; tag it so the cloud can gate ingest on
                # oracle-grade findings.
                "oracle": "crash",
                "sig": self._cur or "",
                "path": err_path,
                "message": str(message),
                "t": _now_ms(),
            }
            if stack:
                ev["stack"] = stack
        self._emit(ev)
        self.flush()

    # ---- transport ---------------------------------------------------------

    def _emit(self, ev):
        if not self._on:
            return
        if self.on_event:
            try:
                self.on_event(ev)
            except Exception:
                pass  # a host callback must never break telemetry
        with self._lock:
            self._buf.append(ev)
            full = len(self._buf) >= self.BATCH_FLUSH_AT
        if full:
            self.flush()

    def flush(self):
        """Flush queued events as one `{appId, sentAt, ctx?, events}` batch to
        `<endpoint>/v1/events`. Best-effort: drops on failure (matches the other
        SDKs). With no endpoint set, the batch goes to the debug stream / the
        on_event hook only."""
        with self._lock:
            if not self._buf:
                return
            events = self._buf
            self._buf = []
            batch = {"appId": self.app_id, "sentAt": _now_ms(), "events": events}
            if self._ctx:
                batch["ctx"] = dict(self._ctx)
            endpoint = self.endpoint
            api_key = self.api_key
        if not endpoint:
            if not self.on_event:
                sys.stderr.write("[reproit] %s\n" % json.dumps(batch))
            return
        body = json.dumps(batch).encode("utf-8")
        headers = {"Content-Type": "application/json"}
        if api_key:
            headers["Authorization"] = "Bearer %s" % api_key
        try:
            req = urllib.request.Request(
                endpoint.rstrip("/") + "/v1/events",
                data=body, headers=headers, method="POST",
            )
            urllib.request.urlopen(req, timeout=5)
        except Exception:
            pass  # best-effort: drop on failure

    # ---- crash / signal handling -------------------------------------------

    def install_crash_handler(self):
        """Install handlers so a fatal crash flushes the session:
          - sys.excepthook for uncaught Python exceptions (records an error event
            then chains to the prior hook so the app's own logging still runs);
          - SIGSEGV / SIGABRT / SIGBUS / SIGFPE for native crashes (records a
            signal error then re-raises the default disposition so the crash is
            not swallowed and any core dump still happens)."""
        prior_excepthook = sys.excepthook

        def excepthook(exc_type, exc, tb):
            try:
                if exc is not None and exc.__traceback__ is None:
                    exc.__traceback__ = tb
                self.record_error(exc)
            except Exception:
                pass
            try:
                prior_excepthook(exc_type, exc, tb)
            except Exception:
                pass

        sys.excepthook = excepthook

        for signum in (
            getattr(signal, "SIGSEGV", None),
            getattr(signal, "SIGABRT", None),
            getattr(signal, "SIGBUS", None),
            getattr(signal, "SIGFPE", None),
        ):
            if signum is None:
                continue
            try:
                signal.signal(signum, self._on_fatal_signal)
            except (ValueError, OSError):
                # signal() only works on the main thread; skip silently otherwise.
                pass

    def _on_fatal_signal(self, signum, frame):
        try:
            name = signal.Signals(signum).name
        except Exception:
            name = "signal %d" % signum
        self.record_error(None, message="fatal native signal: %s" % name)
        # Restore the default disposition and re-raise so the crash is not
        # swallowed (preserves core dumps and the real exit code).
        try:
            signal.signal(signum, signal.SIG_DFL)
        except Exception:
            pass
        try:
            signal.raise_signal(signum)
        except Exception:
            pass

    def dispose(self):
        with self._lock:
            self._on = False
            if self._flush_timer is not None:
                self._flush_timer.cancel()
                self._flush_timer = None
        self.flush()
        self._causal_restore()
