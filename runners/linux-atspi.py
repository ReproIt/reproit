# /// script
# requires-python = ">=3.9"
# dependencies = ["PyGObject"]
# ///
"""ReproIt Linux desktop runner (AT-SPI2 backend).

Drives ANY native Linux app (GTK, Qt, and Avalonia / wxWidgets builds, which
all publish to AT-SPI) through the accessibility bus and prints the
framework-agnostic marker protocol that `reproit` parses. The Linux twin of
runners/macos-ax.swift and runners/windows-uia.py.

The screen signature is the CANONICAL structural signature defined in
docs/signature.md and proven by signature_vectors.json. This file is a Python
port of the Rust oracle (crates/reproit/src/model/signature.rs): it walks the
AT-SPI tree into a normalized Node tree (role from the AT-SPI Role -> the fixed
vocabulary, id from the accessible-id, type for inputs, icon if available),
then serializes the descriptor and hashes it FNV-1a 32-bit. Localized
names/text NEVER enter the hash; they are kept only as a display-only label list.

The signature core (Node, descriptor, signature, plus atspi_role /
atspi_to_node) is importable WITHOUT a Linux/AT-SPI host: the `gi`/`Atspi`
import is deferred to main(), so runners/test_signature.py can prove parity on
any platform.

Run with uv:
    uv run runners/linux-atspi.py

Note: AT-SPI also needs the system GObject-Introspection typelib for Atspi
(e.g. `gir1.2-atspi-2.0`) and accessibility enabled on the session; PyGObject
alone (the pip dep) is not sufficient. Env: REPROIT_TARGET (app name / launch
path), REPROIT_FUZZ_CONFIG.

Linux-only: drives the live AT-SPI accessibility bus. The signature function +
parity test run anywhere.
"""

import json
import os
import re
import subprocess
import sys
import time

ACTION_BUDGET = 36
MAX_LABEL_LEN = 40
MAX_LABELS_PER_STATE = 24
# Overflow oracle tolerance (px): a child must escape its parent's content box by
# more than this to be flagged, so sub-pixel rounding in get_extents never
# produces a false positive. Mirrors the web runner's OVERFLOW_TOL intent.
OVERFLOW_TOL_PX = 4
# HANG watchdog floor (ms): a coarse, well-separated floor so host scheduling
# jitter can never flip the verdict. Matches the web runner's HANG_FLOOR_MS.
HANG_FLOOR_MS = 2000


# ---- CONTENT-BUG oracle (deterministic, label-based) ------------------------
# Mirrors runners/web/runner.mjs detectContentBugs: a rendered label carrying a
# stringify/template artifact leaked to the screen. Each classifier is a pure
# substring/structure test over the trimmed label, so the same a11y tree yields
# the same finding every run and on replay. The match is on STRUCTURE (a literal
# artifact token), never natural language, so a real label that merely mentions
# "null" in prose is not flagged: the token must BE the artifact (whole-word
# undefined/null/NaN, the bracketed literal). Order is fixed and first match wins.
_CB_TEMPLATE_CURLY = re.compile(r"\{\{[^}]*\}\}")
_CB_TEMPLATE_DOLLAR = re.compile(r"\$\{[^}]*\}")
_CB_UNDEFINED = re.compile(r"(^|[\s:>(\[,])undefined($|[\s.,!?)\]<])")
_CB_NULL = re.compile(r"(^|[\s:>(\[,])null($|[\s.,!?)\]<])")
_CB_NAN = re.compile(r"(^|[\s:>(\[,])NaN($|[\s.,!?)\]<])")


def content_bug_reason(text):
    """The stable reason tag for a broken-content label, or None. First match
    wins, so a label carries at most one reason (byte-identical run to run)."""
    if not text:
        return None
    if "[object Object]" in text:
        return "object-object"
    if _CB_TEMPLATE_CURLY.search(text) or _CB_TEMPLATE_DOLLAR.search(text):
        return "unrendered-template"
    if _CB_UNDEFINED.search(text):
        return "undefined"
    if _CB_NULL.search(text):
        return "null"
    if _CB_NAN.search(text):
        return "nan"
    return None


# =============================================================================
# Canonical structural signature (port of crates/reproit/src/model/signature.rs)
# This block has NO Linux imports and is what runners/test_signature.py loads.
# =============================================================================

# The fixed, language-independent role vocabulary (docs/signature.md "Roles").
# Anything outside this set normalizes to `node`.
ROLES = frozenset([
    "screen", "header", "text", "button", "link", "textfield", "image", "icon",
    "list", "listitem", "tab", "switch", "checkbox", "radio", "slider", "menu",
    "menuitem", "dialog", "group", "node",
])

# Roles that flicker in and out of the tree and must be dropped before hashing
# (docs/signature.md normalization rule 2). "transient error banner" is not a
# distinct role in the vocabulary, so it is expressed via the `transient` flag on
# a node; both paths drop the node and its whole subtree. `progress` is the role
# name for spinner/progress.
TRANSIENT_ROLES = frozenset([
    "toast", "snackbar", "spinner", "progress", "tooltip", "badge",
])

# Value-role set (docs/signature.md "Value-state", Layer 2). A node is
# value-bearing only if it has a `value` AND its RAW role is one of these (or it
# is explicitly flagged via `value_node`, Layer 3). Chrome roles (button / label
# / header / text / link) are NEVER value-bearing, preserving the rule-1 text
# exclusion. Several of these (`status, log, progressbar, meter, timer, output`)
# are NOT in the structural ROLES vocabulary, so they normalize to `node` in the
# token body; the value-role test therefore uses the RAW role, not the normalized
# one.
VALUE_ROLES = frozenset([
    "textfield", "status", "log", "progressbar", "meter", "timer", "output",
])


class Node:
    """A normalized accessibility node: the input to the signature.

    Mirrors the Rust `Node` JSON shape so each golden vector's `tree` parses
    directly via Node.from_json:
        { "role": "button", "id": "submit", "type": "text",
          "icon": "e5cd", "transient": false, "value": "3",
          "value_node": false, "children": [ ... ] }
    All fields except `role`/`children` are optional. Localized chrome text is
    still excluded from the descriptor by construction (rule 1). `value` is the
    displayed data value (Layer 2) and is consulted ONLY when the node is
    value-bearing (a value-role or `value_node`-flagged); chrome text never goes
    here. Defaults keep a value-less tree byte-identical to a pre-value-state one.
    """

    __slots__ = ("role", "id", "type", "icon", "transient", "value", "value_node", "children")

    def __init__(self, role, id=None, type=None, icon=None, transient=False,
                 value=None, value_node=False, children=None):
        self.role = role
        self.id = id
        self.type = type
        self.icon = icon
        self.transient = transient
        self.value = value
        self.value_node = value_node
        self.children = children if children is not None else []

    @staticmethod
    def from_json(j):
        kids = [Node.from_json(c) for c in (j.get("children") or [])]
        return Node(
            role=j["role"],
            id=j.get("id"),
            type=j.get("type"),
            icon=j.get("icon"),
            transient=bool(j.get("transient", False)),
            value=j.get("value"),
            value_node=bool(j.get("value_node", False)),
            children=kids,
        )


def normalize_role(role):
    """Known roles pass through; unknown roles map to `node`."""
    return role if role in ROLES else "node"


def _is_transient(node):
    return node.transient or node.role in TRANSIENT_ROLES


class _NormNode:
    """A node after rules 1, 2, 4 (transients removed, children normalized in
    order). Rule 3 (collapse) is applied at serialization time."""

    __slots__ = ("role", "type", "icon", "id", "children")

    def __init__(self, role, type, icon, id, children):
        self.role = role
        self.type = type
        self.icon = icon
        self.id = id
        self.children = children


def _normalize(node):
    """Apply rules 1, 2, 4: exclude text (no text field exists), drop transient
    subtrees, keep document order. Returns None if this node itself is transient."""
    if _is_transient(node):
        return None
    children = []
    for c in node.children:
        nc = _normalize(c)
        if nc is not None:
            children.append(nc)
    return _NormNode(normalize_role(node.role), node.type, node.icon, node.id, children)


def _token_body(n):
    """One node's token body (everything after `<depth>:`), without the repeat
    marker: `<role>[:<type>][#<icon>][@<id>]`."""
    s = n.role
    if n.type is not None:
        s += ":" + n.type
    if n.icon is not None:
        s += "#" + n.icon
    if n.id is not None:
        s += "@" + n.id
    return s


def _subtree_key(n):
    """The canonical subtree descriptor used for collapse comparison (rule 3):
    the pre-order token list of this subtree, depths re-based to 0, so two sibling
    subtrees at the same level compare equal regardless of absolute depth."""
    tokens = []
    _walk_key(n, 0, tokens)
    return ";".join(tokens)


def _walk_key(n, depth, tokens):
    tokens.append("%d:%s" % (depth, _token_body(n)))
    for c in n.children:
        _walk_key(c, depth + 1, tokens)


def _serialize_node(n, depth, repeated, tokens):
    """Emit one node's token (optionally marked repeated) then recurse,
    collapsing across the children run."""
    tok = "%d:%s" % (depth, _token_body(n))
    if repeated:
        tok += "*"
    tokens.append(tok)
    _serialize_children(n.children, depth + 1, tokens)


def _serialize_children(children, depth, tokens):
    """Walk a run of siblings, collapsing maximal runs of >= 2 consecutive
    children whose subtree_key is identical into one emission with the `*`
    marker (count dropped)."""
    i = 0
    while i < len(children):
        key = _subtree_key(children[i])
        j = i + 1
        while j < len(children) and _subtree_key(children[j]) == key:
            j += 1
        run = j - i
        _serialize_node(children[i], depth, run >= 2, tokens)
        i = j


# --- Layer 2: value-state (docs/signature.md "Value-state") -----------------

def is_value_bearing(node):
    """True iff this node carries a canonical value-class in the V: section: it
    has a `value` AND it is value-bearing, i.e. its RAW role is a value-role OR it
    is `value_node`-flagged (Layer 3 opt-in). The raw role is used deliberately:
    roles like `status`/`meter` normalize to `node` but are still value-roles."""
    return node.value is not None and (node.role in VALUE_ROLES or node.value_node)


def value_class(s):
    """Map a value string to a bounded, deterministic, locale-safe value-class
    token. The numeric branch accepts ONLY the strict period-decimal grammar
    `^[+-]?[0-9]+(\\.[0-9]+)?$` (no grouping, no exponent, no leading/trailing
    dot, ASCII digits only); anything ambiguous falls back to NONEMPTY because we
    do not guess locale formats."""
    t = s.strip()
    if t == "":
        return "EMPTY"
    if _is_strict_decimal(t):
        n = float(t)
        a = abs(n)
        if n == 0.0:
            return "ZERO"
        if n < 0.0:
            return "NEG"
        if a < 10.0:
            return "POS1"
        if a < 100.0:
            return "POS2"
        if a < 1000.0:
            return "POS3"
        return "POSL"
    return "NONEMPTY"


def _is_strict_decimal(s):
    """Strict `^[+-]?[0-9]+(\\.[0-9]+)?$`: optional sign, one or more ASCII
    digits, optionally a period plus one or more ASCII digits. Locale-safe by
    construction (no grouping separators, no exponent, no bare dot)."""
    i = 0
    n = len(s)
    if i < n and (s[i] == "+" or s[i] == "-"):
        i += 1
    int_start = i
    while i < n and "0" <= s[i] <= "9":
        i += 1
    if i == int_start:
        return False  # need at least one integer digit
    if i < n and s[i] == ".":
        i += 1
        frac_start = i
        while i < n and "0" <= s[i] <= "9":
            i += 1
        if i == frac_start:
            return False  # a trailing dot with no fraction digits is not allowed
    return i == n


def _value_key(node, structural_index):
    """The V:-section key for a value-bearing node: its stable `id` as `key:<id>`
    if present, else the structural fallback `role:<role>#<idx>` using the
    NORMALIZED role (so the key namespace matches the selector grammar)."""
    if node.id is not None:
        return "key:%s" % node.id
    return "role:%s#%d" % (normalize_role(node.role), structural_index)


def _collect_values(node, out):
    """Collect (value_key, value_class) for the root then descend. Transient
    subtrees (rule 2) are skipped so the V: section stays consistent with the
    body. The root has no peers, so its keyless structural index is 0."""
    if _is_transient(node):
        return
    if is_value_bearing(node):
        out.append((_value_key(node, 0), value_class(node.value if node.value is not None else "")))
    _collect_values_children(node, out)


def _collect_values_children(node, out):
    """Descend into non-transient children, assigning each keyless child its
    per-parent structural index among same-normalized-role peers (matching the
    selector `#idx`), emitting value-bearing children, then recursing. The node
    itself is NOT re-emitted (the caller already handled it)."""
    role_counts = {}
    for child in node.children:
        if _is_transient(child):
            continue
        role = normalize_role(child.role)
        idx = role_counts.get(role, 0)
        role_counts[role] = idx + 1
        if is_value_bearing(child):
            out.append((_value_key(child, idx), value_class(child.value if child.value is not None else "")))
        _collect_values_children(child, out)


def _value_section(root):
    """Build the V: section suffix. Returns "" when there are NO value-bearing
    nodes, keeping the descriptor (and hash) byte-identical to a pre-value-state
    tree. Otherwise `"\\nV:" + key=class;key=class...` sorted by key."""
    pairs = []
    _collect_values(root, pairs)
    if not pairs:
        return ""
    pairs.sort(key=lambda kc: kc[0])
    body = ";".join("%s=%s" % (k, c) for k, c in pairs)
    return "\nV:%s" % body


def descriptor(anchor, root):
    """Build the exact UTF-8 descriptor string that gets hashed
    (docs/signature.md "Descriptor serialization"):
    `"A:" + anchor + "\\n" + tokens.join(";") + value_section`. The `A:` prefix
    line is always present, even with no anchor. The V: section (Layer 2
    value-classes) is appended ONLY when at least one value-bearing node exists,
    so a value-less tree is byte-identical to the pre-value-state descriptor."""
    tokens = []
    norm = _normalize(root)
    if norm is not None:
        _serialize_node(norm, 0, False, tokens)
    return "A:%s\n%s%s" % (anchor or "", ";".join(tokens), _value_section(root))


def fnv1a32_hex(data):
    """FNV-1a, 32-bit, over `data` bytes; 8-char zero-padded lowercase hex."""
    h = 0x811C9DC5
    for b in data:
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return "%08x" % h


def signature(anchor, root):
    """THE canonical signature: FNV-1a 32-bit over descriptor(), 8 hex chars."""
    return fnv1a32_hex(descriptor(anchor, root).encode("utf-8"))


def selector_for(id, role, structural_index):
    """`key:<id>` when a stable id exists, else `role:<role>#<idx>`. The second
    return value (nokey) is metadata for `map --show`; it does NOT affect the
    hash."""
    if id is not None:
        return ("key:%s" % id, False)
    return ("role:%s#%d" % (normalize_role(role), structural_index), True)


# =============================================================================
# AT-SPI Role -> canonical role vocabulary.
# Keyed by the AT-SPI Role *name* (string, e.g. "PUSH_BUTTON") so this map is
# importable without the `gi`/`Atspi` package. atspi_role() accepts the role
# name string; live capture resolves an Atspi.Role enum to its name first.
# Roles outside the vocabulary fall through to `node` via normalize_role.
# =============================================================================

ATSPI_ROLE_TO_ROLE = {
    "FRAME": "screen",
    "WINDOW": "screen",
    "APPLICATION": "screen",
    "DIALOG": "dialog",
    "ALERT": "dialog",
    "FILE_CHOOSER": "dialog",
    "COLOR_CHOOSER": "dialog",
    "HEADING": "header",
    "PAGE_TAB_LIST": "tab",
    "PAGE_TAB": "tab",
    "LABEL": "text",
    "TEXT": "textfield",
    "ENTRY": "textfield",
    "PASSWORD_TEXT": "textfield",
    "PARAGRAPH": "text",
    "STATIC": "text",
    "CAPTION": "text",
    "PUSH_BUTTON": "button",
    "TOGGLE_BUTTON": "button",
    "SPIN_BUTTON": "textfield",
    "LINK": "link",
    "IMAGE": "image",
    "ICON": "image",
    "LIST": "list",
    "LIST_BOX": "list",
    "TABLE": "list",
    "TREE": "list",
    "TREE_TABLE": "list",
    "LIST_ITEM": "listitem",
    "TABLE_ROW": "listitem",
    "TABLE_CELL": "listitem",
    "TREE_ITEM": "listitem",
    "CHECK_BOX": "checkbox",
    "CHECK_MENU_ITEM": "checkbox",
    "RADIO_BUTTON": "radio",
    "RADIO_MENU_ITEM": "radio",
    "SWITCH": "switch",
    "TOGGLE_SWITCH": "switch",
    "SLIDER": "slider",
    "SCROLL_BAR": "node",
    "PROGRESS_BAR": "progress",   # transient -> dropped
    "SPINNER": "spinner",         # transient -> dropped
    "BUSY_INDICATOR": "spinner",  # transient -> dropped
    "TOOL_TIP": "tooltip",        # transient -> dropped
    "NOTIFICATION": "toast",      # transient -> dropped
    "INFO_BAR": "toast",          # transient -> dropped
    "MENU": "menu",
    "MENU_BAR": "menu",
    "POPUP_MENU": "menu",
    "MENU_ITEM": "menuitem",
    "PANEL": "group",
    "FILLER": "group",
    "GROUPING": "group",
    "TOOL_BAR": "group",
    "VIEWPORT": "group",
    "SECTION": "group",
    "FORM": "group",
    "SCROLL_PANE": "group",
    "SPLIT_PANE": "group",
    "LAYERED_PANE": "group",
    "SEPARATOR": "node",
    "STATUS_BAR": "text",
}

# AT-SPI roles whose name signals a password input -> input `type` refinement.
ATSPI_INPUT_TYPE_BY_ROLE = {
    "PASSWORD_TEXT": "password",
    "SPIN_BUTTON": "number",
}


def atspi_role(role_name):
    """Map an AT-SPI Role *name* string (e.g. "PUSH_BUTTON") to the canonical
    role vocabulary. Unknown roles normalize to `node`."""
    return normalize_role(ATSPI_ROLE_TO_ROLE.get(role_name, "node"))


# =============================================================================
# Live AT-SPI capture (Linux only). Everything below imports `gi`/`Atspi`.
# =============================================================================

def emit(s):
    sys.stdout.write(s + "\n")
    sys.stdout.flush()


def load_fuzz():
    p = os.environ.get("REPROIT_FUZZ_CONFIG")
    if not p:
        return {}
    try:
        with open(p, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return {}


def load_batch():
    """The list of per-seed fuzz configs to run in this session.

    Mirrors the other runners' batch contract (templates/explorer_headless.dart
    FuzzCfg.loadBatch, runners/rn / runners/web): reproit's multi-seed fuzz writes
    {"batch":[ <cfg>, ... ]} where each <cfg> is the single-seed shape
    ({seed, budget, edgeWeights, prefix, replay, ...}). A single-seed (legacy)
    run writes the bare {"seed":..} object with no "batch" key. Returns a list
    of (config, is_batch) where is_batch is True only for the multi-seed shape;
    the caller wraps each seed in SEED:BEGIN/SEED:END only when is_batch."""
    j = load_fuzz()
    if not isinstance(j, dict):
        return ([{}], False)
    batch = j.get("batch")
    if isinstance(batch, list) and batch:
        return ([(b if isinstance(b, dict) else {}) for b in batch], True)
    return ([j], False)


class Rng:
    def __init__(self, seed):
        self.s = (seed & 0xFFFFFFFF) or 1

    def next(self, n):
        s = self.s
        s ^= (s << 13) & 0xFFFFFFFF
        s ^= s >> 17
        s ^= (s << 5) & 0xFFFFFFFF
        self.s = s & 0xFFFFFFFF
        return (self.s & 0x7FFFFFFF) % n

    def unit(self):
        return self.next(1 << 20) / (1 << 20)


def _atspi_role_name(node):
    """Resolve the AT-SPI Role *name* string for a live node, so it keys
    ATSPI_ROLE_TO_ROLE. Prefers the enum's value-nick (e.g. "push button" ->
    "PUSH_BUTTON")."""
    try:
        role = node.get_role()
    except Exception:
        return ""
    # Atspi.Role members repr as e.g. <enum ATSPI_ROLE_PUSH_BUTTON ...>; .value_name
    # is "ATSPI_ROLE_PUSH_BUTTON". Strip the prefix to "PUSH_BUTTON".
    try:
        name = role.value_name  # GObject enum value name
        if name:
            return name.replace("ATSPI_ROLE_", "")
    except Exception:
        pass
    try:
        # Fallback: get_role_name() returns "push button" -> "PUSH_BUTTON".
        rn = node.get_role_name() or ""
        return rn.strip().upper().replace(" ", "_").replace("-", "_")
    except Exception:
        return ""


def _atspi_id(node):
    """Stable developer id from the accessible-id (omitted if empty)."""
    try:
        aid = (node.get_accessible_id() or "").strip()
    except Exception:
        aid = ""
    return aid or None


def _atspi_input_type(role_name, role):
    """Input `type` refinement for textfields, from the role name (e.g.
    PASSWORD_TEXT -> password). None when there is nothing to refine."""
    if role != "textfield":
        return None
    return ATSPI_INPUT_TYPE_BY_ROLE.get(role_name)


def _atspi_icon(node):
    """Language-independent icon identity, if published. AT-SPI has no standard
    icon attribute, so this is None unless an attribute exposes one; left as a
    hook for frameworks that do."""
    return None


def _atspi_live_role(node, role_name, role):
    """Promote a status-bar / notification / live-region accessible to the
    value-role `status` (docs/signature.md "Value-state"), so its changing value
    folds into the canonical signature. STATUS_BAR maps to `text` by default;
    NOTIFICATION/INFO_BAR are transient toasts. We promote to `status` when the
    role is a status bar, OR when the object carries an explicit `live` /
    `container-live` AT-SPI attribute that is not "off" (an active live region),
    OR when the STATE_SET reports an active/live announcing object. `status` is
    not in the structural vocabulary, so it normalizes to `node`, matching the
    oracle's status nodes."""
    if role_name == "STATUS_BAR":
        return "status"
    if _atspi_is_live(node) and role in ("text", "node"):
        return "status"
    return role


def _atspi_is_live(node):
    """True when an accessible declares an active ARIA-style live region via its
    AT-SPI object attributes (`live` / `container-live` != "off"), used to detect
    status announcements that change in place."""
    try:
        attrs = node.get_attributes() or {}
    except Exception:
        return False
    for k in ("live", "container-live", "container_live"):
        v = (attrs.get(k) or "").strip().lower()
        if v and v != "off":
            return True
    return False


def _atspi_value(node, role):
    """The displayed data value for a value-bearing accessible (docs/signature.md
    "Value-state", Layer 2). Read from the Value interface (sliders / spin
    buttons / progress bars) for numeric value-roles, or the Text interface (the
    typed contents of an entry / text). Returns None for chrome roles so the V:
    section is never polluted by chrome text. The raw string is bucketed later by
    `value_class`; the raw text never enters the canonical body."""
    if role not in VALUE_ROLES:
        return None
    # Value interface: sliders, spin buttons, progress bars, meters.
    try:
        vi = node.get_value_iface()
        if vi is not None:
            cv = vi.get_current_value()
            if cv is not None:
                return _fmt_value(cv)
    except Exception:
        pass
    # Text interface: the typed contents of an entry / text field.
    try:
        ti = node.get_text_iface()
        if ti is not None:
            n = ti.get_character_count()
            txt = ti.get_text(0, n if n and n >= 0 else -1)
            if txt is not None:
                return str(txt)
    except Exception:
        pass
    # A live region / status bar promoted to status carries its name as value.
    if role == "status":
        try:
            nm = node.get_name()
            if nm is not None:
                return str(nm)
        except Exception:
            pass
    return None


def _fmt_value(cv):
    """Render a numeric AT-SPI value into the strict period-decimal grammar
    `value_class` accepts: an integral value prints with no fraction (5.0 ->
    "5" -> POS1), otherwise the plain repr (locale-safe, period decimal)."""
    try:
        f = float(cv)
    except Exception:
        return str(cv)
    if f == int(f):
        return str(int(f))
    return repr(f)


def _label_of(node):
    """Display-only localized label (NEVER hashed)."""
    try:
        return (node.get_name() or "").strip()
    except Exception:
        return ""


def _atspi_key(node, role):
    """A STABLE, locale-invariant key for an offending node (matches the web
    runner's keyOf grammar): the accessible-id (the test-id analogue) when
    present, else role-typed. NEVER the visible text, so a translated label keeps
    the same finding id and OVERFLOW/CONTENTBUG findings reproduce byte-for-byte."""
    aid = _atspi_id(node)
    if aid:
        return "id:" + aid
    return "role:" + role


def _atspi_accessible_name(node):
    """The accessible NAME of an accessible: its AT-SPI name (the screen-reader
    announcement). This is NOT the value: an entry's typed text comes from the
    Text/Value interface, not get_name(), so a nameless entry is unlabeled. Used
    by the unlabeled oracle (an actionable element with no name is unannounceable)."""
    try:
        return (node.get_name() or "").strip()
    except Exception:
        return ""


def _atspi_node_extents(node):
    """The accessible's screen rectangle (x, y, width, height) via the Component
    interface get_extents, the SAME call the screenshot path uses for the window.
    Returns None when unavailable or zero-sized, so a node with no geometry is
    skipped (no false positive)."""
    try:
        comp = node.get_component_iface()
        if comp is None:
            return None
        # ATSPI_COORD_TYPE_SCREEN == 0 (screen-relative coordinates).
        ext = comp.get_extents(0)
        x, y, w, h = ext.x, ext.y, ext.width, ext.height
    except Exception:
        return None
    if w < 1 or h < 1:
        return None
    return (int(x), int(y), int(w), int(h))


def _anchor_of(app):
    """Screen anchor = window/view identity, if available. AT-SPI has no route,
    so use a stable window identity: the accessible-id of the top window, else
    its toolkit/app name."""
    try:
        for i in range(app.get_child_count()):
            w = app.get_child_at_index(i)
            aid = _atspi_id(w)
            if aid:
                return aid
    except Exception:
        pass
    aid = _atspi_id(app)
    if aid:
        return aid
    try:
        tk = (app.get_toolkit_name() or "").strip()
        if tk:
            return tk
    except Exception:
        pass
    return None


def _atspi_progressbar_role(node, role_name, role):
    """A PROGRESS_BAR maps to the transient `progress` by default (a loading
    indicator is dropped). But a progress bar that publishes a Value interface is
    a meaningful value-state surface, so promote it to the value-role
    `progressbar` (NOT transient; normalizes to `node`) when it has a readable
    value, exactly matching docs/signature.md's value-role set."""
    if role == "progress" and role_name == "PROGRESS_BAR":
        try:
            vi = node.get_value_iface()
            if vi is not None and vi.get_current_value() is not None:
                return "progressbar"
        except Exception:
            pass
    return role


def build_node(node, depth=0):
    """Walk a live AT-SPI accessible into a canonical Node tree (role + id +
    type + icon + value + children). Localized chrome name/text is excluded by
    construction; `value` is read only for value-bearing roles (docs/signature.md
    "Value-state") so the V: section carries the bounded value-class while the
    structural body stays text-free."""
    role_name = _atspi_role_name(node)
    role = atspi_role(role_name)
    role = _atspi_live_role(node, role_name, role)
    role = _atspi_progressbar_role(node, role_name, role)
    out = Node(
        role=role,
        id=_atspi_id(node),
        type=_atspi_input_type(role_name, role),
        icon=_atspi_icon(node),
        value=_atspi_value(node, role),
    )
    if depth < 60:
        try:
            for i in range(node.get_child_count()):
                child = node.get_child_at_index(i)
                if child is not None:
                    out.children.append(build_node(child, depth + 1))
        except Exception:
            pass
    return out


def find_app(Atspi, target):
    """Find the AT-SPI application node whose name matches target."""
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        try:
            if app and target.lower() in (app.get_name() or "").lower():
                return app
        except Exception:
            continue
    return None


def do_press(node):
    try:
        action = node.get_action_iface()
        if action and action.get_n_actions() > 0:
            action.do_action(0)
            return True
    except Exception:
        pass
    return False


# AT-SPI roles that respond to an action (push/click).
def _tappable_roles(Atspi):
    return {
        Atspi.Role.PUSH_BUTTON,
        Atspi.Role.MENU_ITEM,
        Atspi.Role.PAGE_TAB,
        Atspi.Role.LIST_ITEM,
        Atspi.Role.LINK,
        Atspi.Role.CHECK_BOX,
        Atspi.Role.RADIO_BUTTON,
        Atspi.Role.TOGGLE_BUTTON,
    }


# --- Layer 3: opt-in value selectors (config) -------------------------------

def load_value_node_selectors():
    """Read the optional `value_nodes:` selector list from reproit.yaml (Layer 3,
    docs/signature.md). Each entry is a selector string (`key:<id>` or
    `role:<role>#<idx>`) that marks an EXTRA node as value-bearing even when its
    role is not in the value-role set. Parsing is intentionally tiny (a flat
    `value_nodes:` block of `- selector` items) so no YAML dependency is needed;
    a missing file or block yields an empty list (no behavior change)."""
    path = os.environ.get("REPROIT_CONFIG") or os.path.join(os.getcwd(), "reproit.yaml")
    try:
        with open(path, "r", encoding="utf-8") as f:
            lines = f.read().splitlines()
    except Exception:
        return []
    out, in_block = [], False
    for raw in lines:
        line = raw.rstrip()
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if not line.startswith(" ") and not line.startswith("\t"):
            in_block = line.strip().rstrip(":") == "value_nodes" and line.rstrip().endswith(":")
            continue
        if in_block:
            item = line.strip()
            if item.startswith("-"):
                sel = item[1:].strip().strip('"').strip("'")
                if sel:
                    out.append(sel)
    return out


def _node_selector(node, structural_index):
    """The selector string for a node (matches the V: key namespace): `key:<id>`
    if present, else `role:<role>#<idx>` over the normalized role."""
    if node.id is not None:
        return "key:%s" % node.id
    return "role:%s#%d" % (normalize_role(node.role), structural_index)


def apply_value_nodes(root, selectors):
    """Set the `value_node` flag (Layer 3 opt-in) on every node whose selector is
    in `selectors`, so the oracle then treats it as value-bearing through the same
    path as a value-role node. No-op when `selectors` is empty (the value-less
    tree is unchanged)."""
    if not selectors:
        return
    sel = set(selectors)

    def root_visit(node):
        if _is_transient(node):
            return
        if _node_selector(node, 0) in sel:
            node.value_node = True
        _children_visit(node)

    def _children_visit(node):
        role_counts = {}
        for child in node.children:
            if _is_transient(child):
                continue
            role = normalize_role(child.role)
            idx = role_counts.get(role, 0)
            role_counts[role] = idx + 1
            if _node_selector(child, idx) in sel:
                child.value_node = True
            _children_visit(child)

    root_visit(root)


# --- Layer 1: effect detection (runner-local, NOT in the canonical signature) -

def content_fingerprint(anchor, root):
    """The runner-local content fingerprint (docs/signature.md "Value-state",
    Layer 1): the structural signature plus the sorted (stable-key, trimmed raw
    value) pairs over value-bearing nodes. This carries raw localized text and is
    EPHEMERAL: it is a per-step liveness check only and MUST NOT enter the
    canonical graph key. An action is effective iff the structural signature OR
    this fingerprint changed (so a counter 5->6, both POS1, is still effective)."""
    pairs = []

    def root_visit(node):
        if _is_transient(node):
            return
        if is_value_bearing(node):
            pairs.append((_node_selector(node, 0), (node.value or "").strip()))
        _children_visit(node)

    def _children_visit(node):
        role_counts = {}
        for child in node.children:
            if _is_transient(child):
                continue
            role = normalize_role(child.role)
            idx = role_counts.get(role, 0)
            role_counts[role] = idx + 1
            if is_value_bearing(child):
                pairs.append((_node_selector(child, idx), (child.value or "").strip()))
            _children_visit(child)

    root_visit(root)
    pairs.sort(key=lambda kv: kv[0])
    body = ";".join("%s=%s" % (k, v) for k, v in pairs)
    return signature(anchor, root) + "\x1f" + body


class ValueCap:
    """The runner-enforced hard cap (docs/signature.md "Value-state"): at most 8
    DISTINCT value-class combinations per structural node. The oracle is stateless
    and always computes the per-state value-class; the runner observes variants
    over time and, once a structural node has shown more than 8 distinct value
    states, drops that node from the V: section (falls back to structural-only) so
    an adversarial value generator cannot explode the graph.

    Keying is by structural sig (the value-less signature stands in for the
    structural-node identity); the tracked variant is the V: section content. Once
    a sig has accumulated > 8 distinct V: variants, `effective_signature` returns
    the structural-only signature for that sig."""

    CAP = 8

    def __init__(self):
        self._variants = {}   # structural sig -> set of V: variant strings
        self._capped = set()  # structural sigs that have blown the cap

    def effective_signature(self, anchor, root):
        struct_sig = signature(anchor, _structural_only(root))
        if struct_sig in self._capped:
            return struct_sig
        full_sig = signature(anchor, root)
        seen = self._variants.setdefault(struct_sig, set())
        seen.add(full_sig)
        if len(seen) > self.CAP:
            self._capped.add(struct_sig)
            return struct_sig
        return full_sig


def _structural_only(root):
    """A shallow copy of the tree with every `value`/`value_node` cleared, so its
    signature is the pure structural sig (no V: section). Used as the ValueCap key
    and as the fallback signature once a node blows the cap."""
    return Node(
        role=root.role,
        id=root.id,
        type=root.type,
        icon=root.icon,
        transient=root.transient,
        value=None,
        value_node=False,
        children=[_structural_only(c) for c in root.children],
    )


def snapshot(app, tappable_roles, value_selectors=None, cap=None):
    """Build the canonical signature for the current screen plus the display-only
    label list and the tappable index for the fuzz loop. Layer 3 selectors mark
    extra value nodes; Layer 1's content fingerprint is returned for the effect
    check; the ValueCap (Layer 2 runner bound) enforces <= 8 value variants per
    structural node, falling back to the structural-only sig past the cap."""
    anchor = _anchor_of(app)
    root = build_node(app, 0)
    apply_value_nodes(root, value_selectors or [])
    sig = cap.effective_signature(anchor, root) if cap is not None else signature(anchor, root)
    content = content_fingerprint(anchor, root)

    labels, tappables, node_by_label = [], [], {}
    # Oracle accumulators, filled in the SAME tree walk (no second pass).
    unlabeled = [0]
    overflows, overflow_seen = [], set()
    content_bugs, content_bug_seen = [], set()

    def visit(node, depth, parent_box):
        if depth > 60 or node is None:
            return
        try:
            atspi_role_enum = node.get_role()
        except Exception:
            atspi_role_enum = None
        # Raw AT-SPI role name + canonical string role (for stable keys).
        try:
            role_name = _atspi_role_name(node)
        except Exception:
            role_name = ""
        crole = atspi_role(role_name) if role_name else "node"
        is_tap = atspi_role_enum in tappable_roles
        label = _label_of(node)
        if label and len(label) <= MAX_LABEL_LEN:
            labels.append(label)
            if is_tap:
                tappables.append(label)
                node_by_label.setdefault(label, node)
        # UNLABELED oracle: an actionable accessible (a tappable role) with NO
        # accessible name is unannounceable to a screen reader. Count it, keyed off
        # role/actionability (structural), never text, so the count is the same
        # every run for the same tree. An entry's typed text is a VALUE not a name.
        if is_tap and not _atspi_accessible_name(node):
            unlabeled[0] += 1
        # CONTENT-BUG oracle: scan this accessible's label for a stringify/template
        # artifact, keyed by the stable node key + reason and deduped.
        if label:
            reason = content_bug_reason(label)
            if reason is not None:
                key = _atspi_key(node, crole)
                dedup = key + "|" + reason
                if dedup not in content_bug_seen:
                    content_bug_seen.add(dedup)
                    content_bugs.append((key, reason, label[:80]))
        # OVERFLOW oracle: this accessible's extents escaping the parent's content
        # box (AT-SPI exposes no padding, so the border box IS the content box) by
        # more than the tolerance. Pure geometry over the SAME get_extents the
        # screenshot path reads.
        box = _atspi_node_extents(node)
        if parent_box is not None and box is not None:
            px, py, pw, ph = parent_box
            cx, cy, cw, ch = box
            over = max((cx + cw) - (px + pw), px - cx,
                       (cy + ch) - (py + ph), py - cy)
            if over > OVERFLOW_TOL_PX:
                key = _atspi_key(node, crole)
                dedup = key + "|spill"
                if dedup not in overflow_seen:
                    overflow_seen.add(dedup)
                    overflows.append((key, "spill", int(round(over))))
        # The content box passed to children: this accessible's box UNLESS it is a
        # scroll container (a scroller is MEANT to hold larger content), so a
        # scroll pane / viewport suppresses overflow for its subtree. Keyed off the
        # raw AT-SPI role name (SCROLL_PANE/VIEWPORT both canonicalize to `group`,
        # which is too broad to suppress on).
        child_box = None if role_name in ("SCROLL_PANE", "VIEWPORT") else box
        try:
            for i in range(node.get_child_count()):
                visit(node.get_child_at_index(i), depth + 1, child_box)
        except Exception:
            pass

    visit(app, 0, None)
    uniq = list(dict.fromkeys(labels))
    # Stable order so the OVERFLOW/CONTENTBUG markers are byte-identical run to run.
    overflows.sort(key=lambda t: (t[0], t[1]))
    content_bugs.sort(key=lambda t: (t[0], t[1]))
    return {
        "sig": sig,
        "content": content,
        "labels": uniq,
        "tappables": list(dict.fromkeys(tappables)),
        "nodes": node_by_label,
        "unlabeled": unlabeled[0],
        "overflows": overflows,
        "content_bugs": content_bugs,
    }


def crash(title, detail):
    emit(f"EXCEPTION CAUGHT BY REPROIT ╡ {title} ╞")
    emit(f"The following condition was hit: {detail}")
    emit("═" * 8)


# ---- LEAK sampler (MEMORY:SAMPLE, --soak) -----------------------------------
# Under the soak tier (a replay script) we sample the target's resident set size
# (VmRSS from /proc/<pid>/status) once per replay cycle so the Rust soak oracle
# (modes/soak.rs) gets an RSS-vs-time series and reads the slope. VmRSS is the
# native analogue of the web runner's v8 heap_used; the marker shape is IDENTICAL
# ({"t_ms","heap_used"}) so soak.rs parses it unchanged (heap_used carries RSS
# bytes). /proc reports VmRSS in kB. No measurement is taken outside replay.
def _vmrss_bytes(pid):
    """The target's VmRSS in bytes from /proc/<pid>/status, or None on failure."""
    if not pid:
        return None
    try:
        with open("/proc/%d/status" % int(pid), "r", encoding="utf-8") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    parts = line.split()
                    # "VmRSS:  123456 kB"
                    if len(parts) >= 2:
                        return int(parts[1]) * 1024
    except Exception:
        return None
    return None


def sample_rss(pid, t_ms):
    """Emit MEMORY:SAMPLE {"t_ms","heap_used"} (heap_used = VmRSS bytes)."""
    rss = _vmrss_bytes(pid)
    if rss is not None:
        emit("MEMORY:SAMPLE " + json.dumps({"t_ms": int(t_ms), "heap_used": rss}))


# ---- HANG watchdog (EXPLORE:HANG) -------------------------------------------
# A deterministic wall-clock watchdog around each action's observe. AT-SPI has no
# OS "(Not Responding)" signal (unlike Windows IsHungAppWindow) and no main-thread
# Long-Tasks trace (the web runner's signal), so we time the blocking AT-SPI read
# round trip from THIS process: AT-SPI calls are synchronous D-Bus round trips
# that the target services on its main loop, so an app whose main thread froze
# makes the observe wall time spike. We bucket into one coarse, well-separated
# floor (HANG_FLOOR_MS) so timing jitter cannot flip the verdict, keyed by
# (from, action) like the web HANG. CAVEAT (documented gap): this is host-side
# wall time, perturbable by host/D-Bus scheduling, so it is not as deterministic
# as a frame trace; the high floor keeps it false-positive-free.
def maybe_emit_hang(from_sig, action, elapsed_ms):
    if elapsed_ms >= HANG_FLOOR_MS:
        emit("EXPLORE:HANG " + json.dumps(
            {"from": from_sig, "action": action, "bucket": HANG_FLOOR_MS}))


# ---- screenshot capture (SHOOT contract, see crates/.../backends/drive.rs) ---
# The orchestrator passes REPROIT_SHOTS_DIR (absolute) and, on a named shoot
# point, expects <dir>/<name>.png to exist before it reads `SHOOT:<name>` from
# stdout. <name> is [A-Za-z0-9_/-]. With REPROIT_SHOTS_DIR unset we still print
# the marker (capture is best-effort, the orchestrator just logs a miss).

_SHOOT_NAME_OK = frozenset(
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_/-")


def _atspi_window(app):
    """The app's first FRAME/WINDOW accessible (the top-level window), so the
    capture targets it rather than the whole desktop. Falls back to the app
    accessible itself when no window child is exposed."""
    try:
        for i in range(app.get_child_count()):
            w = app.get_child_at_index(i)
            rn = _atspi_role_name(w)
            if rn in ("FRAME", "WINDOW", "DIALOG"):
                return w
    except Exception:
        pass
    return app


def _atspi_window_extents(window):
    """The window's screen extents (x, y, width, height) from the AT-SPI Component
    interface. Returns None if unavailable or zero-sized."""
    try:
        comp = window.get_component_iface()
        if comp is None:
            return None
        # Coordinates relative to the screen (ATSPI_COORD_TYPE_SCREEN == 0).
        ext = comp.get_extents(0)
        x, y, w, h = ext.x, ext.y, ext.width, ext.height
    except Exception:
        return None
    if w < 1 or h < 1:
        return None
    return (int(x), int(y), int(w), int(h))


def _capture_window(window, out_path):
    """Capture the TARGET WINDOW to out_path (PNG), best-effort fallback chain.
    1) gnome-screenshot -w (active/focused window).
    2) ImageMagick `import` of the window's extents region (-window root + crop).
    3) `import -window root` crop, or scrot/grim cropped to the extents.
    Targets the window region rather than the full desktop wherever the geometry
    is known. Returns True on the first backend that writes the file."""
    extents = _atspi_window_extents(window)

    def _ran_ok():
        return os.path.exists(out_path) and os.path.getsize(out_path) > 0

    # 1) gnome-screenshot grabs the active window directly (GNOME sessions).
    if _which("gnome-screenshot"):
        try:
            subprocess.run(["gnome-screenshot", "-w", "-f", out_path],
                           timeout=10, check=False)
            if _ran_ok():
                return True
        except Exception:
            pass

    # 2) ImageMagick `import`: crop the root window to the AT-SPI extents.
    if extents is not None and _which("import"):
        x, y, w, h = extents
        try:
            subprocess.run(
                ["import", "-window", "root", "-crop", f"{w}x{h}+{x}+{y}", out_path],
                timeout=10, check=False)
            if _ran_ok():
                return True
        except Exception:
            pass

    # 3) grim (Wayland) or scrot (X11) cropped to the extents geometry.
    if extents is not None:
        x, y, w, h = extents
        if _which("grim"):
            try:
                subprocess.run(["grim", "-g", f"{x},{y} {w}x{h}", out_path],
                               timeout=10, check=False)
                if _ran_ok():
                    return True
            except Exception:
                pass
        if _which("scrot"):
            try:
                subprocess.run(
                    ["scrot", "-a", f"{x},{y},{w},{h}", out_path],
                    timeout=10, check=False)
                if _ran_ok():
                    return True
            except Exception:
                pass

    # 4) Last resort: whole screen via ImageMagick `import -window root`.
    if _which("import"):
        try:
            subprocess.run(["import", "-window", "root", out_path],
                           timeout=10, check=False)
            if _ran_ok():
                return True
        except Exception:
            pass
    return False


def _which(prog):
    """True if `prog` is on PATH (shutil.which, stdlib, importable anywhere)."""
    import shutil
    return shutil.which(prog) is not None


def shoot(app, name):
    """Capture the target window to <REPROIT_SHOTS_DIR>/<name>.png, then print
    SHOOT:<name>. <name> is sanitized to the contract's [A-Za-z0-9_/-]. With
    REPROIT_SHOTS_DIR unset, skip capture but still emit the marker."""
    name = "".join(c for c in name if c in _SHOOT_NAME_OK)
    if not name:
        return
    shots_dir = os.environ.get("REPROIT_SHOTS_DIR", "")
    if shots_dir:
        out_path = os.path.join(shots_dir, name + ".png")
        try:
            os.makedirs(os.path.dirname(out_path), exist_ok=True)
        except Exception:
            pass
        try:
            _capture_window(_atspi_window(app), out_path)
        except Exception:
            pass
    emit("SHOOT:" + name)


def main():
    try:
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
    except Exception as e:  # pragma: no cover - import guard for non-Linux hosts
        emit("EXCEPTION CAUGHT BY REPROIT ╡ Atspi unavailable ╞")
        emit(f"The following import failed (Linux-only backend): {e}")
        emit("═" * 8)
        sys.exit(3)

    target = os.environ.get("REPROIT_TARGET", "")
    if not target:
        sys.stderr.write("REPROIT_TARGET (app name or launch path) required\n")
        sys.exit(2)
    emit("JOURNEY claimed role=a")
    Atspi.init()

    if os.path.sep in target and os.path.exists(target):
        subprocess.Popen([target])
        time.sleep(2.5)
        app = find_app(Atspi, os.path.basename(target))
    else:
        app = find_app(Atspi, target)
    if app is None:
        crash("target not found", f"no AT-SPI application matching {target!r}")
        sys.exit(3)
    time.sleep(1.0)

    # Target pid for the --soak RSS sampler (AT-SPI exposes it on the app node).
    try:
        target_pid = int(app.get_process_id())
    except Exception:
        target_pid = 0

    tappable_roles = _tappable_roles(Atspi)

    # Layer 3 (config) + Layer 2 runner cap. The value-node selectors and the
    # per-structural-node value-class cap persist across the whole session (every
    # seed), so an adversarial value generator cannot evade the cap by resetting.
    value_selectors = load_value_node_selectors()
    cap = ValueCap()

    def reset_to_root():
        """Best-effort return the app to a comparable starting screen between
        seeds. AT-SPI has no widget-tree reset (the Flutter explorer re-pumps a
        fresh tree); the generic analogue is to escape out of any nested/modal
        screen. Several Escape presses unwind most navigation stacks. A planted
        dead-end with no exit stays put, which is fine: the next seed's frontier
        prefix re-navigates from wherever it lands, and per-seed coverage still
        accrues into the live map."""
        for _ in range(4):
            try:
                Atspi.generate_keyboard_event(9, "", Atspi.KeySynthType.PRESSRELEASE)
            except Exception:
                pass
            time.sleep(0.2)
        time.sleep(0.4)

    def run_seed(fuzz):
        """Explore/replay ONE seed, emitting the same EXPLORE:STATE /
        EXPLORE:EDGE / FUZZ:ACT / FUZZ:MISS markers as a single-seed run. Seen
        states + tried edges are local to the seed so per-seed coverage is
        independent, matching the other runners' per-seed contract."""
        rng = Rng(int(fuzz.get("seed", 0)))
        if fuzz.get("seed"):
            emit(f"JOURNEY[a] step: fuzz seed={fuzz['seed']}")

        seen, tried = set(), set()

        def observe():
            snap = snapshot(app, tappable_roles, value_selectors, cap)
            if snap["sig"] not in seen:
                seen.add(snap["sig"])
                # STATE carries the unlabeled count alongside labels; the core a11y
                # oracle (model/map.rs) reads json["unlabeled"] (defaults to 0).
                emit("EXPLORE:STATE " + json.dumps({
                    "sig": snap["sig"],
                    "labels": snap["labels"][:MAX_LABELS_PER_STATE],
                    "unlabeled": snap["unlabeled"],
                }))
                # OVERFLOW for this newly-seen state, keyed by the SAME sig. Only
                # emitted when a child actually spilled its container.
                if snap["overflows"]:
                    items = [{"key": k, "kind": kind, "by": by}
                             for (k, kind, by) in snap["overflows"]]
                    emit("EXPLORE:OVERFLOW " + json.dumps({"sig": snap["sig"], "items": items}))
                # CONTENT-BUG for this newly-seen state, keyed by the SAME sig. Only
                # emitted when a broken-content artifact is actually rendered.
                if snap["content_bugs"]:
                    items = [{"key": k, "reason": reason, "text": text}
                             for (k, reason, text) in snap["content_bugs"]]
                    emit("EXPLORE:CONTENTBUG " + json.dumps({"sig": snap["sig"], "items": items}))
            return snap

        current = observe()
        stuck = 0
        prefix = fuzz.get("prefix")
        replay = fuzz.get("replay")
        prefix_len = len(prefix) if prefix else 0
        budget = len(replay) if replay else (int(fuzz.get("budget", ACTION_BUDGET)) + prefix_len)
        edge_weights = fuzz.get("edgeWeights", {})

        # LEAK sampler (--soak): only in REPLAY mode (the soak tier writes
        # {"replay":[..]}) do we sample VmRSS, once at start and after each cycle,
        # forming the RSS-vs-time series soak.rs reads. No-op outside replay.
        is_soak = bool(replay)
        soak_start = time.monotonic()
        if is_soak:
            sample_rss(target_pid, 0)

        i = 0
        while i < budget and stuck < 3:
            # In replay/soak, sample RSS once per cycle (BEFORE acting, so cycle k's
            # sample reflects RSS after the previous action settled).
            if is_soak and i > 0:
                sample_rss(target_pid, (time.monotonic() - soak_start) * 1000)
            if replay:
                act = replay[i] if i < len(replay) else None
            elif prefix and i < prefix_len:
                act = prefix[i]
            elif fuzz.get("seed"):
                taps = sorted(current["tappables"])
                ew = edge_weights.get(current["sig"], {})
                options = [f"tap:{l}" for l in taps] + ["back"]
                weights = [1.0 / (1.0 + ew.get(o, 0)) for o in options]
                total = sum(weights)
                r = rng.unit() * total
                act = options[-1]
                for k, w in enumerate(weights):
                    r -= w
                    if r <= 0:
                        act = options[k]
                        break
            else:
                act = next((f"tap:{l}" for l in current["tappables"] if f"{current['sig']}|{l}" not in tried), "back")

            if act is None:
                break
            emit("FUZZ:ACT " + act)
            # Named screenshot point (from a replay/prefix script): capture the
            # target window to REPROIT_SHOTS_DIR and print SHOOT:<name>. Not a UI
            # action, so it does not advance `stuck` or count an edge.
            if act.startswith("shoot:"):
                shoot(app, act[len("shoot:"):])
                i += 1
                continue
            if act == "back":
                from_sig = current["sig"]
                Atspi.generate_keyboard_event(9, "", Atspi.KeySynthType.PRESSRELEASE)  # Escape keycode 9 (X11)
                time.sleep(0.6)
                # HANG watchdog: time ONLY the observe() round trip, after the
                # fixed settle, so the sleep is excluded by construction.
                observe_start = time.monotonic()
                nxt = observe()
                maybe_emit_hang(from_sig, "back", (time.monotonic() - observe_start) * 1000)
                if nxt["sig"] != current["sig"]:
                    emit("EXPLORE:EDGE " + json.dumps({"from": current["sig"], "action": "back", "to": nxt["sig"]}))
                # Layer 1 effect detection: an action is effective iff the
                # canonical signature OR the content fingerprint changed. Reset the
                # stall on any effective action (so a value-only change, e.g. a
                # counter tick, is not mistaken for a no-op); only a true no-op
                # advances `stuck`.
                if nxt["sig"] != current["sig"] or nxt["content"] != current["content"]:
                    stuck = 0
                else:
                    stuck += 1
                current = nxt
                i += 1
                continue
            label = act[len("tap:"):]
            from_sig = current["sig"]
            tried.add(f"{current['sig']}|{label}")
            node = current["nodes"].get(label)
            # HANG watchdog: time the synchronous press + observe round trip. The
            # fixed 0.7s settle sleep is subtracted so only blocking time crosses
            # the floor (a frozen main thread stalls the AT-SPI round trip).
            press_start = time.monotonic()
            if not node or not do_press(node):
                emit("FUZZ:MISS " + act)
                stuck += 1
                i += 1
                continue
            time.sleep(0.7)
            nxt = observe()
            maybe_emit_hang(from_sig, f"tap:{label}", (time.monotonic() - press_start) * 1000 - 700)
            if nxt["sig"] != current["sig"]:
                emit("EXPLORE:EDGE " + json.dumps({"from": current["sig"], "action": f"tap:{label}", "to": nxt["sig"]}))
            # Layer 1 effect detection: reset the stall whenever the action was
            # effective (structural sig OR content fingerprint moved), so a
            # value-only change keeps exploration alive instead of being treated
            # as a dead key.
            if nxt["sig"] != current["sig"] or nxt["content"] != current["content"]:
                stuck = 0
            current = nxt
            i += 1

        emit(f"JOURNEY[a] step: explored {len(seen)} states")

    # Run every seed in this session in sequence. For a multi-seed batch
    # ({"batch":[...]}) wrap EACH seed's walk in SEED:BEGIN <seed> ... SEED:END
    # <seed> so the Rust side (fuzz.rs split_seed_segments) attributes coverage,
    # trace, and findings to the right seed. A single-seed (legacy {"seed":..})
    # run emits NO SEED markers, preserving the byte-for-byte single-seed path.
    batch, is_batch = load_batch()
    for fuzz in batch:
        if is_batch:
            reset_to_root()
            emit(f"SEED:BEGIN {int(fuzz.get('seed', 0))}")
        run_seed(fuzz)
        if is_batch:
            emit(f"SEED:END {int(fuzz.get('seed', 0))}")

    emit("JOURNEY DONE")
    emit("All tests passed")


if __name__ == "__main__":
    main()
