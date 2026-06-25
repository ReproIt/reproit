/// Canonical structural screen signature for Flutter.
///
/// This is the Dart port of the Rust parity oracle
/// (`crates/reproit/src/model/signature.rs`). `docs/signature.md` is the spec;
/// `signature_vectors.json` (repo root) holds the golden vectors every
/// implementation must reproduce bit-for-bit. Both the production SDK
/// (`reproit_flutter.dart`) and the fuzz explorer templates (`templates/*.dart`)
/// compute the signature through THIS file, so they agree by construction.
///
/// The descriptor string that gets hashed is built exactly as the spec defines:
///
///   token  = `<depth>:<role>[:<type>][#<icon>][@<id>]` (with a trailing `*`
///            on a collapsed repeat)
///   body   = tokens joined by `;`, pre-order
///   desc   = `"A:" + anchor + "\n" + body`
///   sig    = FNV-1a 32-bit over the UTF-8 bytes of desc, 8-char lowercase hex
library reproit_signature;

/// The fixed, language-independent role vocabulary (docs/signature.md "Roles").
/// Anything outside this set normalizes to `node`.
const List<String> kRoles = <String>[
  'screen',
  'header',
  'text',
  'button',
  'link',
  'textfield',
  'image',
  'icon',
  'list',
  'listitem',
  'tab',
  'switch',
  'checkbox',
  'radio',
  'slider',
  'menu',
  'menuitem',
  'dialog',
  'group',
  'node',
];

/// Roles that flicker in and out of the tree and must be dropped before hashing
/// (docs/signature.md normalization rule 2). "transient error banner" is not a
/// distinct role in the vocabulary, so it is expressed via the [RNode.transient]
/// flag; both paths drop the node and its whole subtree. `progress` is the role
/// name for spinner/progress.
const Set<String> kTransientRoles = <String>{
  'toast',
  'snackbar',
  'spinner',
  'progress',
  'tooltip',
  'badge',
};

/// Value-role set (docs/signature.md "Value-state", Layer 2). A node carries a
/// canonical value-class in the `V:` section only if it has a [RNode.value] AND
/// either its RAW role is in this set OR it is flagged [RNode.valueNode] (the
/// Layer 3 opt-in). Several of these (`status, log, progressbar, meter, timer,
/// output`) are NOT in the structural [kRoles] vocabulary, so they normalize to
/// `node` in the descriptor body; the value-role test therefore uses the RAW
/// role, not the normalized one. Chrome roles (button/label/header/text/...) are
/// NEVER value-bearing, so rule 1's chrome-text exclusion is preserved exactly.
const Set<String> kValueRoles = <String>{
  'textfield',
  'status',
  'log',
  'progressbar',
  'meter',
  'timer',
  'output',
};

/// A normalized accessibility node: the input to the signature.
///
/// Mirrors the Rust `Node` JSON shape so each golden vector's `tree` parses
/// directly via [RNode.fromJson]:
/// ```json
/// { "role": "button", "id": "submit", "type": "text",
///   "icon": "e5cd", "transient": false, "children": [ ... ] }
/// ```
/// All fields except `role`/`children` are optional. There is deliberately no
/// text/label/value field: localized text is excluded from the descriptor by
/// construction (rule 1), so there is nothing to hash.
class RNode {
  RNode({
    required this.role,
    this.id,
    this.type,
    this.icon,
    this.transient = false,
    this.value,
    this.valueNode = false,
    List<RNode>? children,
  }) : children = children ?? <RNode>[];

  /// Role from the fixed vocabulary; unknown roles normalize to `node`.
  final String role;

  /// Stable developer identifier (key / test-id / a11y-id / resource-id).
  final String? id;

  /// Optional input-type refinement (text, password, email, ...).
  final String? type;

  /// Optional language-independent icon identity (codepoint / symbol / asset).
  final String? icon;

  /// Explicit transient marker (e.g. a transient error banner). Dropped like a
  /// transient role.
  final bool transient;

  /// The node's displayed data value (Layer 2, docs/signature.md "Value-state").
  /// Only consulted when the node is value-bearing (a value-role or a
  /// [valueNode]-flagged node). Chrome text never goes here. Null by default, so
  /// a tree with no values is byte-identical to a pre-value-state tree.
  final String? value;

  /// Opt-in value-node flag (Layer 3). When true the node is treated as
  /// value-bearing even if its role is not in [kValueRoles] (a `reproit.yaml`
  /// `value_nodes:` selector resolves to this flag). False by default.
  final bool valueNode;

  /// Ordered children, in document order.
  final List<RNode> children;

  /// Parse the JSON shape stored in `signature_vectors.json`. This is the form
  /// the parity gate feeds in; it must accept exactly the fields the Rust `Node`
  /// serializes.
  static RNode fromJson(Map<String, dynamic> j) {
    final kids = (j['children'] as List?)
            ?.map((e) => RNode.fromJson((e as Map).cast<String, dynamic>()))
            .toList() ??
        <RNode>[];
    return RNode(
      role: j['role'] as String,
      id: j['id'] as String?,
      type: j['type'] as String?,
      icon: j['icon'] as String?,
      transient: (j['transient'] as bool?) ?? false,
      value: j['value'] as String?,
      valueNode: (j['value_node'] as bool?) ?? false,
      children: kids,
    );
  }
}

/// Normalize a role to the fixed vocabulary: known roles pass through, unknown
/// roles map to `node` (docs/signature.md "Roles").
String normalizeRole(String role) => kRoles.contains(role) ? role : 'node';

bool _isTransient(RNode n) =>
    n.transient || kTransientRoles.contains(n.role);

/// A normalized node after rules 1, 2, 4 are applied (transients removed,
/// children normalized in order). Rule 3 (collapse) is applied at serialization.
class _NormNode {
  _NormNode(this.role, this.type, this.icon, this.id, this.children);
  final String role;
  final String? type;
  final String? icon;
  final String? id;
  final List<_NormNode> children;
}

/// Apply rules 1, 2, 4: exclude text (no text field exists), drop transient
/// subtrees, keep document order. Returns null if this node itself is transient.
_NormNode? _normalize(RNode node) {
  if (_isTransient(node)) return null;
  final children = <_NormNode>[];
  for (final c in node.children) {
    final nc = _normalize(c);
    if (nc != null) children.add(nc);
  }
  return _NormNode(
    normalizeRole(node.role),
    node.type,
    node.icon,
    node.id,
    children,
  );
}

/// One node's token body (everything after `<depth>:`), without the repeat
/// marker: `<role>[:<type>][#<icon>][@<id>]`.
String _tokenBody(_NormNode n) {
  final sb = StringBuffer(n.role);
  if (n.type != null) {
    sb.write(':');
    sb.write(n.type);
  }
  if (n.icon != null) {
    sb.write('#');
    sb.write(n.icon);
  }
  if (n.id != null) {
    sb.write('@');
    sb.write(n.id);
  }
  return sb.toString();
}

/// The canonical subtree descriptor used for collapse comparison (rule 3): the
/// pre-order token list of this subtree, depths re-based to 0, so two sibling
/// subtrees at the same level compare equal regardless of absolute depth.
String _subtreeKey(_NormNode n) {
  final tokens = <String>[];
  _walkKey(n, 0, tokens);
  return tokens.join(';');
}

void _walkKey(_NormNode n, int depth, List<String> tokens) {
  tokens.add('$depth:${_tokenBody(n)}');
  for (final c in n.children) {
    _walkKey(c, depth + 1, tokens);
  }
}

/// Emit one node's token (optionally marked repeated) then recurse, collapsing
/// across the children run.
void _serializeNode(
    _NormNode n, int depth, bool repeated, List<String> tokens) {
  var tok = '$depth:${_tokenBody(n)}';
  if (repeated) tok += '*';
  tokens.add(tok);
  _serializeChildren(n.children, depth + 1, tokens);
}

/// Walk a run of siblings, collapsing maximal runs of >= 2 consecutive children
/// whose subtreeKey is identical into a single emission with the `*` marker.
void _serializeChildren(
    List<_NormNode> children, int depth, List<String> tokens) {
  var i = 0;
  while (i < children.length) {
    final key = _subtreeKey(children[i]);
    var j = i + 1;
    while (j < children.length && _subtreeKey(children[j]) == key) {
      j++;
    }
    final run = j - i;
    _serializeNode(children[i], depth, run >= 2, tokens);
    i = j;
  }
}

// ---------------------------------------------------------------------------
// Layer 2: bounded, locale-safe value-classes (docs/signature.md "Value-state").
// ---------------------------------------------------------------------------

/// True if [n] carries a canonical value-class in the `V:` section: it has a
/// [RNode.value] AND it is value-bearing, i.e. its RAW role is a value-role OR it
/// is [RNode.valueNode]-flagged. The raw role is used deliberately (roles like
/// `status`/`meter` normalize to `node` but are still value-roles).
bool _isValueBearing(RNode n) =>
    n.value != null && (kValueRoles.contains(n.role) || n.valueNode);

/// Strict `^[+-]?[0-9]+(\.[0-9]+)?$`: an optional sign, one or more ASCII digits,
/// optionally a period followed by one or more ASCII digits. No grouping
/// separators, no exponent, no leading/trailing dot. Locale-safe by construction.
bool _isStrictDecimal(String s) {
  final u = s.codeUnits;
  var i = 0;
  if (i < u.length && (u[i] == 0x2b || u[i] == 0x2d)) i++; // + or -
  final intStart = i;
  while (i < u.length && u[i] >= 0x30 && u[i] <= 0x39) {
    i++;
  }
  if (i == intStart) return false; // need at least one integer digit
  if (i < u.length && u[i] == 0x2e) {
    // '.'
    i++;
    final fracStart = i;
    while (i < u.length && u[i] >= 0x30 && u[i] <= 0x39) {
      i++;
    }
    if (i == fracStart) return false; // trailing dot with no fraction
  }
  return i == u.length;
}

/// Map a value string to a bounded, deterministic, locale-safe value-class token
/// (docs/signature.md "Value-state"). EMPTY / ZERO / NEG / POS1<10 / POS2<100 /
/// POS3<1000 / POSL>=1000 for the strict period-decimal grammar; NONEMPTY for
/// anything ambiguous (grouped/locale numbers, currency, text) because we do not
/// guess locale formats.
String valueClass(String s) {
  final t = s.trim();
  if (t.isEmpty) return 'EMPTY';
  if (_isStrictDecimal(t)) {
    final n = double.parse(t);
    final a = n.abs();
    if (n == 0.0) return 'ZERO';
    if (n < 0.0) return 'NEG';
    if (a < 10.0) return 'POS1';
    if (a < 100.0) return 'POS2';
    if (a < 1000.0) return 'POS3';
    return 'POSL';
  }
  return 'NONEMPTY';
}

/// The `V:`-section key for a value-bearing node: its stable `id` as `key:<id>`
/// if present, otherwise the structural fallback `role:<role>#<idx>` using the
/// NORMALIZED role (so the key namespace matches the selector grammar). This is
/// the "stable-key" the `V:` section sorts on.
String _valueKey(RNode n, int structuralIndex) =>
    n.id != null ? 'key:${n.id}' : 'role:${normalizeRole(n.role)}#$structuralIndex';

/// Collect `(value_key, value_class)` pairs for every value-bearing node in the
/// tree, in pre-order, skipping transient subtrees (rule 2) so the `V:` section
/// stays consistent with the structural body. The structural index for a keyless
/// node is its position among same-(normalized-)role, non-transient siblings
/// under the same parent. The root has no peers, so it gets index 0. The result
/// is later sorted by key for deterministic serialization.
List<MapEntry<String, String>> valuePairs(RNode root) {
  final out = <MapEntry<String, String>>[];
  void children(RNode node) {
    final roleCounts = <String, int>{};
    for (final c in node.children) {
      if (_isTransient(c)) continue;
      final role = normalizeRole(c.role);
      final idx = roleCounts[role] ?? 0;
      roleCounts[role] = idx + 1;
      if (_isValueBearing(c)) {
        out.add(MapEntry(_valueKey(c, idx), valueClass(c.value!)));
      }
      children(c);
    }
  }

  if (_isTransient(root)) return out;
  if (_isValueBearing(root)) {
    out.add(MapEntry(_valueKey(root, 0), valueClass(root.value!)));
  }
  children(root);
  // Sort by UTF-8 BYTE order to match the Rust oracle's `String::cmp`. Dart's
  // `String.compareTo` uses UTF-16 code-unit order, which DIVERGES for astral
  // chars (surrogate pairs sort below high-BMP chars). Code-point order (runes)
  // == UTF-8 byte order, so compare rune-by-rune.
  out.sort((a, b) => _compareUtf8(a.key, b.key));
  return out;
}

/// The `V:` section suffix (docs/signature.md "Value-state"). Empty string when
/// there are NO value-bearing pairs, which keeps the descriptor (and hash)
/// byte-identical to a pre-value-state tree. [excludeKeys] lets a RUNNER enforce
/// the per-node cap (Layer 2 "Hard cap") by dropping keys that exceeded their
/// distinct-value-class budget; the SDK passes none.
String _valueSection(List<MapEntry<String, String>> pairs, Set<String>? excludeKeys) {
  final kept = (excludeKeys == null || excludeKeys.isEmpty)
      ? pairs
      : pairs.where((e) => !excludeKeys.contains(e.key)).toList();
  if (kept.isEmpty) return '';
  return '\nV:${kept.map((e) => '${e.key}=${e.value}').join(';')}';
}

/// Build the exact UTF-8 descriptor string that gets hashed (docs/signature.md
/// "Descriptor serialization"): `"A:" + anchor + "\n" + tokens.join(";")`, with
/// the Layer 2 `V:` section appended only when at least one value-bearing node
/// exists. [excludeKeys] drops capped value-keys from the `V:` section (runner
/// cap; the SDK leaves it empty). The `A:` prefix line is always present.
String descriptorFrom(String? anchor, RNode root, Set<String>? excludeKeys) {
  final tokens = <String>[];
  final norm = _normalize(root);
  if (norm != null) {
    _serializeNode(norm, 0, false, tokens);
  }
  final v = _valueSection(valuePairs(root), excludeKeys);
  return 'A:${anchor ?? ''}\n${tokens.join(';')}$v';
}

/// Build the exact UTF-8 descriptor string that gets hashed, with the full
/// (uncapped) `V:` section. The `A:` prefix line is always present, even with no
/// anchor. A tree with no value-bearing nodes is byte-identical to a
/// pre-value-state tree (backward-compatible).
String descriptor(String? anchor, RNode root) =>
    descriptorFrom(anchor, root, null);

/// FNV-1a, 32-bit, over the UTF-8 bytes of [bytes]; 8-char zero-padded
/// lowercase hex (docs/signature.md "Hash").
String fnv1a32Hex(List<int> bytes) {
  var h = 0x811c9dc5;
  for (final b in bytes) {
    h ^= b;
    h = (h * 0x01000193) & 0xFFFFFFFF;
  }
  return h.toRadixString(16).padLeft(8, '0');
}

/// FNV-1a 32-bit over the UTF-8 encoding of [s].
String fnv1a32(String s) {
  // Encode as UTF-8 so non-ASCII descriptors hash identically to Rust's
  // byte-oriented FNV. ASCII-only descriptors (the common case) are unaffected.
  return fnv1a32Hex(_utf8(s));
}

/// Compare two strings by their UTF-8 byte sequences (== Rust `String::cmp`).
/// Code-point order equals UTF-8 byte order, so iterating runes is sufficient
/// and avoids materializing the full byte lists.
int _compareUtf8(String a, String b) {
  final ra = a.runes.toList();
  final rb = b.runes.toList();
  final n = ra.length < rb.length ? ra.length : rb.length;
  for (var i = 0; i < n; i++) {
    if (ra[i] != rb[i]) return ra[i] < rb[i] ? -1 : 1;
  }
  return ra.length - rb.length;
}

/// Minimal UTF-8 encoder (avoids a dart:convert import here; identical output).
List<int> _utf8(String s) {
  final out = <int>[];
  for (final cp in s.runes) {
    if (cp < 0x80) {
      out.add(cp);
    } else if (cp < 0x800) {
      out.add(0xC0 | (cp >> 6));
      out.add(0x80 | (cp & 0x3F));
    } else if (cp < 0x10000) {
      out.add(0xE0 | (cp >> 12));
      out.add(0x80 | ((cp >> 6) & 0x3F));
      out.add(0x80 | (cp & 0x3F));
    } else {
      out.add(0xF0 | (cp >> 18));
      out.add(0x80 | ((cp >> 12) & 0x3F));
      out.add(0x80 | ((cp >> 6) & 0x3F));
      out.add(0x80 | (cp & 0x3F));
    }
  }
  return out;
}

/// THE canonical signature: FNV-1a 32-bit over [descriptor], 8 hex chars.
String signature(String? anchor, RNode root) =>
    fnv1a32(descriptor(anchor, root));

/// The canonical signature with capped value-keys excluded from the `V:` section
/// (runner cap, Layer 2 "Hard cap"). With [excludeKeys] empty/null this is
/// identical to [signature].
String signatureFrom(String? anchor, RNode root, Set<String>? excludeKeys) =>
    fnv1a32(descriptorFrom(anchor, root, excludeKeys));

/// A selector that addresses an element for actions / repros (docs/signature.md
/// "Selectors"): `key:<id>` when a stable id exists, else `role:<role>#<idx>`.
/// [nokey] is true when no id was available (metadata for `map --show`; it does
/// NOT affect the hash).
class Selector {
  Selector(this.selector, this.nokey);
  final String selector;
  final bool nokey;
}

/// Build a selector for a node given its structural index among same-role peers.
Selector selectorFor(String? id, String role, int structuralIndex) {
  if (id != null) return Selector('key:$id', false);
  return Selector('role:${normalizeRole(role)}#$structuralIndex', true);
}
