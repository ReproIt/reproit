part of 'operability_fixture_test.dart';


const int actionBudget = 36;
const int maxLabelLen = 40;
const int maxLabelsPerState = 24;

/// Fuzz config: a HOST file path baked in as one constant define, so one
/// build serves every seed and replay (warm runs). Identical schema to
/// Flutter scaffold: {seed,budget,edgeWeights,prefix,replay,batch}.
const String fuzzConfigPath = String.fromEnvironment('REPROIT_FUZZ_CONFIG');

/// The desired UI locale for the whole run, as a BCP47 tag (e.g. "de", "ar",
/// "pt-BR"), baked in via `--dart-define=REPROIT_LOCALE=de`. When empty the app
/// renders in its own default locale (today's behavior). When set, the app under
/// test is forced into this locale before it first renders, so reproit can fuzz
/// the app in a chosen language. It is the SESSION default; a per-seed
/// `fuzz.locale` (config) still overrides it for that seed. Crucially the locale
/// only changes visible LABELS, never the structural signature (which excludes
/// text by construction).
const String envLocale = String.fromEnvironment('REPROIT_LOCALE');

/// Multi-actor conductor URL, baked via `--dart-define=REPROIT_SCENARIO_BARRIER`.
/// When set, this device plays ONE actor of an authored scenario: it claims a
/// distinct role from the conductor, then pulls its next action on its turn and
/// reports done, instead of fuzzing. Empty for ordinary single-device runs.
const String envBarrier = String.fromEnvironment('REPROIT_SCENARIO_BARRIER');

/// Parse a BCP47 string like "de", "pt-BR", or "zh_Hant_TW" into a Flutter
/// [Locale]. Splits on '-' or '_'; uses the first subtag as the language and a
/// 2-letter UPPERCASE subtag as the country (script/other subtags are ignored,
/// which is enough to drive MaterialApp's locale resolution). Returns null for
/// an empty/blank tag so callers leave the app's default locale untouched.
Locale? parseLocale(String tag) {
  final t = tag.trim();
  if (t.isEmpty) return null;
  final parts = t.split(RegExp('[-_]'));
  final lang = parts.first.toLowerCase();
  if (lang.isEmpty) return null;
  String? country;
  for (final p in parts.skip(1)) {
    if (p.length == 2 && RegExp(r'^[A-Za-z]{2}$').hasMatch(p)) {
      country = p.toUpperCase();
      break;
    }
  }
  return Locale(lang, country);
}

/// Force the app under test into [tag] (BCP47) via the test locale override, so
/// the app renders in that language. Set on the binding's platformDispatcher so
/// MaterialApp/CupertinoApp locale resolution picks it up on the next build.
/// No-op for an empty/unparseable tag.
void applyLocale(WidgetTester t, String tag) {
  final loc = parseLocale(tag);
  if (loc == null) return;
  t.binding.platformDispatcher.localeTestValue = loc;
  t.binding.platformDispatcher.localesTestValue = <Locale>[loc];
}

/// Clear any test locale override so it is scoped to this run and does not leak
/// into a later test in the same process.
void clearLocale(WidgetTester t) {
  try {
    t.binding.platformDispatcher.clearLocaleTestValue();
    t.binding.platformDispatcher.clearLocalesTestValue();
  } catch (_) {}
}

class FuzzCfg {
  FuzzCfg({
    this.seed = 0,
    this.budget = actionBudget,
    this.replay,
    this.prefix,
    this.edgeWeights = const {},
    this.inputs = const [],
    this.locale,
  });
  final int seed;
  final int budget;
  final List<String>? replay;

  /// Property-matched replay (tier 3): synthesized, deterministic field values
  /// to type into matching text fields so a data-specific bug (a long unicode
  /// name, an emoji, an empty/RTL field) reproduces. Each entry is
  /// {field, value}; `field` matches an a11y label or a positional "#<n>" index.
  final List<Map<String, String>> inputs;

  /// Best-effort locale to drive (e.g. "tr"), so locale-folding bugs reproduce.
  final String? locale;

  /// Frontier prefix: executed replay-style BEFORE the seeded walk, so the
  /// randomness is spent at the frontier instead of on getting there.
  final List<String>? prefix;

  /// edgeWeights[fromSig][action] = global traversal count. The seeded pick
  /// weights each candidate edge by 1/(1+count): inverse-visit-count action
  /// scoring. A fixed snapshot, so replays stay deterministic.
  final Map<String, Map<String, int>> edgeWeights;

  static FuzzCfg fromJson(Map<String, dynamic> j) {
    final ewRaw = (j['edgeWeights'] as Map?) ?? {};
    final ew = <String, Map<String, int>>{};
    ewRaw.forEach((sig, m) {
      ew[sig as String] = ((m as Map?) ?? {}).map(
        (k, v) => MapEntry(k as String, (v as num).toInt()),
      );
    });
    final inputs = ((j['inputs'] as List?) ?? const [])
        .map(
          (e) => (e as Map).map(
            (k, v) => MapEntry(k.toString(), v?.toString() ?? ''),
          ),
        )
        .toList();
    return FuzzCfg(
      seed: (j['seed'] as num?)?.toInt() ?? 0,
      budget: (j['budget'] as num?)?.toInt() ?? actionBudget,
      replay: (j['replay'] as List?)?.cast<String>(),
      prefix: (j['prefix'] as List?)?.cast<String>(),
      edgeWeights: ew,
      inputs: inputs,
      locale: j['locale'] as String?,
    );
  }

  /// The list of per-seed configs to run in this session: a single-element list
  /// for {"seed":..}/{"replay":..}, or the explicit list for {"batch":[...]}.
  /// Returns one default config if nothing is set.
  static List<FuzzCfg> loadBatch() {
    if (fuzzConfigPath.isEmpty) return [FuzzCfg()];
    try {
      final raw = File(fuzzConfigPath).readAsStringSync();
      final j = jsonDecode(raw) as Map<String, dynamic>;
      final batch = j['batch'] as List?;
      if (batch != null && batch.isNotEmpty) {
        return batch
            .map((e) => FuzzCfg.fromJson((e as Map).cast<String, dynamic>()))
            .toList();
      }
      return [FuzzCfg.fromJson(j)];
    } catch (_) {
      return [FuzzCfg()];
    }
  }
}

/// Layer 3 opt-in value selectors (docs/signature.md "Value-state"). A
/// `reproit.yaml` may carry a `value_nodes:` list of selectors (`key:<id>` or
/// `role:<role>#<idx>`); nodes matching one are treated as value-bearing even
/// when their role is not a value-role. Read once from the host `reproit.yaml`
/// (headless runs in-process from the repo, so the file is readable); a
/// `--dart-define=REPROIT_VALUE_NODES=key:score,role:text#2` override is also
/// honored.
const String envValueNodes = String.fromEnvironment('REPROIT_VALUE_NODES');

/// Parse the `value_nodes:` selector list from `reproit.yaml` plus the
/// REPROIT_VALUE_NODES dart-define, into a deduped set of selectors. Minimal,
/// dependency-free: it reads the `value_nodes:` block as a YAML list of scalars
/// (`- key:score`) or an inline `[key:score, role:text#2]`. Anything it cannot
/// parse is ignored (best-effort; never breaks exploration).
Set<String> loadValueNodeSelectors() {
  final out = <String>{};
  for (final s in envValueNodes.split(',')) {
    final t = s.trim();
    if (t.isNotEmpty) out.add(t);
  }
  try {
    final f = File('reproit.yaml');
    if (f.existsSync()) {
      final lines = f.readAsLinesSync();
      var inBlock = false;
      for (final raw in lines) {
        final line = raw.replaceAll('\t', '  ');
        final trimmed = line.trim();
        if (trimmed.isEmpty || trimmed.startsWith('#')) continue;
        final keyMatch = RegExp(r'^value_nodes\s*:(.*)$').firstMatch(trimmed);
        if (keyMatch != null) {
          final rest = keyMatch.group(1)!.trim();
          if (rest.startsWith('[')) {
            // Inline list: value_nodes: [key:score, role:text#2]
            for (final item
                in rest.replaceAll(RegExp(r'[\[\]]'), '').split(',')) {
              final v = _unquote(item.trim());
              if (v.isNotEmpty) out.add(v);
            }
            inBlock = false;
          } else {
            inBlock = true; // block list follows on indented `- ` lines
          }
          continue;
        }
        if (inBlock) {
          if (trimmed.startsWith('- ')) {
            final v = _unquote(trimmed.substring(2).trim());
            if (v.isNotEmpty) out.add(v);
          } else if (!line.startsWith(' ')) {
            // A new top-level key ends the value_nodes block.
            inBlock = false;
          }
        }
      }
    }
  } catch (_) {}
  return out;
}

String _unquote(String s) {
  if (s.length >= 2 &&
      ((s.startsWith('"') && s.endsWith('"')) ||
          (s.startsWith("'") && s.endsWith("'")))) {
    return s.substring(1, s.length - 1);
  }
  return s;
}

/// xorshift32: deterministic across runs for the same seed.
class Rng {
  Rng(int seed) : _s = seed == 0 ? 1 : seed & 0xFFFFFFFF;
  int _s;
  int next(int n) {
    _s ^= (_s << 13) & 0xFFFFFFFF;
    _s ^= _s >> 17;
    _s ^= (_s << 5) & 0xFFFFFFFF;
    return (_s & 0x7FFFFFFF) % n;
  }
}

// ===========================================================================
// CANONICAL STRUCTURAL SIGNATURE (docs/signature.md; oracle:
// crates/reproit/src/model/signature.rs). Byte-for-byte aligned with the Rust
// oracle, the simulator explorer, and the production SDK (sdk/reproit_flutter),
// so headless sigs match sim sigs match prod sigs. Do not edit it to "fix" a
// mismatch: diff the descriptor string against the spec instead.
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

/// Map a Flutter [SemanticsData] to the canonical Role vocabulary from
/// flags/actions only, NEVER from the (localized) label. A password is a
/// `textfield` with `type=password` (a TYPE refinement, not a role).
String roleOf(SemanticsData data) {
  final flags = data.flagsCollection;
  if (flags.isTextField) return 'textfield';
  if (flags.isToggled != Tristate.none) return 'switch';
  if (flags.isChecked != CheckedState.none) {
    return flags.isInMutuallyExclusiveGroup ? 'radio' : 'checkbox';
  }
  if (flags.isSlider) return 'slider';
  if (flags.isHeader) return 'header';
  if (flags.isLink) return 'link';
  if (flags.isButton) return 'button';
  if (flags.isImage) return 'image';
  if (data.hasAction(SemanticsAction.tap)) return 'button';
  return 'node';
}

/// The optional input-`type` refinement for a textfield node, from flags only.
String? inputTypeOf(SemanticsData data, String role) {
  if (role != 'textfield') return null;
  return data.flagsCollection.isObscured ? 'password' : 'text';
}

/// The displayed VALUE of a value-bearing semantics node (Layer 2), or null.
/// Detected from flags only: a text field's entered text (`d.value`), a slider's
/// value (`d.value`), and a live region (aria-live's Flutter equivalent: its
/// `d.value` if set, else `d.label`, treated as a status value-role). Chrome
/// roles return null so rule 1's chrome-text exclusion is preserved.
String? valueOf(SemanticsData data) {
  if (data.flagsCollection.isTextField) return data.value;
  if (data.flagsCollection.isSlider) return data.value;
  if (data.flagsCollection.isLiveRegion) {
    return data.value.trim().isNotEmpty ? data.value : data.label;
  }
  return null;
}

/// True when a value-bearing node needs the Layer 3 `valueNode` flag because its
/// structural role is NOT a value-role: a slider (role `slider`) and a live
/// region (often `node`/`text`/`button`). A text field's role IS a value-role,
/// so it needs no flag.
bool valueNodeFlagOf(SemanticsData data) =>
    !data.flagsCollection.isTextField &&
    (data.flagsCollection.isSlider || data.flagsCollection.isLiveRegion);

/// The screen anchor (route template / screen-level key). Captured from the top
/// route's name; a ReproItScreen marker or screen-level Key would also feed here
/// if present. Null/empty leaves the anchor empty (the A: line is still emitted).
String? screenAnchor(WidgetTester t) {
  try {
    String? name;
    final nav = t.state<NavigatorState>(find.byType(Navigator).first);
    nav.popUntil((r) {
      name ??= r.settings.name;
      return true;
    });
    if (name != null && name!.isNotEmpty) return name;
  } catch (_) {}
  return null;
}

/// A stable developer key string for an element's widget, or null. ONLY
/// LocalKeys with a deterministic value are accepted: `ValueKey<T>` and the
/// `Key('x')` factory (which is a `ValueKey<String>`). UniqueKey and GlobalKey
/// are rejected because they are allocated fresh per build (non-deterministic,
/// so useless as a stable anchor). The returned string round-trips through
/// `ValueKey<String|int>(...)` for find.byKey-based replay.
String? keyStringOf(Widget w) {
  final k = w.key;
  if (k is ValueKey<String>) return 's:${k.value}';
  if (k is ValueKey<int>) return 'i:${k.value}';
  if (k is ValueKey) return 'v:${k.value}';
  return null;
}

/// The raw developer-id VALUE from a keyString (strips the `s:`/`i:`/`v:` type
/// prefix). This is what enters the canonical descriptor as `@<id>`, matching
/// how the oracle/SDK treat a Key's value as the stable id. The prefixed
/// keyString is still used for `key:<keyString>` SELECTORS (replay).
String keyValueOf(String ks) {
  if (ks.startsWith('s:') || ks.startsWith('i:') || ks.startsWith('v:')) {
    return ks.substring(2);
  }
  return ks;
}

/// Rebuild a Finder-usable Key from a keyString produced by keyStringOf, for
/// the typed cases we can reconstruct exactly. String/int round-trip; anything
/// else falls back to a string ValueKey on the rendered value (best effort).
Key keyFromString(String ks) {
  if (ks.startsWith('s:')) return ValueKey<String>(ks.substring(2));
  if (ks.startsWith('i:')) {
    return ValueKey<int>(int.tryParse(ks.substring(2)) ?? 0);
  }
  return ValueKey<String>(ks.startsWith('v:') ? ks.substring(2) : ks);
}

/// True when [w] is the root of a subtree that is NOT on the current visible
/// screen, so its keyed elements must be pruned from the collection walk.
///
/// Why this matters: when a screen is reached via Navigator.push, the route(s)
/// underneath stay MOUNTED in the element tree but are taken OFFSTAGE by the
/// framework (a `ModalRoute` whose `offstage` is true is wrapped in an
/// `Offstage(offstage: true)`, and inactive route subtrees also have their
/// `TickerMode` disabled). The semantics walk in `snapshot()` already drops
/// these (their nodes carry `SemanticsFlag.isHidden`), so the visible tappables
/// list only holds onstage nodes. The key collection therefore has to match:
/// if it kept walking offstage routes it would return their keys in document
/// order and the index-based pairing would bind the visible (pushed-route)
/// tappables to the wrong, offstage keys. Pruning here keeps the two lists in
/// lock-step so keyed elements on a pushed route are addressable.
///
/// Detection uses only public, locale-invariant widget signals:
///   * `Offstage(offstage: true)` - inactive ModalRoute / explicitly offstage,
///   * `TickerMode(enabled: false)` - inactive route subtree (animations off),
///   * `Visibility(visible: false)` that does not maintain interactivity.
bool _isOffstageSubtree(Widget w) {
  if (w is Offstage) return w.offstage;
  if (w is TickerMode) return !w.enabled;
  if (w is Visibility) return !w.visible && !w.maintainInteractivity;
  return false;
}

/// Collect every stable developer key present in the live element tree, in
/// document order, as keyString values. Walking the ELEMENT tree (not the
/// semantics tree) is required: developer keys live on Widgets, not on
/// SemanticsData. Order-stable and locale-invariant. Offstage subtrees (routes
/// pushed under the current one) are pruned so the result reflects only the
/// CURRENT visible screen, matching the onstage semantics walk in snapshot().
List<String> collectKeys() {
  final keys = <String>[];
  void walk(Element e) {
    if (_isOffstageSubtree(e.widget)) return;
    final ks = keyStringOf(e.widget);
    if (ks != null) keys.add(ks);
    e.visitChildren(walk);
  }

  final root = WidgetsBinding.instance.rootElement;
  if (root != null) root.visitChildren(walk);
  return keys;
}

/// Crude locale-invariant role of an element, by widget runtime type, used ONLY
/// to pair a keyed element with a tappable semantics node of the same role.
/// Type names are stable and language-independent. Returns null for elements
/// that aren't a recognizable interactive control.
String? elementRole(Widget w) {
  final t = w.runtimeType.toString();
  if (t.contains('EditableText') ||
      t.contains('TextField') ||
      t.contains('TextFormField') ||
      t.contains('CupertinoTextField')) {
    return 'textfield';
  }
  if (t.contains('Switch')) return 'switch';
  if (t.contains('Radio')) return 'radio';
  if (t.contains('Checkbox')) return 'checkbox';
  if (t.contains('Slider')) return 'slider';
  if (t.contains('Button') || t.contains('Chip') || t.contains('Tab')) {
    return 'button';
  }
  if (t.contains('InkWell') ||
      t.contains('GestureDetector') ||
      t.contains('InkResponse') ||
      t.contains('ListTile')) {
    // Generic tappables map to the canonical `button` role (matches roleOf).
    return 'button';
  }
  if (t.contains('Image')) return 'image';
  return null;
}

/// Keyed interactive elements ON THE CURRENT SCREEN, in document order:
/// (keyString, role). Lets a tappable semantics node be addressed by its
/// developer key when one exists. Offstage subtrees (e.g. the Home/List routes
/// that stay mounted underneath a pushed Detail route) are pruned via
/// [_isOffstageSubtree], so this list lines up index-for-index with the onstage
/// tappables collected from the semantics tree in snapshot(). Without the prune,
/// the index pairing would bind a pushed route's visible tappables to the wrong,
/// offstage keys and the real keys (e.g. detail_danger) would never be emitted.
List<MapEntry<String, String>> collectKeyedTappables() {
  final out = <MapEntry<String, String>>[];
  void walk(Element e) {
    if (_isOffstageSubtree(e.widget)) return;
    final ks = keyStringOf(e.widget);
    final role = elementRole(e.widget);
    if (ks != null && role != null) out.add(MapEntry(ks, role));
    e.visitChildren(walk);
  }

  final root = WidgetsBinding.instance.rootElement;
  if (root != null) root.visitChildren(walk);
  return out;
}

SemanticsNode? rootSemanticsNode(WidgetTester tester) {
  for (final renderView in RendererBinding.instance.renderViews) {
    if (renderView.flutterView.viewId == tester.view.viewId) {
      return renderView.owner?.semanticsOwner?.rootSemanticsNode;
    }
  }
  return null;
}

/// Clip a label to the cap WITHOUT dropping its element. A label <= cap is
/// returned unchanged (signatures stay byte-identical for short labels). A
/// longer label is truncated to (cap - 9) code units + '#' + an 8-hex FNV-1a
/// hash of the FULL label, so long-named widgets stay in the snapshot and stay
/// tappable, distinct long labels keep distinct keys, and the result is
/// deterministic. findTappable() resolves the key via its stable prefix.
String clipLabel(String label) {
  if (label.length <= maxLabelLen) return label;
  final suffix = '#${fnv1a(label)}';
  return label.substring(0, maxLabelLen - suffix.length) + suffix;
}

void visit(SemanticsNode node, void Function(SemanticsData) f) {
  final data = node.getSemanticsData();
  f(data);
  node.visitChildren((child) {
    visit(child, f);
    return true;
  });
}

/// A tappable element addressed STRUCTURALLY, never by localized text.
///   sel    canonical, locale-invariant selector for replay:
///            `key:<keyString>`   when the element has a stable developer key
///            `role:<role>#<idx>` otherwise (role + per-role structural index)
///   role   the locale-invariant role token (button, link, tappable, ...)
///   index  per-role structural index (document order among same-role taps)
///   key    the keyString if present, else null
///   label  the visible (localized) text, DISPLAY-ONLY: shown in map --show,
///          never folded into the signature or into `sel`.
