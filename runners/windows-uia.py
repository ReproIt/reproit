# /// script
# requires-python = ">=3.9"
# dependencies = ["uiautomation"]
# ///
"""ReproIt Windows desktop runner (UI Automation backend).

Drives ANY native Windows app (WinUI, WPF, and Qt / Avalonia / wxWidgets
builds, which all publish to UI Automation) through the UIA tree and prints
the framework-agnostic marker protocol that `reproit` parses. The Windows twin
of runners/macos-ax.swift and runners/linux-atspi.py.

The screen signature is the CANONICAL structural signature defined in
docs/signature.md and proven by signature_vectors.json. This file is a Python
port of the Rust oracle (crates/reproit/src/model/signature.rs): it walks the
UIA tree into a normalized Node tree (role from ControlType -> the fixed
vocabulary, id from AutomationId, type for inputs, icon if available), then
serializes the descriptor and hashes it FNV-1a 32-bit. Localized Name/text
NEVER enters the hash; it is kept only as a display-only label list.

The signature core (Node, descriptor, signature, plus uia_role / uia_to_node)
is importable WITHOUT a Windows host: the `uiautomation` import is deferred to
main(), so runners/test_signature.py can prove parity on any platform.

Run with uv (auto-installs `uiautomation`):
    uv run runners/windows-uia.py

Env (set by drive.rs):
    REPROIT_TARGET        window title substring, or path to launch
    REPROIT_FUZZ_CONFIG   fuzz config json (seed/budget/replay/prefix/edgeWeights)

Windows-only: drives the live UI Automation API. The signature function +
parity test run anywhere.
"""

import json
import os
import subprocess
import sys
import time

ACTION_BUDGET = 36
MAX_LABEL_LEN = 40
MAX_LABELS_PER_STATE = 24


# =============================================================================
# Canonical structural signature (port of crates/reproit/src/model/signature.rs)
# This block has NO Windows imports and is what runners/test_signature.py loads.
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
# UIA ControlType -> canonical role vocabulary.
# Keyed by the ControlType *name* (string) so this map is importable without the
# `uiautomation` package. uia_role() accepts either a ControlType name string or
# the numeric ControlType id (resolved lazily against the live package).
# =============================================================================

# ControlType name (uiautomation's `ControlType.<X>` -> "<X>") to canonical role.
# Roles outside the vocabulary fall through to `node` via normalize_role.
UIA_CONTROLTYPE_TO_ROLE = {
    "WindowControl": "screen",
    "PaneControl": "group",
    "GroupControl": "group",
    "CustomControl": "group",
    "HeaderControl": "header",
    "HeaderItemControl": "header",
    "TitleBarControl": "header",
    "TextControl": "text",
    "StatusBarControl": "text",
    "ButtonControl": "button",
    "SplitButtonControl": "button",
    "HyperlinkControl": "link",
    "EditControl": "textfield",
    "DocumentControl": "textfield",
    "ComboBoxControl": "textfield",
    "ImageControl": "image",
    "ListControl": "list",
    "DataGridControl": "list",
    "TableControl": "list",
    "TreeControl": "list",
    "ListItemControl": "listitem",
    "DataItemControl": "listitem",
    "TreeItemControl": "listitem",
    "TabControl": "tab",
    "TabItemControl": "tab",
    "CheckBoxControl": "checkbox",
    "RadioButtonControl": "radio",
    "SliderControl": "slider",
    "ProgressBarControl": "progress",  # transient -> dropped
    "MenuControl": "menu",
    "MenuBarControl": "menu",
    "MenuItemControl": "menuitem",
    "WindowControlDialog": "dialog",
    "ToolTipControl": "tooltip",       # transient -> dropped
    "SeparatorControl": "node",
    "ToolBarControl": "group",
    "ScrollBarControl": "node",
    "SpinnerControl": "spinner",       # transient -> dropped
    "ThumbControl": "node",
    "CalendarControl": "group",
}

# A Toggle/Switch is exposed in UIA as a CheckButton with a Toggle pattern; many
# frameworks publish an explicit "switch"-style LocalizedControlType. We map the
# generic CheckBox to `checkbox`; live capture promotes it to `switch` when the
# control reports a switch localized type (see _uia_role_live).

# Input `type` refinement for EditControls, by the IsPassword flag / a11y hints.
# Filled in by live capture; the vocabulary is the spec's input-type set.


def uia_role(control_type_name):
    """Map a UIA ControlType *name* string to the canonical role vocabulary.
    Unknown control types normalize to `node`."""
    return normalize_role(UIA_CONTROLTYPE_TO_ROLE.get(control_type_name, "node"))


# =============================================================================
# Live UIA capture (Windows only). Everything below imports `uiautomation`.
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


class Rng:
    """xorshift32, identical recurrence to every other runner."""

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


def _control_type_name(auto, ctrl):
    """Resolve the ControlType *name* string for a live UIA control, so it keys
    UIA_CONTROLTYPE_TO_ROLE."""
    try:
        ct = ctrl.ControlType
    except Exception:
        return ""
    # uiautomation exposes ControlType as an IntEnum-like; map id -> name.
    try:
        for name in dir(auto.ControlType):
            if name.endswith("Control") and getattr(auto.ControlType, name) == ct:
                return name
    except Exception:
        pass
    return ""


def _uia_role_live(auto, ctrl, ct_name):
    """Role for a live control: base map, then promote a toggle CheckBox to
    `switch` when the control localizes itself as a switch/toggle, and promote a
    Text control with a live region (LiveSetting != Off) to the value-role
    `status` so its changing value-class folds into the canonical signature
    (docs/signature.md "Value-state"). The Text->status promotion is RAW-role
    only: `status` is not in the structural vocabulary, so it normalizes to `node`
    in the body, exactly like the oracle's status nodes."""
    role = uia_role(ct_name)
    if role == "checkbox":
        try:
            loc = (ctrl.LocalizedControlType or "").lower()
        except Exception:
            loc = ""
        if "switch" in loc or "toggle" in loc:
            return "switch"
    if role == "text" and _uia_is_live(ctrl):
        return "status"
    # A ProgressBar maps to the transient `progress` by default (a loading
    # spinner is dropped). But a ProgressBar that publishes a RangeValue is a
    # meaningful value-state surface, so promote it to the value-role
    # `progressbar` (NOT transient; normalizes to `node` in the body) when it
    # carries a readable range value, exactly matching docs/signature.md's
    # value-role set.
    if role == "progress":
        try:
            rp = ctrl.GetRangeValuePattern()
            if rp is not None and rp.Value is not None:
                return "progressbar"
        except Exception:
            pass
    return role


def _uia_is_live(ctrl):
    """True when a control declares an active live region (LiveSetting is Polite
    or Assertive, i.e. != Off). UIA exposes LiveSetting as an enum where 0 is Off;
    a non-zero setting means the control announces value changes, so we treat it
    as value-bearing status."""
    try:
        ls = ctrl.GetPropertyValue(auto_LiveSettingPropertyId())
    except Exception:
        ls = None
    if ls is None:
        try:
            ls = ctrl.LiveSetting
        except Exception:
            ls = None
    try:
        return ls is not None and int(ls) != 0
    except Exception:
        return False


def auto_LiveSettingPropertyId():
    """The UIA LiveSettingProperty id (30135), used when the typed accessor is not
    surfaced by `uiautomation`. Returning the well-known id keeps this importable
    without the package."""
    return 30135


def _uia_input_type(auto, ctrl, role):
    """Input `type` refinement for textfields, drawn from language-independent
    hints (IsPassword). Returns None when there is nothing to refine."""
    if role != "textfield":
        return None
    try:
        vp = ctrl.GetValuePattern()
        if vp and getattr(vp, "IsReadOnly", False):
            pass
    except Exception:
        pass
    try:
        if ctrl.IsPassword:
            return "password"
    except Exception:
        pass
    return None


def _uia_value(auto, ctrl, role):
    """The displayed data value for a value-bearing control (docs/signature.md
    "Value-state", Layer 2). Read from the ValuePattern (Edit / Document /
    ComboBox) or the RangeValuePattern (Slider / ProgressBar), or the live Text's
    name. Returns None for chrome roles so the V: section is never polluted by
    chrome text. The raw string is bucketed later by `value_class`; the raw text
    itself never enters the canonical body."""
    if role not in VALUE_ROLES:
        return None
    # ValuePattern.Value: Edit / Document / ComboBox text.
    try:
        vp = ctrl.GetValuePattern()
        if vp is not None:
            v = vp.Value
            if v is not None:
                return str(v)
    except Exception:
        pass
    # RangeValuePattern.Value: Slider / ProgressBar numeric position.
    try:
        rp = ctrl.GetRangeValuePattern()
        if rp is not None:
            v = rp.Value
            if v is not None:
                return _fmt_range_value(v)
    except Exception:
        pass
    # A live Text promoted to status carries its announced text as the value.
    if role == "status":
        try:
            name = ctrl.Name
            if name is not None:
                return str(name)
        except Exception:
            pass
    return None


def _fmt_range_value(v):
    """Render a RangeValue (a float) into the strict period-decimal grammar
    `value_class` accepts: an integral value prints with no fraction (so 5.0 ->
    "5" -> POS1), otherwise the plain repr (locale-safe, period decimal)."""
    try:
        f = float(v)
    except Exception:
        return str(v)
    if f == int(f):
        return str(int(f))
    return repr(f)


def _uia_id(ctrl):
    """Stable developer id from AutomationId (omitted if empty)."""
    try:
        aid = (ctrl.AutomationId or "").strip()
    except Exception:
        aid = ""
    return aid or None


def _uia_icon(ctrl):
    """Language-independent icon identity, if the framework publishes one. UIA
    has no standard icon attribute, so this is None unless an automation
    annotation exposes one; left as a hook for frameworks that do."""
    return None


def _label_of(ctrl):
    """Display-only localized label (NEVER hashed)."""
    try:
        name = (ctrl.Name or "").strip()
    except Exception:
        name = ""
    if name:
        return name
    try:
        return (ctrl.GetLegacyIAccessiblePattern().Value or "").strip()
    except Exception:
        return ""


def _anchor_of(auto, window):
    """Screen anchor = window/view identity, if available. UIA has no route, so
    use a stable window identity: AutomationId, else ClassName."""
    aid = _uia_id(window)
    if aid:
        return aid
    try:
        cn = (window.ClassName or "").strip()
    except Exception:
        cn = ""
    return cn or None


def build_node(auto, ctrl, depth=0):
    """Walk a live UIA control into a canonical Node tree (role + id + type +
    icon + value + children). Localized chrome Name/text is excluded by
    construction; `value` is read only for value-bearing roles (docs/signature.md
    "Value-state") so the V: section carries the bounded value-class while the
    structural body stays text-free."""
    ct_name = _control_type_name(auto, ctrl)
    role = _uia_role_live(auto, ctrl, ct_name)
    node = Node(
        role=role,
        id=_uia_id(ctrl),
        type=_uia_input_type(auto, ctrl, role),
        icon=_uia_icon(ctrl),
        value=_uia_value(auto, ctrl, role),
    )
    if depth < 60:
        try:
            for child in ctrl.GetChildren():
                node.children.append(build_node(auto, child, depth + 1))
        except Exception:
            pass
    return node


# UIA control types that respond to an Invoke/press.
def _tappable_types(auto):
    return {
        auto.ControlType.ButtonControl,
        auto.ControlType.MenuItemControl,
        auto.ControlType.TabItemControl,
        auto.ControlType.ListItemControl,
        auto.ControlType.HyperlinkControl,
        auto.ControlType.CheckBoxControl,
        auto.ControlType.RadioButtonControl,
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


def snapshot(auto, window, tappable_types, value_selectors=None, cap=None):
    """Build the canonical signature for the current screen plus the display-only
    label list and the tappable index for the fuzz loop. Layer 3 selectors mark
    extra value nodes; Layer 1's content fingerprint is returned for the effect
    check; the ValueCap (Layer 2 runner bound) enforces <= 8 value variants per
    structural node, falling back to the structural-only sig past the cap."""
    anchor = _anchor_of(auto, window)
    root = build_node(auto, window, 0)
    apply_value_nodes(root, value_selectors or [])
    sig = cap.effective_signature(anchor, root) if cap is not None else signature(anchor, root)
    content = content_fingerprint(anchor, root)

    labels, tappables, node_by_label = [], [], {}

    def visit(ctrl, depth):
        if depth > 60:
            return
        label = _label_of(ctrl)
        if label and len(label) <= MAX_LABEL_LEN:
            labels.append(label)
            try:
                is_tap = ctrl.ControlType in tappable_types
            except Exception:
                is_tap = False
            if is_tap:
                tappables.append(label)
                node_by_label.setdefault(label, ctrl)
        try:
            for child in ctrl.GetChildren():
                visit(child, depth + 1)
        except Exception:
            pass

    visit(window, 0)
    uniq = list(dict.fromkeys(labels))
    return {
        "sig": sig,
        "content": content,
        "labels": uniq,
        "tappables": list(dict.fromkeys(tappables)),
        "nodes": node_by_label,
    }


def press(ctrl):
    for pat in ("GetInvokePattern", "GetTogglePattern", "GetSelectionItemPattern"):
        try:
            p = getattr(ctrl, pat)()
            if p:
                (p.Invoke if hasattr(p, "Invoke") else p.Toggle if hasattr(p, "Toggle") else p.Select)()
                return True
        except Exception:
            continue
    try:
        ctrl.Click(simulateMove=False)
        return True
    except Exception:
        return False


def crash(title, detail):
    emit(f"EXCEPTION CAUGHT BY REPROIT ╡ {title} ╞")
    emit(f"The following condition was hit: {detail}")
    emit("═" * 8)


# ---- screenshot capture (SHOOT contract, see crates/.../backends/drive.rs) ---
# The orchestrator passes REPROIT_SHOTS_DIR (absolute) and, on a named shoot
# point, expects <dir>/<name>.png to exist before it reads `SHOOT:<name>` from
# stdout. <name> is [A-Za-z0-9_/-]. With REPROIT_SHOTS_DIR unset we still print
# the marker (capture is best-effort, the orchestrator just logs a miss).

_SHOOT_NAME_OK = frozenset(
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_/-")


def _window_rect(window):
    """The target window's screen bounding rectangle (left, top, right, bottom),
    from the UIA element's BoundingRectangle. Returns None if unavailable."""
    try:
        r = window.BoundingRectangle
    except Exception:
        return None
    try:
        left, top, right, bottom = r.left, r.top, r.right, r.bottom
    except Exception:
        try:
            left, top, right, bottom = r[0], r[1], r[2], r[3]
        except Exception:
            return None
    if right - left < 1 or bottom - top < 1:
        return None
    return (int(left), int(top), int(right), int(bottom))


def _capture_window(window, out_path):
    """Capture the TARGET WINDOW to out_path (PNG). Targets the window rect, not
    the desktop. Tries, in order: PrintWindow on the native handle (captures even
    an occluded window), then a grab of the window's BoundingRectangle via mss,
    then PIL ImageGrab. Returns True on success. Heavy deps are optional: any
    missing backend is skipped, the next is tried."""
    rect = _window_rect(window)
    # 1) PrintWindow via the native handle: pulls the window's own pixels even if
    #    it is behind another window. Uses ctypes + the GDI handle; no new deps.
    try:
        hwnd = int(window.NativeWindowHandle)
    except Exception:
        hwnd = 0
    if hwnd and rect is not None and _printwindow_capture(hwnd, rect, out_path):
        return True
    # 2) mss: fast screen grab of the window rect (left, top, width, height).
    if rect is not None:
        try:
            import mss  # type: ignore

            left, top, right, bottom = rect
            with mss.mss() as sct:
                img = sct.grab({"left": left, "top": top,
                                "width": right - left, "height": bottom - top})
                import mss.tools  # type: ignore

                mss.tools.to_png(img.rgb, img.size, output=out_path)
            return True
        except Exception:
            pass
    # 3) PIL ImageGrab of the bbox (left, top, right, bottom).
    if rect is not None:
        try:
            from PIL import ImageGrab  # type: ignore

            ImageGrab.grab(bbox=rect).save(out_path)
            return True
        except Exception:
            pass
    return False


def _printwindow_capture(hwnd, rect, out_path):
    """ctypes PrintWindow: render the window into a memory DC, then save it as a
    BMP we hand to PIL for PNG encoding (PIL is the only optional dep here, and we
    fall back to other backends if it is absent). Best-effort: any failure returns
    False so the caller tries the next backend."""
    try:
        import ctypes
        from ctypes import wintypes

        left, top, right, bottom = rect
        w, h = right - left, bottom - top
        user32 = ctypes.windll.user32
        gdi32 = ctypes.windll.gdi32
        hwnd_dc = user32.GetWindowDC(hwnd)
        if not hwnd_dc:
            return False
        mem_dc = gdi32.CreateCompatibleDC(hwnd_dc)
        bmp = gdi32.CreateCompatibleBitmap(hwnd_dc, w, h)
        gdi32.SelectObject(mem_dc, bmp)
        # PW_RENDERFULLCONTENT (0x2) so DWM-composited content is included.
        ok = user32.PrintWindow(hwnd, mem_dc, 2)
        if not ok:
            ok = user32.PrintWindow(hwnd, mem_dc, 0)
        try:
            from PIL import Image  # type: ignore

            class BITMAPINFOHEADER(ctypes.Structure):
                _fields_ = [
                    ("biSize", wintypes.DWORD), ("biWidth", wintypes.LONG),
                    ("biHeight", wintypes.LONG), ("biPlanes", wintypes.WORD),
                    ("biBitCount", wintypes.WORD), ("biCompression", wintypes.DWORD),
                    ("biSizeImage", wintypes.DWORD), ("biXPelsPerMeter", wintypes.LONG),
                    ("biYPelsPerMeter", wintypes.LONG), ("biClrUsed", wintypes.DWORD),
                    ("biClrImportant", wintypes.DWORD),
                ]

            bmi = BITMAPINFOHEADER()
            bmi.biSize = ctypes.sizeof(BITMAPINFOHEADER)
            bmi.biWidth, bmi.biHeight = w, -h  # top-down
            bmi.biPlanes, bmi.biBitCount = 1, 32
            bmi.biCompression = 0  # BI_RGB
            buf = ctypes.create_string_buffer(w * h * 4)
            gdi32.GetDIBits(mem_dc, bmp, 0, h, buf, ctypes.byref(bmi), 0)
            img = Image.frombuffer("RGB", (w, h), buf, "raw", "BGRX", 0, 1)
            img.save(out_path)
            result = True
        except Exception:
            result = False
        gdi32.DeleteObject(bmp)
        gdi32.DeleteDC(mem_dc)
        user32.ReleaseDC(hwnd, hwnd_dc)
        return bool(ok) and result
    except Exception:
        return False


def shoot(window, name):
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
            _capture_window(window, out_path)
        except Exception:
            pass
    emit("SHOOT:" + name)


def main():
    try:
        import uiautomation as auto
    except Exception as e:  # pragma: no cover - import guard for non-Windows hosts
        emit("EXCEPTION CAUGHT BY REPROIT ╡ uiautomation unavailable ╞")
        emit(f"The following import failed (Windows-only backend): {e}")
        emit("═" * 8)
        sys.exit(3)

    target = os.environ.get("REPROIT_TARGET", "")
    if not target:
        sys.stderr.write("REPROIT_TARGET (window title or launch path) required\n")
        sys.exit(2)
    emit("JOURNEY claimed role=a")

    # Launch if it looks like a path, then bind by top-level window.
    if os.path.sep in target and os.path.exists(target):
        subprocess.Popen([target])
        time.sleep(2.0)
        window = auto.GetForegroundControl()
    else:
        window = auto.WindowControl(searchDepth=1, SubName=target)
    if not window.Exists(maxSearchSeconds=8):
        crash("target not found", f"no window matching {target!r}")
        sys.exit(3)
    window.SetActive()
    time.sleep(1.0)

    tappable_types = _tappable_types(auto)

    # Layer 3 (config) + Layer 2 runner cap. The value-node selectors and the
    # per-structural-node value-class cap persist across the whole session.
    value_selectors = load_value_node_selectors()
    cap = ValueCap()

    fuzz = load_fuzz()
    rng = Rng(int(fuzz.get("seed", 0)))
    if fuzz.get("seed"):
        emit(f"JOURNEY[a] step: fuzz seed={fuzz['seed']}")

    seen, tried = set(), set()

    def observe():
        snap = snapshot(auto, window, tappable_types, value_selectors, cap)
        if snap["sig"] not in seen:
            seen.add(snap["sig"])
            emit("EXPLORE:STATE " + json.dumps({"sig": snap["sig"], "labels": snap["labels"][:MAX_LABELS_PER_STATE]}))
        return snap

    current = observe()
    stuck = 0
    prefix = fuzz.get("prefix")
    replay = fuzz.get("replay")
    prefix_len = len(prefix) if prefix else 0
    budget = len(replay) if replay else (int(fuzz.get("budget", ACTION_BUDGET)) + prefix_len)
    edge_weights = fuzz.get("edgeWeights", {})

    i = 0
    while i < budget and stuck < 3:
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
        # Named screenshot point (from a replay/prefix script): capture the target
        # window to REPROIT_SHOTS_DIR and print SHOOT:<name>. Not a UI action, so
        # it does not advance `stuck` or count an edge.
        if act.startswith("shoot:"):
            shoot(window, act[len("shoot:"):])
            i += 1
            continue
        if act == "back":
            auto.SendKeys("{Esc}")
            time.sleep(0.6)
            nxt = observe()
            if nxt["sig"] != current["sig"]:
                emit("EXPLORE:EDGE " + json.dumps({"from": current["sig"], "action": "back", "to": nxt["sig"]}))
            # Layer 1 effect detection: an action is effective iff the canonical
            # signature OR the content fingerprint changed. Reset the stall on any
            # effective action (so a value-only change, e.g. a counter tick, is
            # not mistaken for a no-op); only a true no-op advances `stuck`.
            if nxt["sig"] != current["sig"] or nxt["content"] != current["content"]:
                stuck = 0
            else:
                stuck += 1
            current = nxt
            i += 1
            continue
        label = act[len("tap:"):]
        tried.add(f"{current['sig']}|{label}")
        node = current["nodes"].get(label)
        if not node or not press(node):
            emit("FUZZ:MISS " + act)
            stuck += 1
            i += 1
            continue
        time.sleep(0.7)
        if not window.Exists(maxSearchSeconds=1):
            crash("target window gone", f"the window vanished during {act}")
            break
        nxt = observe()
        if nxt["sig"] != current["sig"]:
            emit("EXPLORE:EDGE " + json.dumps({"from": current["sig"], "action": f"tap:{label}", "to": nxt["sig"]}))
        # Layer 1 effect detection: reset the stall whenever the action was
        # effective (structural sig OR content fingerprint moved), so a value-only
        # change keeps exploration alive instead of being treated as a dead key.
        if nxt["sig"] != current["sig"] or nxt["content"] != current["content"]:
            stuck = 0
        current = nxt
        i += 1

    emit(f"JOURNEY[a] step: explored {len(seen)} states")
    emit("JOURNEY DONE")
    emit("All tests passed")


if __name__ == "__main__":
    main()
