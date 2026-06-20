"""Canonical structural screen signature for native Linux GUI apps.

Byte-identical to the Rust oracle (crates/reproit/src/model/signature.rs), the
React Native SDK (sdk/reproit-react-native/src/signature.ts), the web SDK, and
the Linux runner (runners/linux-atspi.py). Spec: docs/signature.md. Proven
against the 24 golden vectors in signature_vectors.json by
tests/test_parity.py.

A signature hashes STRUCTURE (roles + ids + types + icons + tree shape), never
localized text, so an English and a Japanese render of the same screen hash
identically. The descriptor is:

    "A:" + anchor + "\\n" + tokens.join(";") + value_section

where each retained node emits one pre-order token:

    <depth>:<role>[:<type>][#<icon>][@<id>]   (plus "*" when collapsed)

hashed with FNV-1a 32-bit (offset 0x811c9dc5, prime 0x01000193) into 8
lowercase hex chars.

This module has NO Linux/GTK imports: it is pure Python so the parity test runs
on any host. The live AT-SPI / GTK capture lives in capture.py.
"""

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
# exclusion. Several of these (status, log, progressbar, meter, timer, output)
# are NOT in the structural ROLES vocabulary, so they normalize to `node` in the
# token body; the value-role test therefore uses the RAW role, not the
# normalized one.
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
    excluded from the descriptor by construction (rule 1). `value` is the
    displayed data value (Layer 2) and is consulted ONLY when the node is
    value-bearing (a value-role or `value_node`-flagged); chrome text never goes
    here. Defaults keep a value-less tree byte-identical to a pre-value-state
    one.
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
    subtrees, keep document order. Returns None if this node itself is
    transient."""
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
    the pre-order token list of this subtree, depths re-based to 0, so two
    sibling subtrees at the same level compare equal regardless of absolute
    depth."""
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
    has a `value` AND it is value-bearing, i.e. its RAW role is a value-role OR
    it is `value_node`-flagged (Layer 3 opt-in). The raw role is used
    deliberately: roles like `status`/`meter` normalize to `node` but are still
    value-roles."""
    return node.value is not None and (node.role in VALUE_ROLES or node.value_node)


def value_class(s):
    r"""Map a value string to a bounded, deterministic, locale-safe value-class
    token. The numeric branch accepts ONLY the strict period-decimal grammar
    `^[+-]?[0-9]+(\.[0-9]+)?$` (no grouping, no exponent, no leading/trailing
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
    r"""Strict `^[+-]?[0-9]+(\.[0-9]+)?$`: optional sign, one or more ASCII
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
    r"""Build the V: section suffix. Returns "" when there are NO value-bearing
    nodes, keeping the descriptor (and hash) byte-identical to a pre-value-state
    tree. Otherwise `"\nV:" + key=class;key=class...` sorted by key."""
    pairs = []
    _collect_values(root, pairs)
    if not pairs:
        return ""
    pairs.sort(key=lambda kc: kc[0])
    body = ";".join("%s=%s" % (k, c) for k, c in pairs)
    return "\nV:%s" % body


def descriptor(anchor, root):
    r"""Build the exact UTF-8 descriptor string that gets hashed
    (docs/signature.md "Descriptor serialization"):
    `"A:" + anchor + "\n" + tokens.join(";") + value_section`. The `A:` prefix
    line is always present, even with no anchor. The V: section (Layer 2
    value-classes) is appended ONLY when at least one value-bearing node exists,
    so a value-less tree is byte-identical to the pre-value-state descriptor."""
    tokens = []
    norm = _normalize(root)
    if norm is not None:
        _serialize_node(norm, 0, False, tokens)
    return "A:%s\n%s%s" % (anchor or "", ";".join(tokens), _value_section(root))


def fnv1a32_hex(data):
    """FNV-1a, 32-bit, over `data` bytes; 8-char zero-padded lowercase hex
    (docs/signature.md "Hash"). Offset basis 0x811c9dc5, prime 0x01000193."""
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
    return value (nokey) is metadata for `map show`; it does NOT affect the
    hash."""
    if id is not None:
        return ("key:%s" % id, False)
    return ("role:%s#%d" % (normalize_role(role), structural_index), True)
