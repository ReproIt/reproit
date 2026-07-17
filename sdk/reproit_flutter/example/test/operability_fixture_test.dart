// ReproIt HEADLESS explorer: the same seeded walk as Flutter scaffold,
// but run under `flutter test` (WidgetTester drives the REAL app in-process)
// instead of `flutter drive` on a simulator. No iOS simulator, no Xcode, no
// VM service: the whole fuzz/exploration tier runs in well under a second on
// any machine, Linux included. This is the cheap, fast tier; the simulator
// tier (Flutter scaffold) is reserved for oracles that need the live
// runtime.
//
// Vendor into your repo as test/fuzz_headless_test.dart and adapt the two
// APP-SPECIFIC lines (import + pumpWidget). Run via: reproit fuzz (default).
//
// It emits the EXACT SAME marker lines the simulator explorer does, so the
// Rust parser (model/map.rs parse_run/absorb_run, modes/fuzz.rs trace/excepts)
// is unchanged:
//   EXPLORE:STATE {"sig":..,"labels":[..],"elements":[{sel,role,label,nokey?}],
//                  "texts":[...]}
//   EXPLORE:EDGE  {"from":..,"action":"tap:<selector>"|"back","to":..}
//   FUZZ:ACT <action>            SEED:BEGIN <seed> / SEED:END <seed>
//   ══╡ EXCEPTION CAUGHT BY ... ╞══ ... ════  (app exception blocks)
// The signature is STRUCTURAL + locale-invariant (FNV-1a over the semantics
// tree shape: depth + role per node + sorted developer keys, NO localized
// text). Selectors are "key:<k>" or "role:<role>#<idx>", never visible text.
// Byte-identical to Flutter scaffold so headless sigs match sim sigs.
//
// ---- ORACLE SCOPE (be honest about it) -------------------------------------
//   WORKS HEADLESS:
//     - app exceptions / assertions thrown during the walk (the primary oracle)
//     - leaked-resource teardown asserts (e.g. an undisposed AnimationController
//       surfaces when the widget tree is re-pumped/torn down between seeds)
//     - state-graph / invariant oracles
//       semantics counts, reachability (all derived from the marker stream)
//   DOES NOT WORK HEADLESS (needs the simulator tier, `reproit fuzz --sim`):
//     - JANK / frame-timing: the test binding uses a FAKE clock, so per-frame
//       build/raster durations are not real. No FRAMES:BATCH is emitted here.
//     - keyboard / IME / viewInsets-overflow bugs: no real on-screen keyboard.
//     - platform-channel / native-plugin behavior: plugins are not present.
//
// Safety: the explorer taps everything reachable, so it must ONLY run against
// dev/staging backends covered by the reset contract.

import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/semantics.dart';
import 'package:flutter_test/flutter_test.dart';

// APP-SPECIFIC: import your app's root widget.
import 'package:reproit_flutter_example/operability_fixture.dart';

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
  bool f(SemanticsFlag x) => data.hasFlag(x);
  if (f(SemanticsFlag.isTextField)) return 'textfield';
  if (f(SemanticsFlag.hasToggledState)) return 'switch';
  if (f(SemanticsFlag.hasCheckedState)) {
    return f(SemanticsFlag.isInMutuallyExclusiveGroup) ? 'radio' : 'checkbox';
  }
  if (f(SemanticsFlag.isSlider)) return 'slider';
  if (f(SemanticsFlag.isHeader)) return 'header';
  if (f(SemanticsFlag.isLink)) return 'link';
  if (f(SemanticsFlag.isButton)) return 'button';
  if (f(SemanticsFlag.isImage)) return 'image';
  if (data.hasAction(SemanticsAction.tap)) return 'button';
  return 'node';
}

/// The optional input-`type` refinement for a textfield node, from flags only.
String? inputTypeOf(SemanticsData data, String role) {
  if (role != 'textfield') return null;
  return data.hasFlag(SemanticsFlag.isObscured) ? 'password' : 'text';
}

/// The displayed VALUE of a value-bearing semantics node (Layer 2), or null.
/// Detected from flags only: a text field's entered text (`d.value`), a slider's
/// value (`d.value`), and a live region (aria-live's Flutter equivalent: its
/// `d.value` if set, else `d.label`, treated as a status value-role). Chrome
/// roles return null so rule 1's chrome-text exclusion is preserved.
String? valueOf(SemanticsData data) {
  if (data.hasFlag(SemanticsFlag.isTextField)) return data.value;
  if (data.hasFlag(SemanticsFlag.isSlider)) return data.value;
  if (data.hasFlag(SemanticsFlag.isLiveRegion)) {
    return data.value.trim().isNotEmpty ? data.value : data.label;
  }
  return null;
}

/// True when a value-bearing node needs the Layer 3 `valueNode` flag because its
/// structural role is NOT a value-role: a slider (role `slider`) and a live
/// region (often `node`/`text`/`button`). A text field's role IS a value-role,
/// so it needs no flag.
bool valueNodeFlagOf(SemanticsData data) =>
    !data.hasFlag(SemanticsFlag.isTextField) &&
    (data.hasFlag(SemanticsFlag.isSlider) ||
        data.hasFlag(SemanticsFlag.isLiveRegion));

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
class Tappable {
  Tappable(this.sel, this.role, this.index, this.key, this.label, this.bounds);
  final String sel;
  final String role;
  final int index;
  final String? key;
  final String label;
  final List<int>? bounds;
  bool get hasKey => key != null;
}

class ScreenText {
  ScreenText(this.text, this.bounds);
  final String text;
  final List<int>? bounds;
}

class Snapshot {
  Snapshot(
    this.tree,
    this.anchor,
    this.sig,
    this.labels,
    this.tappables,
    this.texts,
    this.contentFp,
  );

  /// The captured canonical node tree (screen-rooted). Kept so the explorer can
  /// re-sign it with the Layer 2 per-node value-class CAP applied (capped keys
  /// dropped from the `V:` section). The raw `sig` below is the UNCAPPED canonical
  /// signature; `effectiveSig` re-signs with capped keys excluded.
  final RNode tree;

  /// The screen anchor (route template) that prefixes the signature.
  final String? anchor;

  /// STRUCTURAL + value-state CANONICAL signature: FNV-1a over the canonical
  /// descriptor (anchor prefix + normalized role/type/icon/id tree + the Layer 2
  /// `V:` value-class section). NO localized text contributes to the body. Same
  /// screen in English and German hashes identically; it matches the Rust oracle
  /// and the production SDK.
  final String sig;

  /// DISPLAY-ONLY visible text labels, for map --show human readability. Never
  /// part of the signature.
  final List<String> labels;

  /// Tappable elements, addressed structurally (key, else role+index).
  final List<Tappable> tappables;

  /// Screen text boxes, used only by importers to resolve text-authored tests to
  /// structural selectors.
  final List<ScreenText> texts;

  /// Layer 1 content fingerprint (runner-local, docs/signature.md): the
  /// structural+value signature PLUS sorted (stable-key, trimmed raw text) over
  /// text-bearing nodes. NEVER enters the canonical graph key (it carries raw
  /// localized text). Used only to decide if an action was EFFECTIVE: if the sig
  /// OR this fingerprint changed, something happened; if neither moved, the
  /// action was a no-op. This is what stops the explorer stalling on value-state
  /// screens whose structure never changes.
  final String contentFp;

  /// The canonical signature re-computed with the per-node value-class CAP
  /// applied: any value-key in [cappedKeys] is dropped from the `V:` section so
  /// an adversarial value generator (>8 distinct value-classes for one node)
  /// cannot explode the graph. With no capped keys this equals [sig].
  String effectiveSig(Set<String> cappedKeys) =>
      cappedKeys.isEmpty ? sig : signatureFrom(anchor, tree, cappedKeys);
}

Snapshot snapshot(WidgetTester t) => snapshotWith(t, const <String>{});

/// Build a [Snapshot]. [valueSelectors] is the Layer 3 `value_nodes:` opt-in set
/// (`key:<id>` / `role:<role>#<idx>`): a node matching one is marked
/// value-bearing even when its role is not a value-role.
Snapshot snapshotWith(WidgetTester t, Set<String> valueSelectors) {
  final labels = <String>[];
  final rawTaps = <_TapNode>[]; // tappable nodes in document order
  final texts = <ScreenText>[];
  // (stable-key, trimmed raw text) over text-bearing nodes -> Layer 1 content fp.
  final textParts = <String>[];
  // Developer ids matched to canonical-role nodes in document order. Walking the
  // ELEMENT tree is required because keys live on Widgets, not SemanticsData.
  final keyedIdsByRole = <String, List<String>>{};
  for (final kt in collectKeyedTappables()) {
    (keyedIdsByRole[kt.value] ??= <String>[]).add(keyValueOf(kt.key));
  }
  final perRoleId = <String, int>{};
  // Global per-normalized-role document-order index, for resolving a Layer 3
  // `role:<role>#<idx>` value selector against this screen.
  final perRoleSel = <String, int>{};

  // Build the CANONICAL node tree (roles + types + ids + values), wrapped in a
  // `screen` root. The same walk captures DISPLAY-ONLY labels, the tappables
  // list, and the Layer 1 content fingerprint parts.
  final root = t.binding.pipelineOwner.semanticsOwner?.rootSemanticsNode;
  final rootChildren = <RNode>[];
  if (root != null) {
    RNode? build(SemanticsNode node) {
      final data = node.getSemanticsData();
      if (data.hasFlag(SemanticsFlag.isHidden)) {
        final kids = <RNode>[];
        node.visitChildren((c) {
          final b = build(c);
          if (b != null) kids.add(b);
          return true;
        });
        if (kids.isEmpty) return null;
        return RNode(role: 'group', children: kids);
      }
      final role = roleOf(data);
      final type = inputTypeOf(data, role);
      // Match a developer id by canonical role in document order.
      final idx = perRoleId[role] ?? 0;
      perRoleId[role] = idx + 1;
      final roleIds = keyedIdsByRole[role];
      final id =
          (roleIds != null && idx < roleIds.length) ? roleIds[idx] : null;

      // Layer 2 value-state: a value-role node's displayed value (text field,
      // slider, live region). Layer 3 opt-in: a node matching a `value_nodes:`
      // selector (by id, else by role+structural-index) is forced value-bearing.
      final selIdx = perRoleSel[role] ?? 0;
      perRoleSel[role] = selIdx + 1;
      final matchesSelector =
          (id != null && valueSelectors.contains('key:$id')) ||
              valueSelectors.contains('role:$role#$selIdx');
      var value = valueOf(data);
      var valueNode = value != null && valueNodeFlagOf(data);
      if (matchesSelector) {
        // Force value-bearing via the Layer 3 flag; source a value if the node
        // does not already expose one (best-effort: value, else label).
        value ??= data.value.trim().isNotEmpty ? data.value : data.label;
        valueNode = true;
      }

      // Multiline labels (e.g. "Compose\nTab 2 of 3") normalize to first line.
      final label = data.label.trim().split('\n').first.trim();
      final rect = _globalRect(node);
      final bounds = rect.width > 0 && rect.height > 0
          ? <int>[
              rect.left.round(),
              rect.top.round(),
              rect.width.round(),
              rect.height.round()
            ]
          : null;
      final tappable = data.hasAction(SemanticsAction.tap) &&
          !data.hasFlag(SemanticsFlag.isTextField);
      if (label.isNotEmpty) labels.add(clipLabel(label));
      if (label.isNotEmpty && bounds != null) {
        texts.add(ScreenText(clipLabel(label), bounds));
      }
      if (tappable) rawTaps.add(_TapNode(role, clipLabel(label), bounds));

      // Layer 1: text-bearing parts (stable-key + trimmed raw text). The raw
      // value of a value node and the raw label of a text node both count, so a
      // counter whose display value changes registers as content movement even
      // when structure and value-CLASS are unchanged (e.g. 41 -> 42 stays POS2).
      final stableKey =
          id != null ? 'key:$id' : 'role:${normalizeRole(role)}#$selIdx';
      final rawText = (value ?? '').trim();
      final rawLabel = label;
      if (rawText.isNotEmpty) textParts.add('$stableKey$rawText');
      if (rawLabel.isNotEmpty) textParts.add('$stableKey$rawLabel');

      final kids = <RNode>[];
      node.visitChildren((c) {
        final b = build(c);
        if (b != null) kids.add(b);
        return true;
      });
      return RNode(
        role: role,
        id: id,
        type: type,
        value: value,
        valueNode: valueNode,
        children: kids,
      );
    }

    root.visitChildren((c) {
      final b = build(c);
      if (b != null) rootChildren.add(b);
      return true;
    });
  }

  // CANONICAL signature: descriptor of the screen-rooted tree, prefixed by the
  // screen anchor (route template). Matches crates/reproit/src/model/signature.rs.
  final anchor = screenAnchor(t);
  final tree = RNode(role: 'screen', children: rootChildren);
  final sig = signature(anchor, tree);

  // Layer 1 content fingerprint: structural+value sig + sorted text parts. Raw
  // text is included here ONLY; it never enters `sig` / the canonical graph key.
  final sortedText = textParts.toList()..sort();
  final contentFp = fnv1a('$sig ${sortedText.join(' ')}');

  // Build structural selectors. Each tappable maps to a developer KEY when one
  // exists (preferred: replays in any locale), else falls back to role +
  // per-role structural index. Keyed interactive elements are harvested in
  // document order and paired to semantics tappables of the same role in
  // document order. A tappable with no key keeps role+index and is flagged so
  // the map layer can later warn the developer to add a key.
  final keyedByRole = <String, List<String>>{};
  for (final kt in collectKeyedTappables()) {
    (keyedByRole[kt.value] ??= <String>[]).add(kt.key);
  }
  final tappables = <Tappable>[];
  final perRole = <String, int>{};
  for (final tn in rawTaps) {
    final idx = perRole[tn.role] ?? 0;
    perRole[tn.role] = idx + 1;
    final roleKeys = keyedByRole[tn.role];
    final key =
        (roleKeys != null && idx < roleKeys.length) ? roleKeys[idx] : null;
    final sel = key != null ? 'key:$key' : 'role:${tn.role}#$idx';
    tappables.add(Tappable(sel, tn.role, idx, key, tn.label, tn.bounds));
  }

  final unique = labels.toSet().toList();
  return Snapshot(tree, anchor, sig, unique, tappables, texts, contentFp);
}

/// Internal: a tappable semantics node captured during the structural walk.
class _TapNode {
  _TapNode(this.role, this.label, this.bounds);
  final String role;
  final String label;
  final List<int>? bounds;
}

/// Marker emit. `flutter test` sends stdout straight through, so a plain
/// `print` lands in the drive-a.log the Rust side parses (no `flutter: `
/// prefix, which the parser already tolerates).
void emit(String line) => print(line);

/// Drain the test framework's latched exception (if any) and re-emit it in the
/// SAME block shape the simulator path produces, so modes/fuzz.rs
/// exceptions_in_log parses it unchanged. Returns true if an exception was
/// captured.
///
/// Why takeException instead of FlutterError.onError: `flutter test` LATCHES the
/// first uncaught app exception of a step and would FAIL + abort the test (and
/// thus the rest of the batch) at teardown. takeException() returns that object
/// AND clears the latch, so the walk continues and we control the output. This
/// is exactly how the leaked-AnimationController bug surfaces headless: pumping
/// a fresh/empty tree disposes the offending State and the framework latches the
/// "disposed with an active Ticker" error, which we drain here.
bool drainException(WidgetTester t, {String? phase}) {
  final ex = t.takeException();
  if (ex == null) return false;
  final type = ex.runtimeType.toString();
  // Pull a Dart frame out of the message text (the leak error embeds the
  // creation stack, e.g. `(package:bugzoo/main.dart:210:45)`), so the report
  // points at code even though takeException gives us no separate stack.
  final text = ex.toString();
  final frames = RegExp(
    r'(?:package:|file://)[\w./:-]+\.dart:\d+(?::\d+)?',
  ).allMatches(text).map((m) => m.group(0)!).toSet().take(12).toList();
  emit(
    '══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞'
    '═══════════════════════════════════════',
  );
  emit('The following $type was thrown${phase != null ? ' $phase' : ''}:');
  for (final l in text.split('\n')) {
    if (l.trim().isEmpty) break;
    emit(l);
  }
  emit('');
  for (var i = 0; i < frames.length; i++) {
    emit('#$i      ${frames[i]}');
  }
  emit('════════════════════════════════════════════════════════════════');
  return true;
}

// ===========================================================================
// OPERABILITY / ACCESSIBILITY GROUND-TRUTH (EXPLORE:GROUNDTRUTH).
//
// Two graphs, joined per element:
//   GRAPH 1 (operability): the live WIDGET/ELEMENT tree. An element is operable
//     iff it carries a LIVE interactive affordance (a non-null gesture callback /
//     non-empty recognizer / an actionable control TYPE) AND is hit-testable
//     (its RenderBox has a non-empty size and an on-screen centre).
//   GRAPH 2 (accessibility): the semantics tree (same tree EXPLORE:STATE signs).
//     Each operable element joins to the SMALLEST semantics rect containing its
//     hit-test centre; rolePresent = that node has a real role, namePresent = it
//     carries a label/tooltip/value.
//   KEYBOARD: FocusManager.instance.rootScope traversal order -> inTabOrder /
//     focusable; activation via the framework's default Actions is approximated
//     by "focusable AND in the tab order" (a bare GestureDetector has neither).
//
// Engine rule (reproit): an operable element is an a11y GAP iff
// keyboardActivatable==false OR inTabOrder==false OR rolePresent==false. We only
// emit dims we actually determined; missing dims default true (no gap) on the
// engine side. PUBLIC API only (widget.onTap, e.renderObject, RenderBox,
// FocusManager) so it survives profile/AOT; no WidgetInspector RPCs.
// ===========================================================================

/// An operable widget found in graph 1: a hit-testable element with a live
/// interactive affordance. `element` is the live Element (for focus-ancestry
/// attribution); `point` is its on-screen hit-test centre in SEMANTICS (physical)
/// space, used to join it to a semantics node.
class _Operable {
  _Operable(
      this.gestureKind, this.role, this.keyString, this.element, this.point);
  final String gestureKind;
  final String role;
  final String? keyString;
  final Element element;
  final Offset point;
  FocusNode? focusNode; // attributed from the tab order by render-ancestry.
}

/// The on-screen hit-test centre of [e]'s RenderBox, or null when the element is
/// not laid out, not a box, or has zero area. Public API only (renderObject,
/// RenderBox.hasSize/size/localToGlobal).
Offset? _hitPoint(Element e) {
  final ro = e.renderObject;
  if (ro is! RenderBox) return null;
  if (!ro.hasSize) return null;
  final size = ro.size;
  if (size.isEmpty) return null;
  try {
    return ro.localToGlobal(size.center(Offset.zero));
  } catch (_) {
    return null;
  }
}

/// gestureKind ("tap"|"button"|"field"|"raw") for an operable widget, or null
/// when [w] has no LIVE affordance. Checks the runtime TYPE (locale-invariant)
/// AND the public callback fields, so a GestureDetector with onTap==null (and no
/// other live callback) is correctly NOT operable.
String? _operableKind(Widget w) {
  if (w is GestureDetector) {
    final live = w.onTap != null ||
        w.onDoubleTap != null ||
        w.onLongPress != null ||
        w.onTapDown != null ||
        w.onTapUp != null;
    return live ? 'tap' : null;
  }
  if (w is InkResponse) {
    // InkWell extends InkResponse.
    final live = w.onTap != null ||
        w.onDoubleTap != null ||
        w.onLongPress != null ||
        w.onTapDown != null;
    return live ? 'tap' : null;
  }
  if (w is RawGestureDetector) {
    return w.gestures.isNotEmpty ? 'raw' : null;
  }
  if (w is ListTile) {
    final live = w.onTap != null || w.onLongPress != null;
    return live ? 'button' : null;
  }
  final t = w.runtimeType.toString();
  if (t.contains('EditableText') ||
      t.contains('TextField') ||
      t.contains('TextFormField') ||
      t.contains('CupertinoTextField')) {
    return 'field';
  }
  if (t.contains('Switch') ||
      t.contains('Checkbox') ||
      t.contains('Radio') ||
      t.contains('Slider') ||
      t.contains('Button') ||
      t.contains('Chip') ||
      t.contains('Tab')) {
    return 'button';
  }
  return null;
}

/// Locale-invariant role token for an operable element, matching elementRole():
/// generic tappables (GestureDetector/InkWell/ListTile/raw) -> `button`.
String _operableRole(Widget w) {
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
  return 'button';
}

/// A semantics node reduced to (id, global rect, role, named) for the graph-2
/// join. `id` is SemanticsNode.id, used to collapse the several operable widgets
/// of one Material control (its outer keyed widget, its InkWell, its internal
/// RawGestureDetector) that all join to the SAME semantics node into one entry.
class _SemRect {
  _SemRect(this.id, this.rect, this.role, this.named);
  final int id;
  final Rect rect;
  final String role;
  final bool named;
}

/// Global rect of a semantics node, composing ancestor transforms (each
/// SemanticsNode.transform maps the node into its parent's coordinates).
Rect _globalRect(SemanticsNode node) {
  var matrix = Matrix4.identity();
  SemanticsNode? n = node;
  while (n != null) {
    final tr = n.transform;
    if (tr != null) matrix = tr.multiplied(matrix);
    n = n.parent;
  }
  return MatrixUtils.transformRect(matrix, node.rect);
}

/// The smallest-area semantics rect that contains [p], or null.
_SemRect? _smallestContaining(List<_SemRect> nodes, Offset p) {
  _SemRect? best;
  var bestArea = double.infinity;
  for (final s in nodes) {
    if (s.rect.contains(p)) {
      final area = s.rect.width * s.rect.height;
      if (area < bestArea) {
        bestArea = area;
        best = s;
      }
    }
  }
  return best;
}

/// Whether keyboard focus is confined to a sub-region it can't tab out of.
///
/// Reported CONSERVATIVELY as false. A real focus trap can only be told apart
/// from a legitimate modal by actually stepping the [FocusTraversalPolicy]
/// (next()/previous()) and observing focus never leave a region, which MUTATES
/// the live focus state. This snapshot must stay side-effect-free (it runs in
/// the middle of the seeded walk), so it does not drive traversal. Static scope
/// flags do NOT distinguish a trap from normal nesting: the framework marks the
/// root scope, every route scope, and each FocusTraversalGroup
/// `TraversalEdgeBehavior.closedLoop` BY DEFAULT, so a closedLoop scope is the
/// norm, not a trap signal. Emitting a guess here would feed the engine false
/// gaps. A dedicated key-driven trap oracle is the place to determine this.
bool _detectFocusTrap(FocusScopeNode rootScope) => false;

/// True when [w] roots a subtree that takes NO pointer input or is excluded
/// from semantics, so its gesture detectors are framework chrome, not real user
/// affordances. The chief offender is the route's `ModalBarrier`, whose
/// `_ModalBarrierGestureDetector` (a RawGestureDetector) sits under
/// `IgnorePointer` + `ExcludeSemantics` when no dialog is up; without this prune
/// it surfaces as a phantom operable `raw` element joined to no semantics node.
bool _isInertSubtree(Widget w) {
  if (w is IgnorePointer) return w.ignoring;
  if (w is AbsorbPointer) return w.absorbing;
  if (w is ExcludeSemantics) return w.excluding;
  return false;
}

/// Build the EXPLORE:GROUNDTRUTH payload for the current screen. [sig] MUST be
/// the SAME signature emitted on the paired EXPLORE:STATE so the engine joins
/// the two markers. Returns a JSON-ready map:
///   {"sig":..,"focusTrap":bool,"elements":[{id,operable,gestureKind,a11y{..}}]}
Map<String, dynamic> groundTruth(WidgetTester t, String sig) {
  // GRAPH 1: operable widgets in the live, on-screen element tree (offstage
  // subtrees pruned, exactly like the key/tappable walks).
  // Semantics rects are in PHYSICAL (device) pixels; RenderBox.localToGlobal
  // returns LOGICAL pixels. Scale operable hit points by the devicePixelRatio so
  // both graphs share one coordinate space for the geometric join.
  final dpr = t.view.devicePixelRatio;
  final operables = <_Operable>[];
  void walk(Element e) {
    if (_isOffstageSubtree(e.widget) || _isInertSubtree(e.widget)) return;
    final kind = _operableKind(e.widget);
    if (kind != null) {
      final pt = _hitPoint(e);
      if (pt != null) {
        operables.add(
          _Operable(kind, _operableRole(e.widget), keyStringOf(e.widget), e,
              pt * dpr),
        );
      }
    }
    e.visitChildren(walk);
  }

  final rootEl = WidgetsBinding.instance.rootElement;
  if (rootEl != null) rootEl.visitChildren(walk);

  // GRAPH 2: onstage semantics nodes as (id, global rect, role, named).
  final semNodes = <_SemRect>[];
  final root = t.binding.pipelineOwner.semanticsOwner?.rootSemanticsNode;
  if (root != null) {
    void semWalk(SemanticsNode n) {
      final d = n.getSemanticsData();
      if (!d.hasFlag(SemanticsFlag.isHidden)) {
        final named = d.label.trim().isNotEmpty ||
            d.tooltip.trim().isNotEmpty ||
            d.value.trim().isNotEmpty;
        semNodes.add(_SemRect(n.id, _globalRect(n), roleOf(d), named));
      }
      n.visitChildren((c) {
        semWalk(c);
        return true;
      });
    }

    semWalk(root);
  }

  // KEYBOARD: focus traversal order (tab order). Each FocusNode carries its
  // BuildContext (= the Focus element), so a node is ATTRIBUTED to the operable
  // element it lives inside, by render-ancestry. A control like ElevatedButton
  // owns its Focus node internally (the Focus widget's `focusNode` field is
  // null), so reading the widget field misses it; walking up from the node's
  // context to the enclosing operable element is what catches it.
  final fm = FocusManager.instance;
  final tabOrder = fm.rootScope.traversalDescendants.toList();
  final focusTrap = _detectFocusTrap(fm.rootScope);
  // Map each operable element to its nearest tab-order FocusNode by ancestry.
  final opIndexByElement = <Element, int>{};
  for (var i = 0; i < operables.length; i++) {
    opIndexByElement[operables[i].element] = i;
  }
  for (final fn in tabOrder) {
    final ctx = fn.context;
    if (ctx is! Element) continue;
    // Self-or-ancestor: the operable element enclosing this focus node.
    Element? hit;
    if (opIndexByElement.containsKey(ctx)) {
      hit = ctx;
    } else {
      ctx.visitAncestorElements((anc) {
        if (opIndexByElement.containsKey(anc)) {
          hit = anc;
          return false;
        }
        return true;
      });
    }
    if (hit != null) {
      final op = operables[opIndexByElement[hit]!];
      op.focusNode ??= fn; // first (nearest in tab order) wins.
    }
  }
  final tabOrderSet = tabOrder.toSet();

  // JOIN graph1 -> graph2 and COLLAPSE. One Material control expands into several
  // operable widgets (its outer keyed widget, its InkWell, its internal
  // RawGestureDetector) that all join to the SAME semantics node; they are one
  // logical control, so group operables by their joined semantics-node id and
  // emit ONE entry per group. The group is `operable` if any member is, has a
  // role/name if its shared semantics node does, and is focusable / in tab order
  // / keyboard-activatable if ANY member's attributed focus node says so. Within
  // a group the KEYED selector wins (else the first member's role+index), so the
  // entry's id matches the EXPLORE:STATE selector for the same control.
  // Operables that join to NO semantics node keep their own ungrouped entry
  // (these are the real gaps: operable but absent from the semantics graph).
  final groups = <int, List<int>>{}; // semantics node id -> operable indices
  final semForOp = <int, _SemRect>{};
  for (var i = 0; i < operables.length; i++) {
    final sem = _smallestContaining(semNodes, operables[i].point);
    if (sem != null) {
      semForOp[i] = sem;
      (groups[sem.id] ??= <int>[]).add(i);
    }
  }

  // Per-role structural index for keyless selectors, assigned in document order
  // over the COLLAPSED entries so it lines up with the EXPLORE:STATE indexing.
  final perRole = <String, int>{};
  String selectorFor(_Operable op) {
    if (op.keyString != null) return 'key:${op.keyString}';
    final idx = perRole[op.role] ?? 0;
    perRole[op.role] = idx + 1;
    return 'role:${op.role}#$idx';
  }

  final elements = <Map<String, dynamic>>[];
  void emitEntry(List<int> memberIdx, _SemRect? sem) {
    // Prefer the keyed member for the selector; else the first (document order).
    final lead = memberIdx.firstWhere(
      (i) => operables[i].keyString != null,
      orElse: () => memberIdx.first,
    );
    final op = operables[lead];
    final rolePresent = sem != null && sem.role != 'node';
    final namePresent = sem != null && sem.named;
    var focusable = false;
    var inTabOrder = false;
    for (final i in memberIdx) {
      final fn = operables[i].focusNode;
      if (fn == null) continue;
      if (fn.canRequestFocus && !fn.skipTraversal) focusable = true;
      if (tabOrderSet.contains(fn)) inTabOrder = true;
    }
    // keyboardActivatable: reachable by Tab (in the traversal order) AND
    // focusable, so the framework's default Enter/Space Actions can activate it.
    // A bare GestureDetector (no Focus) is neither, so this is false for it.
    final keyboardActivatable = inTabOrder && focusable;
    elements.add({
      'id': selectorFor(op),
      'operable': true,
      'gestureKind': op.gestureKind,
      'a11y': {
        'rolePresent': rolePresent,
        'namePresent': namePresent,
        'focusable': focusable,
        'inTabOrder': inTabOrder,
        'keyboardActivatable': keyboardActivatable,
      },
    });
  }

  // Emit in document order of the LEAD operable so output order is stable.
  final emittedGroups = <int>{};
  for (var i = 0; i < operables.length; i++) {
    final sem = semForOp[i];
    if (sem != null) {
      if (emittedGroups.add(sem.id)) emitEntry(groups[sem.id]!, sem);
    } else {
      emitEntry(<int>[i], null);
    }
  }

  return {'sig': sig, 'focusTrap': focusTrap, 'elements': elements};
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  Future<void> settle(WidgetTester t, int ms) async {
    for (var i = 0; i < ms ~/ 100; i++) {
      await t.pump(const Duration(milliseconds: 100));
    }
  }

  // APP-SPECIFIC: pump your app's root widget. A closure so the batch runner
  // can re-pump a FRESH widget tree between seeds (state reset). Re-pumping
  // tears down the previous tree, which is exactly what surfaces leaked-
  // resource bugs (e.g. an undisposed AnimationController) headless.
  Future<void> pumpApp(WidgetTester t) async {
    await t.pumpWidget(const OperabilityFixtureApp());
  }

  testWidgets('explore (headless)', (tester) async {
    final semantics = tester.ensureSemantics();
    // In scenario mode the real role is claimed from the conductor below (which
    // prints its own `claimed role=` marker), so don't assert role=a here.
    if (envBarrier.isEmpty) {
      emit('JOURNEY claimed role=a');
    }

    // Force the requested run locale BEFORE the app first pumps, so every screen
    // renders in that language. Scoped to the run: cleared in the teardown
    // below. A per-seed fuzz.locale still overrides this for that seed.
    if (envLocale.isNotEmpty) {
      applyLocale(tester, envLocale);
      emit('JOURNEY[a] step: locale=$envLocale');
    }

    // STRUCTURAL tap: resolve a locale-invariant selector and tap it. Returns
    // true on success.
    //   key:<keyString>   -> find.byKey (replays in ANY locale)
    //   role:<role>#<idx>  -> the idx-th tappable of that role, in document
    //                         order, tapped via the semantics action (no text)
    Future<bool> tapSelector(String sel) async {
      if (sel.startsWith('key:')) {
        final f = find.byKey(keyFromString(sel.substring(4)));
        if (f.evaluate().isEmpty) return false;
        try {
          await tester.tap(f.first, warnIfMissed: false);
          return true;
        } catch (_) {
          return false;
        }
      }
      if (sel.startsWith('role:')) {
        final hash = sel.indexOf('#');
        if (hash < 0) return false;
        final role = sel.substring('role:'.length, hash);
        final idx = int.tryParse(sel.substring(hash + 1)) ?? -1;
        if (idx < 0) return false;
        // Re-derive document-order tappables of this role from the live tree and
        // tap the idx-th via its semantics tap action. No text involved.
        var seen = -1;
        SemanticsNode? target;
        final root =
            tester.binding.pipelineOwner.semanticsOwner?.rootSemanticsNode;
        if (root != null) {
          void walk(SemanticsNode n) {
            if (target != null) return;
            final d = n.getSemanticsData();
            if (!d.hasFlag(SemanticsFlag.isHidden)) {
              final tappable = d.hasAction(SemanticsAction.tap) &&
                  !d.hasFlag(SemanticsFlag.isTextField);
              if (tappable && roleOf(d) == role) {
                seen++;
                if (seen == idx) target = n;
              }
            }
            n.visitChildren((c) {
              walk(c);
              return true;
            });
          }

          walk(root);
        }
        if (target == null) return false;
        try {
          tester.semantics.tap(find.semantics.byPredicate((n) => n == target));
          return true;
        } catch (_) {
          return false;
        }
      }
      return false;
    }

    Future<bool> goBack(WidgetTester t) async {
      try {
        final nav = t.state<NavigatorState>(find.byType(Navigator).first);
        final popped = await nav.maybePop();
        await settle(t, 900);
        return popped;
      } catch (_) {
        return false;
      }
    }

    // Property-matched replay: type a synthesized value into the text field that
    // matches `field` (by a11y label, then by a positional digit index into the
    // on-screen EditableTexts). Returns true if it filled something.
    Future<bool> fillField(String field, String value) async {
      for (final f in [
        find.bySemanticsLabel(field),
        find.bySemanticsLabel(RegExp(RegExp.escape(field))),
      ]) {
        if (f.evaluate().isNotEmpty) {
          try {
            await tester.enterText(f.first, value);
            await settle(tester, 500);
            return true;
          } catch (_) {}
        }
      }
      // Index only VISIBLE (hit-testable) fields, so a field built but offstage
      // on another PageView/IndexedStack/Tab page can't shift the positional
      // index. Same visible-only discipline the tap path uses; fall back to the
      // full set only if nothing is hit-testable.
      var edits = find.byType(EditableText).hitTestable();
      if (edits.evaluate().isEmpty) {
        edits = find.byType(EditableText);
      }
      final n = edits.evaluate().length;
      final idx = int.tryParse(field.replaceAll(RegExp(r'[^0-9]'), ''));
      if (idx != null && idx < n) {
        try {
          await tester.enterText(edits.at(idx), value);
          await settle(tester, 500);
          return true;
        } catch (_) {}
      }
      return false;
    }

    // One seed's walk. Identical action SEQUENCE to the simulator explorer for
    // the same (seed, build): the determinism contract is preserved so a
    // headless finding replays on the simulator byte-for-byte.
    // Shared verb helpers, used by BOTH the single-actor replay loop and the
    // multi-actor scenario loop, so authored type:/assert:/auth: actions behave
    // identically and the two paths can't drift. (The single-actor path used to
    // treat every non-back action as a tap, silently degrading fills/asserts to
    // misses.)
    Future<bool> waitFor(bool Function() pred) async {
      final sw = Stopwatch()..start();
      while (sw.elapsed < const Duration(seconds: 8)) {
        if (pred()) return true;
        await Future.delayed(const Duration(milliseconds: 250));
        await tester.pump(const Duration(milliseconds: 100));
      }
      return pred();
    }

    bool textPresent(String want) =>
        find.textContaining(want).evaluate().isNotEmpty ||
        find
            .bySemanticsLabel(RegExp(RegExp.escape(want)))
            .evaluate()
            .isNotEmpty;

    int countMatching(String finder) {
      if (finder.startsWith('key:')) {
        return find.byKey(keyFromString(finder.substring(4))).evaluate().length;
      }
      if (finder.startsWith('role:')) {
        final hash = finder.indexOf('#');
        final wantRole = finder.substring(
          'role:'.length,
          hash < 0 ? finder.length : hash,
        );
        var c = 0;
        final root =
            tester.binding.pipelineOwner.semanticsOwner?.rootSemanticsNode;
        if (root != null) {
          void walk(SemanticsNode n) {
            final d = n.getSemanticsData();
            if (!d.hasFlag(SemanticsFlag.isHidden) && roleOf(d) == wantRole) {
              c++;
            }
            n.visitChildren((ch) {
              walk(ch);
              return true;
            });
          }

          walk(root);
        }
        return c;
      }
      return find.textContaining(finder).evaluate().length;
    }

    Future<bool> fillSelector(String finder, String value) async {
      if (finder.startsWith('key:')) {
        final f = find.byKey(keyFromString(finder.substring(4)));
        if (f.evaluate().isEmpty) return false;
        try {
          await tester.enterText(f.first, value);
          await settle(tester, 500);
          return true;
        } catch (_) {
          return false;
        }
      }
      return fillField(finder, value);
    }

    Future<void> execAssert(String spec, String who) async {
      if (spec.startsWith('text=')) {
        final want = spec.substring('text='.length);
        final ok = await waitFor(() => textPresent(want));
        emit(
          'FUZZ:ASSERT ${ok ? "pass" : "fail"} text=${jsonEncode(want)} actor=$who',
        );
        return;
      }
      if (spec.startsWith('count:')) {
        final r = spec.substring('count:'.length);
        final eq = r.lastIndexOf('=');
        final finder = eq >= 0 ? r.substring(0, eq) : r;
        final want = eq >= 0 ? (int.tryParse(r.substring(eq + 1)) ?? 0) : 0;
        final ok = await waitFor(() => countMatching(finder) == want);
        final result = ok ? "pass" : "fail";
        final got = countMatching(finder);
        emit(
            'FUZZ:ASSERT $result count $finder want=$want got=$got actor=$who');
      }
    }

    Future<void> runSeed(FuzzCfg fuzz) async {
      final seenStates = <String>{};
      final triedEdges = <String>{};
      // Layer 3 opt-in value selectors (reproit.yaml `value_nodes:` + the
      // REPROIT_VALUE_NODES define), resolved once per seed.
      final valueSelectors = loadValueNodeSelectors();
      // Layer 2 hard cap (runner-enforced): the distinct value-class combinations
      // observed per structural value-key. Once a key has shown >8, it is capped
      // (added to `cappedKeys`) and dropped from the V: section for the rest of
      // the seed, so an adversarial value generator cannot explode the graph.
      final seenClassesPerKey = <String, Set<String>>{};
      final cappedKeys = <String>{};

      // Update the cap state from a fresh snapshot, then return the EFFECTIVE
      // canonical signature (the V: section with capped keys dropped). This is
      // the state key used everywhere below, so EXPLORE:STATE/EDGE stay aligned.
      String effectiveSigOf(Snapshot snap) {
        for (final pair in valuePairs(snap.tree)) {
          if (cappedKeys.contains(pair.key)) continue;
          final seen = seenClassesPerKey.putIfAbsent(
            pair.key,
            () => <String>{},
          );
          seen.add(pair.value);
          if (seen.length > 8) cappedKeys.add(pair.key);
        }
        return snap.effectiveSig(cappedKeys);
      }

      Snapshot observe() {
        final snap = snapshotWith(tester, valueSelectors);
        final sig = effectiveSigOf(snap);
        if (seenStates.add(sig)) {
          // sig: STRUCTURAL + value-state (roles + shape + keys + V: classes),
          // locale-invariant. labels: DISPLAY-ONLY visible text (map --show),
          // never in the sig. elements: structural selectors for replay; `nokey`
          // flags a tappable that has no developer key (the map layer can warn).
          emit(
            'EXPLORE:STATE ${jsonEncode({
                  "sig": sig,
                  if (snap.anchor != null) "route": snap.anchor,
                  "labels": snap.labels.take(maxLabelsPerState).toList(),
                  "elements": snap.tappables
                      .take(maxLabelsPerState)
                      .map((e) => {
                            "sel": e.sel,
                            "role": e.role,
                            "label": e.label,
                            if (e.bounds != null) "bounds": e.bounds,
                            if (!e.hasKey) "nokey": true
                          })
                      .toList(),
                  "texts": snap.texts
                      .take(maxLabelsPerState)
                      .map((t) => {
                            "text": t.text,
                            if (t.bounds != null) "bounds": t.bounds
                          })
                      .toList(),
                })}',
          );
          // Operability/a11y ground-truth for the SAME sig: graph1 (operable) x
          // graph2 (semantics role/name) + keyboard reachability/activation.
          emit('EXPLORE:GROUNDTRUTH ${jsonEncode(groundTruth(tester, sig))}');
        }
        return snap;
      }

      // The effective (capped) signature of a snapshot, for edge comparisons.
      String sigOf(Snapshot s) => s.effectiveSig(cappedKeys);

      // Layer 1 effect detection (runner-local): an action is EFFECTIVE iff the
      // structural+value signature changed OR the content fingerprint changed
      // (raw text moved). If neither moved it was a no-op. This stops the
      // explorer stalling on value-state screens (a counter whose structure and
      // value-class never change, but whose displayed number does).
      bool effective(Snapshot before, Snapshot after) =>
          sigOf(before) != sigOf(after) || before.contentFp != after.contentFp;

      final rng = Rng(fuzz.seed);
      if (fuzz.seed != 0) emit('JOURNEY[a] step: fuzz seed=${fuzz.seed}');
      if (fuzz.replay != null) {
        emit('JOURNEY[a] step: replaying ${fuzz.replay!.length} actions');
      }

      // Property-matched replay: drive the locale (best-effort) and type each
      // synthesized input into its matching field as that field appears.
      if (fuzz.locale != null && fuzz.locale!.isNotEmpty) {
        applyLocale(tester, fuzz.locale!);
        emit('JOURNEY[a] step: locale=${fuzz.locale}');
      }
      final filledFields = <String>{};
      Future<void> applyInputs() async {
        for (final inp in fuzz.inputs) {
          final field = inp['field'] ?? '';
          if (field.isEmpty || filledFields.contains(field)) continue;
          final value = inp['value'] ?? '';
          if (await fillField(field, value)) {
            filledFields.add(field);
            emit(
              'FUZZ:FILL ${jsonEncode({
                    "field": field,
                    "len": value.runes.length
                  })}',
            );
          }
        }
      }

      var current = observe();
      await applyInputs();
      var stuck = 0;
      final prefixLen = fuzz.prefix?.length ?? 0;
      final budget = fuzz.replay?.length ?? (fuzz.budget + prefixLen);
      for (var actions = 0; actions < budget && stuck < 3; actions++) {
        await applyInputs();
        // Choose: exact replay > frontier prefix > seeded random > systematic.
        String? act;
        if (fuzz.replay != null) {
          act = fuzz.replay![actions];
        } else if (actions < prefixLen) {
          act = fuzz.prefix![actions];
        } else if (fuzz.seed != 0) {
          // Candidates addressed by STRUCTURAL selector (key, else role+index),
          // never by visible text, so the seeded pick and any replay are
          // locale-invariant.
          final taps = current.tappables.map((e) => e.sel).toList()..sort();
          final ew = fuzz.edgeWeights[sigOf(current)] ?? const {};
          final options = [...taps.map((s) => 'tap:$s'), 'back'];
          final weights = options.map((o) => 1.0 / (1 + (ew[o] ?? 0))).toList();
          final total = weights.fold<double>(0, (a, b) => a + b);
          var r = (rng.next(1 << 20) / (1 << 20)) * total;
          act = options.last;
          for (var k = 0; k < options.length; k++) {
            r -= weights[k];
            if (r <= 0) {
              act = options[k];
              break;
            }
          }
        } else {
          for (final el in current.tappables) {
            if (!triedEdges.contains('${sigOf(current)}|${el.sel}')) {
              act = 'tap:${el.sel}';
              break;
            }
          }
          act ??= 'back';
        }

        emit('FUZZ:ACT $act');
        if (act == 'back') {
          final popped = await goBack(tester);
          final next = observe();
          // An edge is emitted whenever the structural+value STATE changed. The
          // stuck counter resets on any EFFECTIVE action (state OR content moved),
          // so a value-state screen (counter/calculator) does not stall the walk.
          if (popped && sigOf(next) != sigOf(current)) {
            emit(
              'EXPLORE:EDGE ${jsonEncode({
                    "from": sigOf(current),
                    "action": "back",
                    "to": sigOf(next)
                  })}',
            );
          }
          if (popped && effective(current, next)) {
            stuck = 0;
          } else {
            stuck++;
          }
          current = next;
          continue;
        }
        final a = act!;
        // Authored journeys replay type:/assert:/auth:, not just tap/back. Run
        // them through the SAME shared verbs the scenario path uses, or a fill/
        // expect silently degrades to a tap (MISS) - the single-actor drift bug.
        if (a.startsWith('type:') ||
            a.startsWith('assert:') ||
            a.startsWith('auth:')) {
          if (a.startsWith('type:')) {
            final body = a.substring('type:'.length);
            final eq = body.lastIndexOf('=');
            final finder = eq >= 0 ? body.substring(0, eq) : body;
            final value = eq >= 0 ? body.substring(eq + 1) : '';
            if (!await fillSelector(finder, value)) emit('FUZZ:MISS $a');
          } else if (a.startsWith('assert:')) {
            await execAssert(a.substring('assert:'.length), 'a');
          }
          // auth: is a no-op on the flutter runner (session restore unsupported).
          await settle(tester, 600);
          current = observe();
          continue;
        }
        final sel = a.substring('tap:'.length);
        triedEdges.add('${sigOf(current)}|$sel');
        final ok = await tapSelector(sel);
        if (!ok) {
          emit('FUZZ:MISS $act');
          stuck++;
          continue;
        }
        await settle(tester, 1200);
        // Drain + re-emit any exception this step latched, so the walk
        // continues past it and the finding lands in the log.
        drainException(tester, phase: 'during the walk');
        final next = observe();
        if (sigOf(next) != sigOf(current)) {
          emit(
            'EXPLORE:EDGE ${jsonEncode({
                  "from": sigOf(current),
                  "action": "tap:$sel",
                  "to": sigOf(next)
                })}',
          );
        }
        // Layer 1: reset the stall counter on any EFFECTIVE action, even when
        // the state key is unchanged (e.g. 41 -> 42 keeps POS2 but content moved).
        if (effective(current, next)) {
          stuck = 0;
        } else if (sigOf(next) == sigOf(current)) {
          stuck++;
        }
        current = next;
      }

      emit('JOURNEY[a] step: explored ${seenStates.length} states');
    }

    // ---- Multi-actor scenario client -----------------------------------
    // When a conductor URL is baked in, this device plays ONE actor: claim a
    // distinct role, pump the app, then loop pulling the next action on this
    // actor's turn and reporting done, until the conductor says DONE. The wire
    // protocol is universal; only the action execution here is Flutter-specific.
    if (envBarrier.isNotEmpty) {
      final client = HttpClient();
      Future<String> hit(String method, String path) async {
        final uri = Uri.parse('$envBarrier$path');
        final req = method == 'POST'
            ? await client.postUrl(uri)
            : await client.getUrl(uri);
        final resp = await req.close();
        return (await resp.transform(utf8.decoder).join()).trim();
      }

      // Role identity: claim from the conductor. The baked REPROIT_DEVICE label
      // is unreliable here (a warm device reuses another's build, so every
      // device would read the same label); the conductor hands out a/b/...
      // atomically so two actors can never collide on one role.
      String role;
      try {
        role = await hit('GET', '/claim');
        if (role.isEmpty || role.startsWith('ERR')) role = 'a';
      } catch (_) {
        role = 'a';
      }
      emit('JOURNEY claimed role=$role');

      await pumpApp(tester);
      await settle(tester, 2500);

      // Universal recording: a scenario traverses real, often deep screens
      // (beacon detail, chat) that a blind single-actor crawl can't reach, so
      // emit the same EXPLORE:STATE/EDGE records the fuzz crawl does. `map` then
      // folds these into the verified graph: the dual-user journeys double as the
      // mapper for screens only reachable with data or a peer.
      final scenarioSeen = <String>{};
      String observeScenario() {
        final snap = snapshot(tester);
        if (scenarioSeen.add(snap.sig)) {
          emit(
            'EXPLORE:STATE ${jsonEncode({
                  "sig": snap.sig,
                  if (snap.anchor != null) "route": snap.anchor,
                  "labels": snap.labels.take(maxLabelsPerState).toList(),
                  "elements": snap.tappables
                      .take(maxLabelsPerState)
                      .map((e) => {
                            "sel": e.sel,
                            "role": e.role,
                            "label": e.label,
                            if (e.bounds != null) "bounds": e.bounds,
                            if (!e.hasKey) "nokey": true
                          })
                      .toList(),
                  "texts": snap.texts
                      .take(maxLabelsPerState)
                      .map((t) => {
                            "text": t.text,
                            if (t.bounds != null) "bounds": t.bounds
                          })
                      .toList(),
                })}',
          );
          emit(
              'EXPLORE:GROUNDTRUTH ${jsonEncode(groundTruth(tester, snap.sig))}');
        }
        return snap.sig;
      }

      String? lastSig = observeScenario();

      // exec() below uses the shared waitFor/textPresent/countMatching/
      // fillSelector/execAssert hoisted to the testWidgets scope (so the
      // single-actor replay loop runs the exact same verbs).
      Future<void> exec(String act) async {
        emit('FUZZ:ACT $role $act');
        if (act == 'back') {
          await goBack(tester);
          return;
        }
        if (act.startsWith('auth:')) {
          // Session-restore login is not yet wired on the Flutter runner; use
          // `login(<account>)` (UI flow) for multi-user auth. No-op so ordering
          // still advances, but flag it loudly.
          emit(
            'JOURNEY[a] step: auth-restore unsupported on flutter runner; use login() for $act',
          );
          await settle(tester, 200);
          return;
        }
        if (act.startsWith('assert:')) {
          await execAssert(act.substring('assert:'.length), role);
          return;
        }
        if (act.startsWith('type:')) {
          final body = act.substring('type:'.length);
          final eq = body.lastIndexOf('=');
          final finder = eq >= 0 ? body.substring(0, eq) : body;
          final value = eq >= 0 ? body.substring(eq + 1) : '';
          var ok = await fillSelector(finder, value);
          if (!ok) {
            ok = await waitFor(() => countMatching(finder) > 0) &&
                await fillSelector(finder, value);
          }
          if (!ok) emit('FUZZ:MISS $role $act');
          return;
        }
        // default: tap:<selector>
        final sel = act.startsWith('tap:') ? act.substring('tap:'.length) : act;
        var ok = await tapSelector(sel);
        if (!ok) {
          final sw = Stopwatch()..start();
          while (!ok && sw.elapsed < const Duration(seconds: 8)) {
            await Future.delayed(const Duration(milliseconds: 250));
            await tester.pump(const Duration(milliseconds: 100));
            ok = await tapSelector(sel);
          }
        }
        if (!ok) emit('FUZZ:MISS $role $act');
        await settle(tester, 1000);
      }

      for (var guard = 0; guard < 100000; guard++) {
        String body;
        try {
          body = await hit('GET', '/next?device=$role');
        } catch (_) {
          await Future.delayed(const Duration(milliseconds: 100));
          continue;
        }
        if (body == 'DONE') break;
        if (body == 'WAIT') {
          await Future.delayed(const Duration(milliseconds: 40));
          continue;
        }
        final act = body.startsWith('ACT\t') ? body.substring(4) : body;
        await exec(act);
        // Record the traversal: a state on every step, an edge when a tap/back
        // moved the structural signature.
        final newSig = observeScenario();
        final isEdge = act == 'back' || act.startsWith('tap:');
        if (isEdge && lastSig != null && newSig != lastSig) {
          emit(
            'EXPLORE:EDGE ${jsonEncode({
                  "from": lastSig,
                  "action": act == 'back' ? 'back' : act,
                  "to": newSig
                })}',
          );
        }
        lastSig = newSig;
        try {
          await hit('POST', '/done?device=$role');
        } catch (_) {}
      }

      client.close();
      emit('JOURNEY DONE');
      await settle(tester, 1000);
      clearLocale(tester);
      semantics.dispose();
      return;
    }

    // Run every seed in this session in sequence. Between seeds, re-pump a
    // FRESH widget tree (replacing the whole tree disposes the prior one) so
    // each seed starts clean AND leaked-resource teardown asserts surface.
    final batch = FuzzCfg.loadBatch();
    for (final fuzz in batch) {
      emit('SEED:BEGIN ${fuzz.seed}');
      // APP-SPECIFIC: fresh root widget. Re-pumping a fresh tree disposes the
      // PREVIOUS seed's tree, so a leaked-resource bug touched last seed
      // surfaces HERE (the dispose-time assert is attributed to this seed's
      // BEGIN, which is fine: the trace that reached it is the prior seed's).
      drainException(tester, phase: 'on teardown of the previous seed');
      await pumpApp(tester);
      await settle(tester, 1500);
      drainException(tester, phase: 'on first pump');
      await runSeed(fuzz);
      // Dispose this seed's tree NOW (pump an empty tree) so a leak it caused
      // is latched and attributed to THIS seed, before SEED:END.
      await tester.pumpWidget(const SizedBox.shrink());
      await tester.pump(const Duration(milliseconds: 200));
      drainException(tester, phase: 'on seed teardown');
      emit('SEED:END ${fuzz.seed}');
    }

    emit('JOURNEY DONE');
    // Scope the locale override to this run only.
    clearLocale(tester);
    semantics.dispose();
  });
}
