"""reproit-linux: production telemetry SDK for native Linux GUI apps (GTK / Qt).

Embeds in a real GTK or Qt desktop app, captures the in-app widget tree through
the accessibility interface (ATK for GTK in-process, AT-SPI for Qt and any
toolkit on the bus), computes the CANONICAL structural signature byte-for-byte
identical to the Rust oracle and every other SDK, and reports the state graph +
crash signatures to the reproit cloud so a production crash replays as a test.

It is the in-app twin of runners/linux-atspi.py (which drives an app from
OUTSIDE through the AT-SPI bus); this SDK runs INSIDE the app and shares the same
signature core and the same role map, so the in-app graph and the fuzz graph
align 1:1.

Usage (one call in your app entry, after the main window is built):

    from reproit_linux import ReproIt
    win = builder.get_object("main_window")          # your top-level GtkWindow
    ReproIt.init(
        app_id="example",
        endpoint="https://ingest.reproit.example",
        api_key="sk_...",
        root_widget=win,            # a GTK widget; the SDK walks its accessible
    )

For a Qt app (or any AT-SPI toolkit), pass the AT-SPI root accessible instead:

    ReproIt.init(app_id="example", atspi_root=top_accessible, ...)

Then call ReproIt.observe(action) after each user action (or wire it to your
signal handlers) to record a state edge. A fatal crash flushes the session
automatically via the installed crash/signal handler.
"""

from .signature import (
    Node, descriptor, signature, value_class, fnv1a32_hex, selector_for,
)
from .capture import (
    build_node_atk, build_node_atspi, node_from_attrs, atk_role, anchor_of,
    label_of_atk,
)
from .reporter import Reporter, auto_context

__all__ = [
    "ReproIt", "Node", "descriptor", "signature", "value_class", "fnv1a32_hex",
    "selector_for", "build_node_atk", "build_node_atspi", "node_from_attrs",
    "atk_role", "anchor_of", "Reporter", "auto_context",
]


class _ReproIt:
    """The telemetry singleton, mirroring the other SDKs' `ReproIt.init(...)`
    entry. Safe to call init once; later calls are ignored."""

    def __init__(self):
        self._on = False
        self._reporter = None
        self._root_acc = None       # ATK accessible (GTK in-process)
        self._atspi_root = None     # AT-SPI accessible (Qt / bus)
        self._anchor = None
        self._invariants = {}       # id -> predicate (idempotent by id)

    def init(self, app_id, endpoint=None, api_key=None, on_event=None,
             root_widget=None, root_accessible=None, atspi_root=None,
             value_nodes=None, flush_ms=5000, path_cap=60, redact_labels=False,
             install_crash_handler=True):
        """Initialize telemetry.

        Provide exactly one capture root:
          - root_widget:      a GTK widget; its `.get_accessible()` is walked
                              (GTK in-process via ATK).
          - root_accessible:  an ATK accessible directly (already resolved).
          - atspi_root:       an AT-SPI accessible (Qt / any bus toolkit).

        value_nodes: Layer-3 opt-in selectors (docs/signature.md "Value-state")
        that mark EXTRA nodes value-bearing; same grammar as reproit.yaml.
        """
        if self._on:
            return self
        if not app_id:
            raise ValueError("ReproIt.init: app_id is required")

        self._reporter = Reporter(
            app_id=app_id, endpoint=endpoint, api_key=api_key, on_event=on_event,
            flush_ms=flush_ms, path_cap=path_cap, redact_labels=redact_labels,
        )
        self._value_nodes = list(value_nodes or [])

        if root_widget is not None:
            try:
                self._root_acc = root_widget.get_accessible()
            except Exception:
                self._root_acc = None
        if root_accessible is not None:
            self._root_acc = root_accessible
        if atspi_root is not None:
            self._atspi_root = atspi_root

        self._on = True
        if install_crash_handler:
            self._reporter.install_crash_handler()
        self._reporter.start_timer()

        # First snapshot (the 'load' edge) once a root is available.
        if self._root_acc is not None or self._atspi_root is not None:
            self.observe("load")
        return self

    # ---- capture -----------------------------------------------------------

    def _capture(self):
        """Walk the configured root into a canonical Node tree + anchor + labels.
        Returns (anchor, root_node, labels) or (None, None, []) if no root."""
        if self._root_acc is not None:
            root = build_node_atk(self._root_acc)
            anchor = anchor_of(self._root_acc)
            labels = _collect_labels_atk(self._root_acc)
        elif self._atspi_root is not None:
            root = build_node_atspi(self._atspi_root)
            anchor = None  # AT-SPI anchor resolution handled by the runner side
            labels = []
        else:
            return (None, None, [])
        self._apply_value_nodes(root)
        return (anchor, root, labels)

    def _apply_value_nodes(self, root):
        """Set the value_node flag (Layer 3 opt-in) on nodes matching a configured
        selector, so the oracle treats them as value-bearing. No-op when empty."""
        if not self._value_nodes:
            return
        sel = set(self._value_nodes)
        from .signature import normalize_role, _is_transient

        def node_selector(node, idx):
            if node.id is not None:
                return "key:%s" % node.id
            return "role:%s#%d" % (normalize_role(node.role), idx)

        def visit_children(node):
            counts = {}
            for child in node.children:
                if _is_transient(child):
                    continue
                role = normalize_role(child.role)
                idx = counts.get(role, 0)
                counts[role] = idx + 1
                if node_selector(child, idx) in sel:
                    child.value_node = True
                visit_children(child)

        if not _is_transient(root):
            if node_selector(root, 0) in sel:
                root.value_node = True
            visit_children(root)

    # ---- public API --------------------------------------------------------

    def invariant(self, inv_id, predicate):
        """Register an app invariant: a predicate the app declares that must hold
        in EVERY visited state. `predicate()` returns truthy when it HOLDS, or
        falsy / raises / an object ``{"ok": False, "message": ...}`` when it is
        VIOLATED. reproit's fuzzer evaluates every registered invariant on each
        state-observe and reports the failures as `invariant` findings; in
        production the registry is INERT (a plain store, evaluated only under the
        fuzzer), so this is zero-overhead until a run reproduces it. Registration
        is idempotent by id, so re-registering an id replaces it."""
        if isinstance(inv_id, str) and callable(predicate):
            self._invariants[inv_id] = predicate
        return self

    def observe(self, action="auto"):
        """Capture the current screen and record a state edge if the signature
        changed. Call after each user action (or wire to signal handlers)."""
        if not self._on or self._reporter is None:
            return None
        anchor, root, labels = self._capture()
        if root is None:
            return None
        sig = self._reporter.record_edge(anchor, root, action=action, labels=labels)
        _report_invariants(self._invariants, sig)
        return sig

    def record_snapshot(self, tree, action="auto", anchor=None):
        """Escape hatch: record a state edge from a Node tree you build yourself
        (e.g. a custom-drawn surface with no accessible). Hashed identically."""
        if not self._on or self._reporter is None:
            return None
        sig = self._reporter.record_edge(anchor, tree, action=action)
        _report_invariants(self._invariants, sig)
        return sig

    def record_error(self, exc, message=None):
        """Record an uncaught-error event (carrying the graph path) and flush."""
        if self._on and self._reporter is not None:
            self._reporter.record_error(exc, message=message)

    def set_context(self, key, value):
        if self._reporter is not None:
            self._reporter.set_context(key, value)
        return self

    def identify(self, uid_hash, context=None):
        if self._reporter is not None:
            self._reporter.identify(uid_hash, context=context)
        return self

    def context(self):
        return self._reporter.context() if self._reporter is not None else {}

    def flush(self):
        if self._reporter is not None:
            self._reporter.flush()

    def dispose(self):
        if self._reporter is not None:
            self._reporter.dispose()
        self._on = False
        self._reporter = None
        self._root_acc = None
        self._atspi_root = None


def _eval_invariant(predicate):
    """Evaluate one predicate. Returns None when it HOLDS, or a failure message
    string when it is VIOLATED. Mirrors the web SDK contract: truthy holds; falsy
    / raises / ``{"ok": False, "message": ...}`` is a violation (the raised text,
    the object's message, or "" for a bare falsy)."""
    try:
        result = predicate()
    except Exception as exc:  # a raising predicate is a violation
        return str(exc) or exc.__class__.__name__
    if isinstance(result, dict):
        if result.get("ok") is False:
            return str(result.get("message", ""))
        return None
    return None if result else ""


def _report_invariants(invariants, sig):
    """Evaluate every registered invariant and, ONLY under the fuzzer (the
    ``REPROIT_UNDER_FUZZER`` env var the AT-SPI runner sets on the launched
    child, which is the fuzzer-detection gate), write one marker line
        REPROIT_INVARIANT {"sig": "<sig>", "items": [{"id", "message"}, ...]}
    listing the VIOLATED invariants to stderr, which the runner scrapes and
    re-emits as EXPLORE:INVARIANT. Silent when the registry is empty or every
    invariant held (no empty-items line). Inert in production (env var unset)."""
    import os
    import sys
    import json
    if not invariants or not os.environ.get("REPROIT_UNDER_FUZZER"):
        return
    items = []
    for inv_id, predicate in invariants.items():
        message = _eval_invariant(predicate)
        if message is not None:
            items.append({"id": inv_id, "message": message})
    if not items:
        return
    sys.stderr.write(
        "REPROIT_INVARIANT %s\n" % json.dumps({"sig": sig or "", "items": items})
    )
    sys.stderr.flush()


def _collect_labels_atk(acc, out=None, depth=0):
    """Display-only localized labels for `map show` (NEVER hashed)."""
    if out is None:
        out = []
    if depth > 60 or acc is None:
        return out
    lbl = label_of_atk(acc)
    if lbl and len(lbl) <= 40 and lbl not in out:
        out.append(lbl)
    try:
        for i in range(acc.get_n_accessible_children()):
            _collect_labels_atk(acc.ref_accessible_child(i), out, depth + 1)
    except Exception:
        pass
    return out[:24]


ReproIt = _ReproIt()
