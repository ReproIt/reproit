"""Cloud reporter + crash handler for the Python TUI SDK.

A Python terminal-UI app (Textual, Rich, urwid, prompt_toolkit, or a hand-rolled
raw-mode dashboard) creates one Reporter, calls observe(screen, action) with each
rendered frame, and the SDK:

  1. computes the SAME canonical TUI screen signature the fuzz runner computes
     (signature.py, a port of crates/tui-sig), in the TUI namespace;
  2. records a coverage EDGE only when the STRUCTURAL signature changes (and tracks
     the content fingerprint as the Layer-1 effect token, exactly like the Go/Rust
     SDKs and the runner);
  3. batches events and POSTs them to the cloud as the SAME contract every other
     reproit SDK uses, via stdlib urllib (best-effort, drop on failure):
         { appId, sentAt, ctx?, events }
  4. installs a crash handler (sys.excepthook + SIGSEGV/SIGABRT/SIGBUS/SIGFPE)
     that records an error event carrying the crashing screen's signature and the
     graph path, flushes, then chains to the prior disposition so the crash is not
     swallowed.

This mirrors the Linux SDK reporter (sdk/reproit-linux/reproit_linux/reporter.py)
event contract byte-for-byte (edge + error events, the {appId, sentAt, ctx?,
events} batch, the urllib transport, the crash handler), and the Go TUI reporter's
observe-on-signature-change behavior. It has NO Textual/Rich import: the capture
layer (capture.py) feeds it a ScreenContents.

No em dashes anywhere, per project rules.
"""

import json
import sys
import signal
import threading
import time
import urllib.request

from .signature import structural_sig, content_fingerprint, labels_of

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
    """Tier-1 auto dimensions, best-effort and zero-PII: platform, OS version,
    locale, release flag. Each is omitted (never raised) when its source is
    unavailable. Mirrors the Linux/RN/Flutter `ctx` shape."""
    ctx = {}
    try:
        if _platform is not None:
            ctx["platform"] = _platform.system().lower() or "unknown"
            ver = _platform.release()
            if ver:
                ctx["osVersion"] = str(ver)
    except Exception:
        pass
    try:
        if _locale is not None:
            loc = None
            try:
                loc = _locale.getlocale()[0]
            except Exception:
                loc = None
            if loc:
                ctx["locale"] = str(loc)
    except Exception:
        pass
    # A release build is the default assumption (no shipped TUI debug toggle);
    # callers can override via set_context.
    ctx["release"] = not sys.flags.dev_mode
    return ctx


class Reporter:
    """Buffers events and flushes batches to the cloud. Thread-safe: the flush
    timer and a crashing thread can both touch the buffer.

    The event shapes match the other reproit SDKs byte-for-byte:
      edge:  {kind:"edge", from?, action, to, labels?, t}
      error: {kind:"error", sig, path:[{sig,action}...], message, stack?, t}
    """

    BATCH_FLUSH_AT = 50

    def __init__(self, app_id, endpoint=None, api_key=None, on_event=None,
                 flush_ms=5000, path_cap=60, redact_labels=False):
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
        self._cur = None         # current structural signature
        self._cur_fp = None      # current content fingerprint (Layer-1, ephemeral)
        self._ctx = auto_context()
        self._flush_timer = None
        self._on = True

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

    def observe(self, screen, action="auto"):
        """Sign the current rendered screen and record a coverage edge if the
        STRUCTURAL signature changed. `screen` is a ScreenContents (capture.py);
        the SDK reads its contents text and cursor cell. The CONTENT fingerprint is
        tracked too, so a value-only change (same skeleton, different on-screen
        number) is detected as an effect, exactly as the runner does, but it is
        ephemeral and never becomes the canonical state identity (the runner
        records edges ONLY on signature change, so we match it and keep the cloud
        graph identical). Returns the current structural signature."""
        contents = screen.text()
        cursor = screen.cursor
        return self.observe_contents(contents, cursor, action)

    def observe_contents(self, contents, cursor, action="auto"):
        """Lowest-level path: the app hands the exact contents string plus the
        0-based (row, col) cursor cell. Used by observe() and available directly
        for apps that already hold a contents string (e.g. a Rich export)."""
        sig = structural_sig(contents, cursor)
        fp = content_fingerprint(contents, cursor)
        with self._lock:
            self._cur_fp = fp
            if sig == self._cur:
                # No structural change: a value-only effect is real but does not
                # open a new coverage edge.
                return sig
            prev = self._cur
            self._cur = sig
            self._path.append({"sig": sig, "action": action or "auto"})
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
            if not self.redact_labels:
                labels = labels_of(contents)
                if labels:
                    ev["labels"] = labels
        self._emit(ev)
        return sig

    def current_sig(self):
        """The last observed structural signature (the state to replay)."""
        with self._lock:
            return self._cur

    def record_error(self, exc, message=None, action=None):
        """Record an uncaught-error event carrying the current signature and the
        graph path that produced it, then flush promptly (errors ship
        immediately). Matches the other SDKs' error shape (sig + path + message +
        stack)."""
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
            # Include the in-flight action in the path: an action whose handler
            # throws stops the path one step short of the bug, so a path-based
            # replay would never fire it. Mirrors the GUI SDKs.
            err_path = [dict(p) for p in self._path]
            if action:
                err_path.append({"sig": self._cur or "", "action": action})
            ev = {
                "kind": "error",
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
        SDKs). With no endpoint set, the batch goes to the on_event hook, or the
        debug stream if there is none."""
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
            not swallowed and any core dump still happens).
        Restore the terminal yourself in your own handler if needed; this SDK does
        not touch the TTY."""
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
