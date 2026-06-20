/// ReproIt production telemetry for Flutter.
///
/// Emits the SAME state-graph and error events from real users that the reproit
/// test runners emit, so the production graph aligns 1:1 with test-time graphs
/// and a prod "cannot reproduce" becomes a deterministic replay.
///
/// Usage (one line in main):
///
/// ```dart
/// void main() {
///   WidgetsFlutterBinding.ensureInitialized();
///   ReproIt.init(const ReproItConfig(
///     appId: 'example',
///     endpoint: 'https://ingest.reproit.example',
///     apiKey: 'sk_...',
///   ));
///   runApp(const MyApp());
/// }
/// ```
///
/// Optionally add [ReproIt.navigatorObserver] to `MaterialApp.navigatorObservers`
/// to label route transitions; without it, transitions are still captured from
/// the semantics tree and tap hit-testing.
library reproit_flutter;

import 'dart:async';
import 'dart:convert';
import 'dart:math';
import 'dart:ui' show Offset, Rect, PlatformDispatcher;

import 'package:crypto/crypto.dart' show sha256;
import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart' show GestureBinding;
import 'package:flutter/scheduler.dart';
import 'package:flutter/semantics.dart';
import 'package:flutter/widgets.dart';
import 'package:http/http.dart' as http;

import 'src/capture.dart';
import 'src/signature.dart';

export 'src/signature.dart'
    show
        RNode,
        descriptor,
        descriptorFrom,
        signature,
        signatureFrom,
        valueClass,
        valuePairs,
        kValueRoles,
        fnv1a32,
        fnv1a32Hex,
        selectorFor,
        Selector;

/// Configuration for [ReproIt.init]. Field names and defaults mirror the web SDK
/// (`sdk/reproit-web.js`) so behavior is consistent across platforms.
class ReproItConfig {
  /// Identifies the app in the cloud (the `appId` in every batch).
  final String appId;

  /// `POST <endpoint>/v1/events`. If null, events go only to [onEvent]/debug.
  final String? endpoint;

  /// Bearer token sent as `Authorization: Bearer <apiKey>` when set.
  final String? apiKey;

  /// Dev hook / custom transport; called for every event in addition to (or
  /// instead of, when [endpoint] is null) the HTTP sink.
  final void Function(Map<String, dynamic> event)? onEvent;

  /// Fraction of sessions that report (0..1). Decided once at init.
  final double sampleRate;

  /// Max distinct labels captured per state (matches the runners).
  final int maxLabels;

  /// Labels longer than this are ignored (matches the runners).
  final int maxLabelLen;

  /// Max length of the action trail kept for repro paths.
  final int pathCap;

  /// How often batched events are flushed.
  final Duration flushInterval;

  /// When true, only signatures are sent (no human-readable labels).
  final bool redactLabels;

  /// Settle window: snapshot once the UI has been quiet this long.
  final Duration debounce;

  const ReproItConfig({
    required this.appId,
    this.endpoint,
    this.apiKey,
    this.onEvent,
    this.sampleRate = 1.0,
    this.maxLabels = 24,
    this.maxLabelLen = 40,
    this.pathCap = 60,
    this.flushInterval = const Duration(seconds: 5),
    this.redactLabels = false,
    this.debounce = const Duration(milliseconds: 350),
  });
}

class _Snapshot {
  final String sig;
  final List<String> labels;
  final int unlabeled;
  _Snapshot(this.sig, this.labels, this.unlabeled);
}

/// PII-safe input fingerprinting (tier-3 on-error context).
///
/// Some bugs only reproduce with a specific INPUT property: a 312-char name, an
/// emoji, a Turkish dotless "i", an empty field, an RTL string. To reproduce
/// those without storing PII we capture DERIVED FEATURES of on-screen text-field
/// values at error time, never the values themselves; the cloud turns these into
/// a property-matched replay fixture.
///
/// [fingerprintValue] is the load-bearing pure function: identical shape and
/// rules across all five SDKs and host-unit-tested in each. It returns FEATURES
/// only and NEVER includes the raw string.
class ReproItFingerprint {
  /// Fingerprint schema version. Bumped to 2 for the byte/script/combining/
  /// zero-width/newline/edge-whitespace features below; the cloud reads it to
  /// stay backward-compatible with v1 fingerprints (len/charset/emoji/rtl/empty).
  static const int fpVersion = 2;

  /// Code-point count (so "José🎉" -> 5), charset, emoji/RTL/empty flags, plus
  /// the v2 features: bytes, scripts, combining/zero-width/newline/edge-ws.
  static Map<String, Object> fingerprintValue(String value) {
    final runes = value.runes.toList();
    final len = runes.length;
    final isEmpty = value.trim().isEmpty;
    final units = value.codeUnits;
    var hasUnicode = false;
    var allDigits = !isEmpty;
    var hasNewline = false;
    for (final cp in runes) {
      if (cp > 0x7f) hasUnicode = true;
      if (cp < 0x30 || cp > 0x39) allDigits = false;
    }
    for (final c in units) {
      if (c == 0x0a || c == 0x0d) hasNewline = true;
    }
    final charset = hasUnicode ? 'unicode' : (allDigits ? 'numeric' : 'ascii');
    // Edge whitespace: a fixed whitespace set (parity-safe, not locale trim).
    bool isWs(int cc) =>
        cc == 0x09 ||
        cc == 0x0a ||
        cc == 0x0b ||
        cc == 0x0c ||
        cc == 0x0d ||
        cc == 0x20 ||
        cc == 0xa0;
    final edgeWs =
        units.isNotEmpty && (isWs(units.first) || isWs(units.last));
    return <String, Object>{
      'len': len,
      'bytes': utf8.encode(value).length,
      'charset': charset,
      'scripts': _scripts(units),
      'hasEmoji': _hasEmoji(runes),
      'isEmpty': isEmpty,
      'isRtl': _isRtl(runes),
      'hasCombiningMarks': _hasCombining(units),
      'hasZeroWidth': _hasZeroWidth(units),
      'hasNewline': hasNewline,
      'leadingTrailingWhitespace': edgeWs,
    };
  }

  /// Zero-width / invisible code points (injection + normalization breakers).
  static bool _hasZeroWidth(List<int> units) {
    for (final c in units) {
      if (c == 0x200b ||
          c == 0x200c ||
          c == 0x200d ||
          c == 0x2060 ||
          c == 0xfeff) {
        return true;
      }
    }
    return false;
  }

  /// Combining marks (a base char + combining accent renders differently than a
  /// precomposed one; a classic normalization/layout breaker).
  static bool _hasCombining(List<int> units) {
    for (final c in units) {
      if ((c >= 0x0300 && c <= 0x036f) ||
          (c >= 0x1ab0 && c <= 0x1aff) ||
          (c >= 0x1dc0 && c <= 0x1dff) ||
          (c >= 0x20d0 && c <= 0x20ff) ||
          (c >= 0xfe20 && c <= 0xfe2f)) {
        return true;
      }
    }
    return false;
  }

  /// The Unicode SCRIPTS present, as a sorted unique list of coarse bucket
  /// names. Mixed-script (e.g. ["Arabic","Latin"]) is what bidi bugs need, which
  /// `isRtl` alone can't express. Ranges are fixed and shared verbatim with the
  /// other SDKs.
  static List<String> _scripts(List<int> units) {
    final found = <String>{};
    for (final c in units) {
      if ((c >= 0x41 && c <= 0x5a) ||
          (c >= 0x61 && c <= 0x7a) ||
          (c >= 0xc0 && c <= 0x24f) ||
          (c >= 0x1e00 && c <= 0x1eff)) {
        found.add('Latin');
      } else if (c >= 0x370 && c <= 0x3ff) {
        found.add('Greek');
      } else if (c >= 0x400 && c <= 0x4ff) {
        found.add('Cyrillic');
      } else if (c >= 0x590 && c <= 0x5ff) {
        found.add('Hebrew');
      } else if ((c >= 0x600 && c <= 0x6ff) ||
          (c >= 0x750 && c <= 0x77f) ||
          (c >= 0x8a0 && c <= 0x8ff)) {
        found.add('Arabic');
      } else if (c >= 0x900 && c <= 0x97f) {
        found.add('Devanagari');
      } else if (c >= 0xe00 && c <= 0xe7f) {
        found.add('Thai');
      } else if ((c >= 0x3040 && c <= 0x30ff) ||
          (c >= 0x3400 && c <= 0x9fff) ||
          (c >= 0xac00 && c <= 0xd7a3) ||
          (c >= 0xf900 && c <= 0xfaff)) {
        found.add('CJK');
      }
    }
    final list = found.toList()..sort();
    return list;
  }

  /// Any code point in a strong RTL Unicode block (Arabic / Hebrew / ...).
  static bool _isRtl(List<int> runes) {
    for (final c in runes) {
      if ((c >= 0x0590 && c <= 0x05ff) || // Hebrew
          (c >= 0x0600 && c <= 0x06ff) || // Arabic
          (c >= 0x0700 && c <= 0x074f) || // Syriac
          (c >= 0x0780 && c <= 0x07bf) || // Thaana
          (c >= 0x07c0 && c <= 0x07ff) || // N'Ko
          (c >= 0x08a0 && c <= 0x08ff) || // Arabic Extended-A
          (c >= 0xfb1d && c <= 0xfb4f) || // Hebrew presentation forms
          (c >= 0xfb50 && c <= 0xfdff) || // Arabic presentation forms-A
          (c >= 0xfe70 && c <= 0xfeff)) { // Arabic presentation forms-B
        return true;
      }
    }
    return false;
  }

  /// Common emoji / pictographic blocks + regional indicators (flags).
  static bool _hasEmoji(List<int> runes) {
    for (final c in runes) {
      if ((c >= 0x1f000 && c <= 0x1faff) || // pictographs, emoji, symbols
          (c >= 0x1f1e6 && c <= 0x1f1ff) || // regional indicators (flags)
          (c >= 0x2600 && c <= 0x27bf) || // misc symbols + dingbats
          c == 0x2764 || // heavy black heart
          c == 0xfe0f || // variation selector-16 (emoji style)
          c == 0x200d) { // zero-width joiner (emoji sequences)
        return true;
      }
    }
    return false;
  }
}

class _Step {
  final String sig;
  final String action;
  _Step(this.sig, this.action);
  Map<String, dynamic> toJson() => {'sig': sig, 'action': action};
}

/// The ReproIt telemetry singleton.
class ReproIt {
  ReproIt._(this._cfg);
  static ReproIt? _i;

  final ReproItConfig _cfg;
  SemanticsHandle? _semantics;
  Timer? _debounce;
  Timer? _flushTimer;
  final List<Map<String, dynamic>> _queue = [];
  final List<_Step> _path = [];
  // PII-safe context dimensions sent with each batch (the "which users" answer).
  final Map<String, Object?> _context = {};
  String? _currentSig;
  String? _pendingAction; // derived at tap time from a semantics hit-test
  String? _anchor; // current screen anchor (route name), prefixes the signature
  bool _disposed = false;

  /// Initialize telemetry. Safe to call once; later calls are ignored.
  static void init(ReproItConfig config) {
    if (_i != null) return;
    // Sampling decision, made once per session.
    if (config.sampleRate < 1.0 && Random().nextDouble() > config.sampleRate) {
      return;
    }
    final inst = ReproIt._(config);
    _i = inst;
    inst._start();
  }

  /// Add to `MaterialApp.navigatorObservers` to label route transitions as
  /// `nav:<routeName>`; optional (transitions are captured without it too).
  static NavigatorObserver get navigatorObserver => _ReproItNavObserver();

  /// Flush queued events immediately (e.g. before a known teardown).
  static Future<void> flush() => _i?._flush() ?? Future.value();

  /// The current context dimensions sent with each batch (read-only view).
  @visibleForTesting
  static Map<String, Object?> get context =>
      Map.unmodifiable(_i?._context ?? const {});

  /// Attach a hashed user id (so the cloud can group "these N users hit it"
  /// without storing identity) plus optional context dimensions.
  static void identify(String userId, {Map<String, Object?>? context}) {
    final inst = _i;
    if (inst == null) return;
    inst._context['uid'] =
        sha256.convert(utf8.encode(userId)).toString().substring(0, 16);
    if (context != null) inst._context.addAll(context);
  }

  /// Set a single PII-safe context dimension (e.g. role, plan, a count bucket).
  static void setContext(String key, Object? value) =>
      _i?._context[key] = value;

  /// Merge several context dimensions at once.
  static void setContexts(Map<String, Object?> values) =>
      _i?._context.addAll(values);

  void _start() {
    final binding = WidgetsBinding.instance;
    // Tier-1 auto dimensions: zero-PII, web-safe, high-signal for "works for me
    // but not for them" bugs (locale, platform, timezone, text scale, build).
    final d = PlatformDispatcher.instance;
    _context.addAll({
      'platform': kIsWeb ? 'web' : defaultTargetPlatform.name,
      'locale': d.locale.toLanguageTag(),
      'tz': DateTime.now().timeZoneName,
      'textScale': d.textScaleFactor,
      'release': kReleaseMode,
    });
    // Force the semantics tree on even with no a11y service attached; this is
    // what lets us read the same tree the test runner sees.
    _semantics = binding.ensureSemantics();

    // Capture taps to label edges (mirrors the web SDK's click listener).
    GestureBinding.instance.pointerRouter.addGlobalRoute(_onPointer);

    // Snapshot after the UI settles (debounced per frame).
    binding.addPersistentFrameCallback((_) => _scheduleSnapshot());

    // Errors -> error events carrying the graph path.
    final priorFlutterOnError = FlutterError.onError;
    FlutterError.onError = (details) {
      _recordError(details.exceptionAsString(), details.stack);
      if (priorFlutterOnError != null) {
        priorFlutterOnError(details);
      } else {
        FlutterError.presentError(details);
      }
    };
    final priorPlatformOnError = PlatformDispatcher.instance.onError;
    PlatformDispatcher.instance.onError = (error, stack) {
      _recordError(error.toString(), stack);
      return priorPlatformOnError?.call(error, stack) ?? false;
    };

    _flushTimer = Timer.periodic(_cfg.flushInterval, (_) => _flush());
    // First snapshot once the first frame is up.
    SchedulerBinding.instance.addPostFrameCallback((_) => _scheduleSnapshot());
  }

  void _scheduleSnapshot() {
    if (_disposed) return;
    _debounce?.cancel();
    _debounce = Timer(_cfg.debounce, _maybeSnapshot);
  }

  // ---- semantics tree walk -------------------------------------------------

  /// Walk the live semantics tree, invoking [onNode] with each visible node's
  /// data and its global rect (logical pixels).
  void _walk(void Function(SemanticsData data, Rect globalRect) onNode) {
    final root =
        WidgetsBinding.instance.pipelineOwner.semanticsOwner?.rootSemanticsNode;
    if (root == null) return;
    void visit(SemanticsNode node, Matrix4 parentToGlobal) {
      if (node.rect.isEmpty) return;
      final data = node.getSemanticsData();
      if (data.hasFlag(SemanticsFlag.isHidden)) return;
      final toGlobal = node.transform == null
          ? parentToGlobal
          : (parentToGlobal.clone()..multiply(node.transform!));
      final globalRect = MatrixUtils.transformRect(toGlobal, node.rect);
      onNode(data, globalRect);
      node.visitChildren((child) {
        visit(child, toGlobal);
        return true;
      });
    }

    visit(root, Matrix4.identity());
  }

  static String _labelOf(SemanticsData d) =>
      d.label.trim().split('\n').first.trim();

  static bool _isTappable(SemanticsData d) =>
      d.hasAction(SemanticsAction.tap) && !d.hasFlag(SemanticsFlag.isTextField);

  static bool _isNamed(SemanticsData d) =>
      _labelOf(d).isNotEmpty ||
      d.tooltip.trim().isNotEmpty ||
      d.value.trim().isNotEmpty;

  /// Clip a label to maxLabelLen, byte-identical to the runners' clipLabel:
  /// names <= maxLen are unchanged; longer names become the first (maxLen - 9)
  /// chars + '#' + the 8-hex FNV-1a of the full name. This keeps long-labeled
  /// elements explorable (not dropped) AND keeps production signatures matching
  /// the runners' test signatures on screens with long labels.
  String _clipLabel(String name) {
    final maxLen = _cfg.maxLabelLen;
    if (name.length <= maxLen) return name;
    var h = 0x811c9dc5;
    for (final c in name.codeUnits) {
      h ^= c;
      h = (h * 0x01000193) & 0xffffffff;
    }
    final suffix = '#${h.toRadixString(16).padLeft(8, '0')}';
    return name.substring(0, maxLen - suffix.length) + suffix;
  }

  /// Developer keys keyed by canonical role, in document order, so a semantics
  /// node can be matched to the widget Key that produced it (developer keys live
  /// on Widgets, not on SemanticsData). Mirrors the explorer templates.
  Map<String, List<String>> _keyedIdsByRole() {
    final byRole = <String, List<String>>{};
    void roleOfWidget(Element e) {
      final id = idFromKey(e.widget.key);
      if (id == null) return;
      final t = e.widget.runtimeType.toString();
      String? role;
      if (t.contains('EditableText') ||
          t.contains('TextField') ||
          t.contains('TextFormField') ||
          t.contains('CupertinoTextField')) {
        role = 'textfield';
      } else if (t.contains('Switch')) {
        role = 'switch';
      } else if (t.contains('Radio')) {
        role = 'radio';
      } else if (t.contains('Checkbox')) {
        role = 'checkbox';
      } else if (t.contains('Slider')) {
        role = 'slider';
      } else if (t.contains('Button') || t.contains('Chip') || t.contains('Tab')) {
        role = 'button';
      } else if (t.contains('InkWell') ||
          t.contains('GestureDetector') ||
          t.contains('InkResponse') ||
          t.contains('ListTile')) {
        role = 'button';
      } else if (t.contains('Image')) {
        role = 'image';
      }
      if (role != null) (byRole[role] ??= <String>[]).add(id);
    }

    final root = WidgetsBinding.instance.rootElement;
    if (root != null) {
      void walk(Element e) {
        roleOfWidget(e);
        e.visitChildren(walk);
      }

      root.visitChildren(walk);
    }
    return byRole;
  }

  /// Build the canonical [RNode] tree (docs/signature.md "Inputs") from the live
  /// semantics tree. Roles come from flags only; ids come from developer Keys
  /// matched by role in document order; localized text is never read in. The
  /// whole tree is wrapped in a `screen` root so the signature has one root.
  RNode _captureTree() {
    final keyedByRole = _keyedIdsByRole();
    final perRole = <String, int>{};

    RNode? build(SemanticsNode node) {
      final data = node.getSemanticsData();
      if (data.hasFlag(SemanticsFlag.isHidden)) {
        // Skip the hidden node itself but keep walking its children at this
        // level (a hidden wrapper should not break the structure).
        final kids = <RNode>[];
        node.visitChildren((c) {
          final built = build(c);
          if (built != null) kids.add(built);
          return true;
        });
        // Splice children up: represent the hidden wrapper as a transparent
        // group only if it actually has retained children.
        if (kids.isEmpty) return null;
        return RNode(role: 'group', children: kids);
      }
      final role = roleFromSemantics(data);
      final type = inputTypeFromSemantics(data, role);
      // Match a developer id by role in document order.
      final idx = perRole[role] ?? 0;
      perRole[role] = idx + 1;
      final roleIds = keyedByRole[role];
      final id = (roleIds != null && idx < roleIds.length) ? roleIds[idx] : null;
      // Layer 2 value-state: capture a value-role node's displayed value (text
      // field, slider, live region) so the canonical V: section folds in a
      // bounded value-class. Chrome roles return a null value here.
      final value = valueFromSemantics(data);
      final valueNode = value != null && valueNodeFlagFor(data);
      final kids = <RNode>[];
      node.visitChildren((c) {
        final built = build(c);
        if (built != null) kids.add(built);
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

    final root =
        WidgetsBinding.instance.pipelineOwner.semanticsOwner?.rootSemanticsNode;
    final children = <RNode>[];
    if (root != null) {
      root.visitChildren((c) {
        final built = build(c);
        if (built != null) children.add(built);
        return true;
      });
    }
    return RNode(role: 'screen', children: children);
  }

  _Snapshot? _snapshot() {
    final labels = <String>[];
    var unlabeled = 0;
    var any = false;
    _walk((d, _) {
      any = true;
      final label = _labelOf(d);
      final tappable = _isTappable(d);
      if (tappable && !_isNamed(d)) unlabeled++;
      if (label.isEmpty) return;
      labels.add(_clipLabel(label));
    });
    if (!any) return null;
    final unique = labels.toSet().toList();
    // STRUCTURAL signature: canonical descriptor of the captured node tree,
    // prefixed by the screen anchor (route name). Locale-invariant by
    // construction (no text enters the tree). Matches the Rust oracle and the
    // fuzz explorer, which derive the anchor the same way.
    final tree = _captureTree();
    final sig = signature(_anchor ?? _routeAnchor(), tree);
    return _Snapshot(sig, unique.take(_cfg.maxLabels).toList(), unlabeled);
  }

  /// The current screen anchor read directly from the live Navigator, used when
  /// no [navigatorObserver] supplied one. This mirrors `screenAnchor` in the
  /// explorer templates so the SDK and the runner agree on the anchor (hence on
  /// the signature) even without the observer attached.
  String? _routeAnchor() {
    String? name;
    // Prefer the topmost route's name from the first NavigatorState found.
    final root = WidgetsBinding.instance.rootElement;
    if (root == null) return null;
    NavigatorState? nav;
    void findNav(Element e) {
      if (nav != null) return;
      if (e is StatefulElement && e.state is NavigatorState) {
        nav = e.state as NavigatorState;
        return;
      }
      e.visitChildren(findNav);
    }

    root.visitChildren(findNav);
    final n = nav;
    if (n == null) return null;
    n.popUntil((r) {
      name ??= r.settings.name;
      return true;
    });
    return (name != null && name!.isNotEmpty) ? name : null;
  }

  /// Collect PII-safe fingerprints of on-screen text fields for the on-error
  /// context. Walks the semantics tree for text-field nodes, fingerprints each
  /// value to FEATURES, then discards the value. The raw text never leaves this
  /// method.
  ///
  /// LIMITATION (honest, see README): a field's text is read from the semantics
  /// node's `value` (what the platform a11y layer exposes). Obscured fields
  /// (`obscureText`, e.g. passwords) report their value as masked bullets in
  /// semantics, so their fingerprint reflects the masked form (length is right,
  /// charset is ascii); we treat that as acceptable since the real value is
  /// never read. Fields with no value contribute `isEmpty:true`.
  List<Map<String, Object>> _collectFields() {
    final out = <Map<String, Object>>[];
    var index = 0;
    _walk((d, _) {
      if (!d.hasFlag(SemanticsFlag.isTextField)) return;
      final label = _labelOf(d);
      final field = label.isNotEmpty
          ? label
          : (d.hint.trim().isNotEmpty ? d.hint.trim() : '#${index}');
      index++;
      final fp = ReproItFingerprint.fingerprintValue(d.value);
      out.add(<String, Object>{'field': field, ...fp});
    });
    return out;
  }

  /// The accessible name of the deepest tappable, named node under [point].
  /// [point] is a pointer position in logical pixels; the semantics tree is in
  /// physical pixels, so scale by devicePixelRatio before hit-testing.
  String? _labelAt(Offset point) {
    final dpr = WidgetsBinding
            .instance.platformDispatcher.implicitView?.devicePixelRatio ??
        1.0;
    final p = point * dpr;
    String? best;
    _walk((d, rect) {
      if (!rect.contains(p)) return;
      if (!_isTappable(d)) return;
      final label = _labelOf(d);
      if (label.isEmpty) return;
      best = _clipLabel(label); // deepest wins; clip to match the snapshot key
    });
    return best;
  }

  // ---- event capture -------------------------------------------------------

  void _onPointer(PointerEvent e) {
    if (_disposed) return;
    if (e is PointerDownEvent) {
      final label = _labelAt(e.position);
      _pendingAction = label != null ? 'tap:$label' : 'tap:?';
    }
  }

  void _onRoute(String? routeName) {
    // Prefer an explicit nav action over a stale tap if a route just changed.
    _pendingAction = routeName != null && routeName.isNotEmpty
        ? 'nav:$routeName'
        : 'nav';
    // The route name is the screen anchor: it prefixes the structural signature
    // (docs/signature.md "Anchor short-circuit semantics"). A null/empty name
    // leaves the anchor empty, which still emits the `A:` prefix line.
    if (routeName != null && routeName.isNotEmpty) _anchor = routeName;
  }

  void _maybeSnapshot() {
    if (_disposed) return;
    final snap = _snapshot();
    if (snap == null) return;
    if (_currentSig == null) {
      // initial state
      _currentSig = snap.sig;
      _emitEdge(from: null, action: 'load', to: snap, append: true);
      return;
    }
    if (snap.sig == _currentSig) return;
    final action = _pendingAction ?? 'auto';
    _pendingAction = null;
    _emitEdge(from: _currentSig, action: action, to: snap, append: true);
    _currentSig = snap.sig;
  }

  void _emitEdge({
    required String? from,
    required String action,
    required _Snapshot to,
    required bool append,
  }) {
    if (append) {
      _path.add(_Step(from ?? '', action));
      if (_path.length > _cfg.pathCap) _path.removeAt(0);
    }
    final ev = <String, dynamic>{
      'kind': 'edge',
      if (from != null) 'from': from,
      'action': action,
      'to': to.sig,
      't': DateTime.now().millisecondsSinceEpoch,
    };
    if (!_cfg.redactLabels) ev['labels'] = to.labels;
    _enqueue(ev);
  }

  void _recordError(String message, StackTrace? stack) {
    if (_disposed) return;
    final lines = stack == null
        ? <String>[]
        : stack.toString().split('\n').where((l) => l.trim().isNotEmpty).take(8).toList();
    String source = '';
    int line = 0;
    if (lines.isNotEmpty) {
      // best-effort: pull "(file.dart:42:..)" out of the top frame
      final m = RegExp(r'([\w./-]+\.dart):(\d+)').firstMatch(lines.first);
      if (m != null) {
        source = m.group(1)!;
        line = int.tryParse(m.group(2)!) ?? 0;
      }
    }
    final ev = <String, dynamic>{
      'kind': 'error',
      'sig': _currentSig ?? '',
      'path': _path.map((s) => s.toJson()).toList(),
      'message': message,
      'stack': lines,
      'source': source,
      'line': line,
      't': DateTime.now().millisecondsSinceEpoch,
    };
    // Tier-3 on-error context: PII-safe fingerprints of on-screen text fields,
    // under `context.fingerprint`. Best-effort: never break error reporting.
    try {
      final fp = _collectFields();
      if (fp.isNotEmpty) {
        ev['context'] = {
          'fingerprint': fp,
          'fpVersion': ReproItFingerprint.fpVersion,
        };
      }
    } catch (_) {}
    _enqueue(ev);
    // Errors are worth shipping promptly.
    scheduleMicrotask(_flush);
  }

  void _enqueue(Map<String, dynamic> ev) {
    _cfg.onEvent?.call(ev);
    if (_cfg.endpoint == null) {
      if (_cfg.onEvent == null && kDebugMode) {
        debugPrint('reproit ${jsonEncode(ev)}');
      }
      return;
    }
    _queue.add(ev);
  }

  // ---- transport -----------------------------------------------------------

  /// THE canonical structural signature: FNV-1a 32-bit over the canonical
  /// descriptor of a node tree, prefixed by the screen [anchor]
  /// (docs/signature.md). This is what production edges carry and what the parity
  /// gate asserts against `signature_vectors.json`. Exposed for parity tests and
  /// advanced use.
  static String signatureOfTree(String? anchor, RNode tree) =>
      signature(anchor, tree);

  /// LEGACY compatibility entry: FNV-1a over sorted, pipe-joined labels. This is
  /// NOT the structural signature and does NOT match the oracle; it is retained
  /// only so older callers that hashed a label set keep compiling. New code must
  /// use [signatureOfTree]. The real signature emitted on edges is structural.
  @visibleForTesting
  static String signatureOf(List<String> labels) {
    final s = (labels.toList()..sort()).join('|');
    return fnv1a32(s);
  }

  /// PII-safe fingerprint of a single text value (FEATURES, never the value).
  /// Exposed for unit tests and advanced use. See [ReproItFingerprint].
  static Map<String, Object> fingerprintValue(String value) =>
      ReproItFingerprint.fingerprintValue(value);

  Future<void> _flush() async {
    if (_disposed || _queue.isEmpty) return;
    final endpoint = _cfg.endpoint;
    if (endpoint == null) {
      _queue.clear();
      return;
    }
    final batch = _queue.toList();
    _queue.clear();
    final body = jsonEncode({
      'appId': _cfg.appId,
      'sentAt': DateTime.now().millisecondsSinceEpoch,
      if (_context.isNotEmpty) 'ctx': _context,
      'events': batch,
    });
    try {
      await http.post(
        Uri.parse('$endpoint/v1/events'),
        headers: {
          'Content-Type': 'application/json',
          if (_cfg.apiKey != null) 'Authorization': 'Bearer ${_cfg.apiKey}',
        },
        body: body,
      );
    } catch (_) {
      // Best-effort: re-queue this batch ahead of newer events for one retry.
      _queue.insertAll(0, batch);
    }
  }

  /// Tear down (mainly for tests).
  static void dispose() {
    final inst = _i;
    if (inst == null) return;
    inst._disposed = true;
    inst._debounce?.cancel();
    inst._flushTimer?.cancel();
    inst._semantics?.dispose();
    _i = null;
  }
}

class _ReproItNavObserver extends NavigatorObserver {
  void _note(Route<dynamic>? route) {
    ReproIt._i?._onRoute(route?.settings.name);
  }

  @override
  void didPush(Route<dynamic> route, Route<dynamic>? previousRoute) =>
      _note(route);
  @override
  void didPop(Route<dynamic> route, Route<dynamic>? previousRoute) =>
      _note(previousRoute);
  @override
  void didReplace({Route<dynamic>? newRoute, Route<dynamic>? oldRoute}) =>
      _note(newRoute);
}
