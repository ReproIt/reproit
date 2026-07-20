part of '../reproit_explorer.dart';

// ===========================================================================
// CANONICAL STRUCTURAL SIGNATURE (docs/signature.md; oracle:
// crates/reproit/src/domain/signature.rs). This block is byte-for-byte aligned
// with the Rust oracle and the production SDK (sdk/reproit_flutter), so the
// explorer, the SDK, and the runners all hash the SAME descriptor. Do not edit
// it to "fix" a mismatch: diff the descriptor string against the spec instead.
// ===========================================================================

/// The fixed, language-independent role vocabulary. Unknown roles -> `node`.
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

/// Roles that flicker in/out and are dropped before hashing (rule 2). `progress`
/// is the role name for spinner/progress.
const Set<String> kTransientRoles = <String>{
  'toast',
  'snackbar',
  'spinner',
  'progress',
  'tooltip',
  'badge',
};

/// Value-role set (docs/signature.md "Value-state", Layer 2). A node carries a
/// canonical value-class only if it has a value AND either its RAW role is in
/// this set OR it is `valueNode`-flagged (Layer 3 opt-in). Several of these are
/// NOT structural roles (they normalize to `node`), so the test uses the RAW
/// role. Chrome roles (button/header/text/...) are NEVER value-bearing.
const Set<String> kValueRoles = <String>{
  'textfield',
  'status',
  'log',
  'progressbar',
  'meter',
  'timer',
  'output',
};

String normalizeRole(String role) => kRoles.contains(role) ? role : 'node';

/// A normalized accessibility node: the input to the canonical signature.
/// Mirrors the Rust `Node` shape. The structural body never reads localized text
/// (rule 1); `value`/`valueNode` feed ONLY the Layer 2 `V:` value-class section.
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
  final String role;
  final String? id;
  final String? type;
  final String? icon;
  final bool transient;
  final String? value;
  final bool valueNode;
  final List<RNode> children;
}

/// FNV-1a 32-bit over the UTF-8 bytes of [s], 8-char zero-padded lowercase hex.
/// Used for the canonical descriptor (ASCII in practice) and clipLabel hashes.
String fnv1a(String s) {
  var h = 0x811c9dc5;
  for (final b in utf8.encode(s)) {
    h ^= b;
    h = (h * 0x01000193) & 0xFFFFFFFF;
  }
  return h.toRadixString(16).padLeft(8, '0');
}

bool _isTransient(RNode n) => n.transient || kTransientRoles.contains(n.role);

class _NormNode {
  _NormNode(this.role, this.type, this.icon, this.id, this.children);
  final String role;
  final String? type;
  final String? icon;
  final String? id;
  final List<_NormNode> children;
}

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

String _tokenBody(_NormNode n) {
  final sb = StringBuffer(n.role);
  if (n.type != null) sb.write(':${n.type}');
  if (n.icon != null) sb.write('#${n.icon}');
  if (n.id != null) sb.write('@${n.id}');
  return sb.toString();
}

String _subtreeKey(_NormNode n) {
  final tokens = <String>[];
  void walk(_NormNode m, int depth) {
    tokens.add('$depth:${_tokenBody(m)}');
    for (final c in m.children) {
      walk(c, depth + 1);
    }
  }

  walk(n, 0);
  return tokens.join(';');
}

void _serializeNode(
  _NormNode n,
  int depth,
  bool repeated,
  List<String> tokens,
) {
  var tok = '$depth:${_tokenBody(n)}';
  if (repeated) tok += '*';
  tokens.add(tok);
  _serializeChildren(n.children, depth + 1, tokens);
}

void _serializeChildren(
  List<_NormNode> children,
  int depth,
  List<String> tokens,
) {
  var i = 0;
  while (i < children.length) {
    final key = _subtreeKey(children[i]);
    var j = i + 1;
    while (j < children.length && _subtreeKey(children[j]) == key) {
      j++;
    }
    _serializeNode(children[i], depth, (j - i) >= 2, tokens);
    i = j;
  }
}

// --- Layer 2: bounded, locale-safe value-classes (docs/signature.md). --------

/// True if [n] carries a value-class in the `V:` section: it has a value AND its
/// RAW role is a value-role OR it is `valueNode`-flagged.
bool _isValueBearing(RNode n) =>
    n.value != null && (kValueRoles.contains(n.role) || n.valueNode);

/// Strict `^[+-]?[0-9]+(\.[0-9]+)?$`: optional sign, >=1 ASCII digits, optional
/// period + >=1 ASCII digits. No grouping, no exponent, no leading/trailing dot.
bool _isStrictDecimal(String s) {
  final u = s.codeUnits;
  var i = 0;
  if (i < u.length && (u[i] == 0x2b || u[i] == 0x2d)) i++;
  final intStart = i;
  while (i < u.length && u[i] >= 0x30 && u[i] <= 0x39) {
    i++;
  }
  if (i == intStart) return false;
  if (i < u.length && u[i] == 0x2e) {
    i++;
    final fracStart = i;
    while (i < u.length && u[i] >= 0x30 && u[i] <= 0x39) {
      i++;
    }
    if (i == fracStart) return false;
  }
  return i == u.length;
}

/// Bounded, deterministic, locale-safe value-class token (docs/signature.md).
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

/// The stable `V:`-section key: `key:<id>` if keyed, else `role:<role>#<idx>`
/// (NORMALIZED role, structural index among same-role non-transient siblings).
String _valueKey(RNode n, int idx) =>
    n.id != null ? 'key:${n.id}' : 'role:${normalizeRole(n.role)}#$idx';

/// `(value_key, value_class)` for every value-bearing node, pre-order, skipping
/// transient subtrees, sorted by key (deterministic). The structural index for a
/// keyless node is its position among same-(normalized-)role non-transient
/// siblings; the root gets index 0.
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
  out.sort((a, b) => a.key.compareTo(b.key));
  return out;
}

/// `\nV:` + `key=class;...` for the kept pairs, or '' if none. [excludeKeys]
/// drops keys the runner capped (Layer 2 "Hard cap").
String _valueSection(
  List<MapEntry<String, String>> pairs,
  Set<String>? excludeKeys,
) {
  final kept = (excludeKeys == null || excludeKeys.isEmpty)
      ? pairs
      : pairs.where((e) => !excludeKeys.contains(e.key)).toList();
  if (kept.isEmpty) return '';
  return '\nV:${kept.map((e) => '${e.key}=${e.value}').join(';')}';
}

/// `"A:" + anchor + "\n" + tokens.join(";")` + the Layer 2 `V:` section (only
/// when a value-bearing node exists). [excludeKeys] drops capped value-keys.
String descriptorFrom(String? anchor, RNode root, Set<String>? excludeKeys) {
  final tokens = <String>[];
  final norm = _normalize(root);
  if (norm != null) _serializeNode(norm, 0, false, tokens);
  final v = _valueSection(valuePairs(root), excludeKeys);
  return 'A:${anchor ?? ''}\n${tokens.join(';')}$v';
}

/// `"A:" + anchor + "\n" + tokens.join(";")` with the full `V:` section. The A:
/// line is always present; a value-less tree is byte-identical to before Layer 2.
String descriptor(String? anchor, RNode root) =>
    descriptorFrom(anchor, root, null);

/// The canonical signature: FNV-1a 32-bit over the descriptor, 8 hex chars.
String signature(String? anchor, RNode root) => fnv1a(descriptor(anchor, root));

/// The canonical signature with capped value-keys excluded (runner cap).
String signatureFrom(String? anchor, RNode root, Set<String>? excludeKeys) =>
    fnv1a(descriptorFrom(anchor, root, excludeKeys));

/// STRUCTURAL-ONLY signature: the canonical signature with the ENTIRE value-state
/// (V:) section excluded, so a legitimately changing value (a clock, a counter)
/// never trips a metamorphic comparison. Used by the lifecycle-metamorphic
/// oracles (rotation, background-restore), whose relation must hold across
/// value-state drift.
String structuralSignature(String? anchor, RNode root) =>
    signatureFrom(anchor, root, valuePairs(root).map((e) => e.key).toSet());
