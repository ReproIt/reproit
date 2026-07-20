part of '../reproit_explorer.dart';

const int actionBudget = 36;
const int maxLabelLen = 40;
const int maxLabelsPerState = 24;

/// Fuzz config: a HOST file path baked in as one constant define, so one
/// build serves every seed and replay (simulator apps read host paths).
/// JSON: {"seed": 42, "budget": 60} for a seeded walk,
///       {"replay": ["tap:Meet", "back", ...]} for exact replay (shrinking), or
///       {"batch": [ {"seed":..,"budget":..}, ... ]} to run several seeds in
///       ONE drive session (install/launch/connect paid once; the widget tree
///       is re-pumped between seeds so each seed starts from a fresh state).
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

/// Runtime permission to DENY for the PERMISSION-WALK oracle, baked via
/// `--dart-define=REPROIT_DENY_PERMISSION=camera` (any label the app requests;
/// only its name is used, for the finding). When set, the explorer mocks the
/// permission_handler platform channel to answer every request as
/// permanentlyDenied and marks each screen reached AFTER a denial with
/// EXPLORE:PERMISSIONWALK; the Rust invariant then fires only for a marked screen
/// that is also a graph dead end. Empty for ordinary runs (no denial sweep).
const String envDenyPermission = String.fromEnvironment(
  'REPROIT_DENY_PERMISSION',
);

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
    this.contractActions = const [],
    this.inputs = const [],
    this.locale,
  });
  final int seed;
  final int budget;
  final List<String>? replay;

  /// Property-matched replay (tier 3): synthesized, deterministic field values
  /// to type into matching text fields so a data-specific bug (a long unicode
  /// name, an emoji, an empty/RTL field) reproduces. Each entry is
  /// {field, value}; `field` matches an a11y label or a positional index.
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
  final List<String> contractActions;

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
      contractActions:
          (j['contractActions'] as List?)?.cast<String>() ?? const [],
      inputs: inputs,
      locale: j['locale'] as String?,
    );
  }

  /// The list of per-seed configs to run in this session: a single-element
  /// list for {"seed":..}/{"replay":..} (backward compatible), or the explicit
  /// list for {"batch":[...]}. Returns one default config if nothing is set.
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
/// when their role is not a value-role. The list is read once from the host
/// `reproit.yaml` (the file the runner already owns); a `--dart-define=
/// REPROIT_VALUE_NODES=key:score,role:text#2` override is also honored so a
/// simulator build (which cannot read host files) still gets the list.
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
