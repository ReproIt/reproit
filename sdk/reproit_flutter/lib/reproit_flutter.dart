// ignore_for_file: deprecated_member_use

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
///     endpoint: 'https://ingest.reproit.com',
///     apiKey: 'pk_live_...',
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
import 'src/causal.dart';
// App-invariant channel: reads REPROIT_INVARIANT_FILE and appends markers, via
// dart:io on native and a no-op stub on web (keeps the SDK web-safe).
import 'src/invariant_channel_stub.dart'
    if (dart.library.io) 'src/invariant_channel_io.dart' as invchan;
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
export 'src/causal.dart' show ReproItCausalClient, redactCausal;

/// Configuration for [ReproIt.init]. Field names and defaults mirror the web SDK
/// (`sdk/reproit-web.js`) so behavior is consistent across platforms.
class ReproItConfig {
  /// Identifies the app in the cloud (the `appId` in every batch).
  final String appId;

  /// `POST <endpoint>/v1/events`. If null, events go only to [onEvent]/debug.
  final String? endpoint;

  /// Bearer token sent as `Authorization: Bearer <apiKey>` when set.
  final String? apiKey;

  /// User-visible application version stamped into `ctx.build.version`.
  final String? buildVersion;

  /// Source revision stamped into `ctx.build.commit`.
  final String? buildCommit;

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
    this.buildVersion,
    this.buildCommit,
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
  _Snapshot(this.sig, this.labels);
}

/// Global-coordinate geometry for an explicitly owned indicator.
class ReproItIndicatorGeometry {
  final Rect indicator;
  final Rect owner;
  final Rect container;
  final bool animating;
  final bool transformsResolved;
  const ReproItIndicatorGeometry(
      {required this.indicator,
      required this.owner,
      required this.container,
      this.animating = false,
      this.transformsResolved = true});
}

class ReproItFocusObservation {
  final String key;
  final bool focusedEditable,
      exactKeyboardRect,
      animating,
      transformsResolved,
      intentionalHiddenEditor,
      systemUi;
  final Rect field, usableViewport;
  const ReproItFocusObservation(
      {required this.key,
      required this.focusedEditable,
      required this.field,
      required this.usableViewport,
      required this.exactKeyboardRect,
      this.animating = false,
      this.transformsResolved = true,
      this.intentionalHiddenEditor = false,
      this.systemUi = false});
}

class _FocusContract {
  final ReproItFocusObservation? Function() sample;
  final bool Function() reveal;
  const _FocusContract(this.sample, this.reveal);
}

enum ReproItContractStatus { violation, satisfied, abstain }

enum ReproItStateBoundary {
  rotation,
  backgroundForeground,
  navigationRoundTrip,
  processRecreation
}

enum ReproItBoundaryPhase { before, after }

class ReproItStructuralObservation {
  final String key, state;
  final bool authoritative, settled;
  const ReproItStructuralObservation(
      {required this.key,
      required this.state,
      required this.authoritative,
      required this.settled});
}

class ReproItContractResult {
  final ReproItContractStatus status;
  final String id;
  final String? message;
  const ReproItContractResult(this.status, this.id, [this.message]);
}

class ReproItStatePreservationContract {
  final Set<ReproItStateBoundary> boundaries;
  final ReproItStructuralObservation? Function() sample;
  final bool Function(ReproItStateBoundary, ReproItStructuralObservation)?
      saveBaseline;
  final ReproItStructuralObservation? Function(ReproItStateBoundary)?
      loadBaseline;
  const ReproItStatePreservationContract(
      {required this.boundaries,
      required this.sample,
      this.saveBaseline,
      this.loadBaseline});
}

class ReproItActionEffectObservation {
  final String? route, state;
  final bool authoritative, settled;
  const ReproItActionEffectObservation(
      {this.route,
      this.state,
      required this.authoritative,
      required this.settled});
}

class ReproItTargetEffect {
  final String target;
  const ReproItTargetEffect(this.target);
}

class ReproItChangeEffect {
  final String? target;
  final bool? changed;
  const ReproItChangeEffect({this.target, this.changed});
}

class ReproItActionEffectContract {
  final ReproItActionEffectObservation? Function() sample;
  final ReproItTargetEffect? route;
  final ReproItChangeEffect? state;
  const ReproItActionEffectContract(
      {required this.sample, this.route, this.state});
}

class _IndicatorContract {
  final String dependentKey, ownerKey, containerKey;
  final double maxGap;
  final ReproItIndicatorGeometry? Function() sample;
  const _IndicatorContract(this.dependentKey, this.ownerKey, this.containerKey,
      this.maxGap, this.sample);
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
  /// Fingerprint schema version for the byte/script/combining/zero-width/
  /// newline/edge-whitespace features below.
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
    final edgeWs = units.isNotEmpty && (isWs(units.first) || isWs(units.last));
    return <String, Object>{
      'len': len,
      'bytes': utf8.encode(value).length,
      'graphemes': _graphemeCount(runes),
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

  static bool _isCombiningCp(int c) =>
      (c >= 0x0300 && c <= 0x036f) ||
      (c >= 0x1ab0 && c <= 0x1aff) ||
      (c >= 0x1dc0 && c <= 0x1dff) ||
      (c >= 0x20d0 && c <= 0x20ff) ||
      (c >= 0xfe20 && c <= 0xfe2f);

  static int _graphemeCount(List<int> runes) {
    var n = 0;
    var joined = false;
    for (final c in runes) {
      if (c == 0x200d) {
        joined = true;
        continue;
      }
      if (_isCombiningCp(c) || (c >= 0xfe00 && c <= 0xfe0f)) continue;
      if (joined) {
        joined = false;
        continue;
      }
      n += 1;
    }
    return n;
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
          (c >= 0xfe70 && c <= 0xfeff)) {
        // Arabic presentation forms-B
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
          c == 0x200d) {
        // zero-width joiner (emoji sequences)
        return true;
      }
    }
    return false;
  }
}

class _Step {
  final String sig;
  final String action;
  final String? label;
  _Step(this.sig, this.action, [this.label]);
  Map<String, dynamic> toJson() => {
        'sig': sig,
        'action': action,
        if (label != null) 'label': label,
      };
}

class _PendingStep {
  final String action;
  final String? label;
  _PendingStep(this.action, [this.label]);
  _Step toStep(String sig, bool redactLabels) =>
      _Step(sig, action, redactLabels ? null : label);
}

class _TapTarget {
  final String selector;
  final String? label;
  _TapTarget(this.selector, this.label);
}

/// Result of an app-invariant predicate registered with [ReproIt.invariant].
///
/// Return one of these when you want to attach a failure [message]; a bare
/// `bool` (or any truthy value) also works: truthy / true means the invariant
/// HELD, false / null / a thrown error means it was VIOLATED. Mirrors the web
/// SDK's `{ ok, message }` object.
class InvariantResult {
  /// True when the invariant held; false marks it violated.
  final bool ok;

  /// Human-readable reason it failed (folded into the finding); "" when held.
  final String message;

  /// Held with no message.
  const InvariantResult.ok()
      : ok = true,
        message = '';

  /// Violated, with the failure [message].
  const InvariantResult.violated(this.message) : ok = false;
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
  _PendingStep? _pendingStep; // derived at tap time from a semantics hit-test
  String? _anchor; // current screen anchor (route name), prefixes the signature
  bool _disposed = false;
  int _causalActionIndex = 0;

  /// Zero-config start: the one-line quickstart. Begins telemetry with sensible
  /// defaults and no required configuration, then delegates to [init]. Enabled
  /// only in a debug/profile build; a no-op in release ([kReleaseMode]) unless
  /// [enableInRelease] is set, so shipping this one line does nothing in a
  /// release build by default. [appId] defaults to `'app'` when omitted (Flutter
  /// has no synchronous package id without a plugin); pass [appId], or use [init]
  /// with an explicit [ReproItConfig], to override any field.
  static void start({String? appId, bool enableInRelease = false}) {
    if (kReleaseMode && !enableInRelease) return;
    init(ReproItConfig(appId: appId ?? 'app'));
  }

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

  /// Initialize ReproIt and install automatic `package:http` causal
  /// capture/fail-closed replay for every default Client created in [body].
  static R run<R>(ReproItConfig config, R Function() body) {
    init(config);
    const causal = bool.fromEnvironment('REPROIT_CAUSAL');
    if (!causal) return body();
    return http.runWithClient(
      body,
      () => ReproItCausalClient.fromEnvironment(
        actionIndex: () => _i?._causalActionIndex ?? 0,
      ),
    );
  }

  /// Add to `MaterialApp.navigatorObservers` to label route transitions as
  /// `nav:<routeName>`; optional (transitions are captured without it too).
  static NavigatorObserver get navigatorObserver => _ReproItNavObserver();

  /// Flush queued events immediately (e.g. before a known teardown).
  static Future<void> flush() => _i?._flush() ?? Future.value();

  /// Capture the current structural state as a tester-observed bug.
  static bool captureBug() => _i?._captureBug() ?? false;

  /// The current context dimensions sent with each batch (read-only view).
  @visibleForTesting
  static Map<String, Object?> get context =>
      Map.unmodifiable(_i?._context ?? const {});

  /// PII-safe fingerprints of the on-screen text fields right now (the same set
  /// attached to an error event's `context.fingerprint`). Exposed for tests so
  /// they can assert the privacy contract (e.g. obscured/password fields are
  /// skipped entirely). Returns an empty list when uninitialized.
  @visibleForTesting
  static List<Map<String, Object>> collectFieldFingerprints() =>
      _i?._collectFields() ?? const [];

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

  // ---- app invariants ------------------------------------------------------

  /// App-declared invariants: predicates that must hold in EVERY visited state.
  /// SDK-owned and STATIC so registration works before [init] and survives it
  /// (mirrors the web SDK's stable `window.__reproit_invariants` global). A Dart
  /// map preserves insertion order and keeps an existing key's position when the
  /// value is replaced, so this is idempotent by id. INERT in production: the
  /// predicates are stored but only evaluated under the reproit fuzzer (see
  /// [_maybeEmitInvariants]), so registration is zero-overhead.
  static final Map<String, Object? Function()> _invariants =
      <String, Object? Function()>{};
  static final Map<String, _IndicatorContract> _indicatorContracts = {};
  static final Map<String, String> _indicatorPrior = {};
  static final Map<String, int> _indicatorCounts = {};
  static Timer? _indicatorRetry;
  static final Map<String, _FocusContract> _focusContracts = {};
  static final Set<String> _focusAttempted = {};
  static final Map<String, String> _focusPrior = {};
  static final Map<String, int> _focusCounts = {};
  static final Map<String, ReproItStatePreservationContract> _stateContracts =
      {};
  static final Map<String, ReproItStructuralObservation> _stateBaselines = {};
  static final Map<String, ReproItActionEffectContract> _actionContracts = {};
  static final Map<String, ReproItActionEffectObservation> _actionBefore = {};
  static void focusedInput(String id,
      {required ReproItFocusObservation? Function() sample,
      required bool Function() reveal}) {
    if (id.isNotEmpty) _focusContracts[id] = _FocusContract(sample, reveal);
  }

  static void preserveState(
      String id, ReproItStatePreservationContract contract) {
    if (id.isNotEmpty && contract.boundaries.isNotEmpty) {
      _stateContracts[id] = contract;
    }
  }

  static List<ReproItContractResult> stateBoundary(
      ReproItStateBoundary kind, ReproItBoundaryPhase phase) {
    final out = <ReproItContractResult>[];
    final ids = _stateContracts.keys.toList()..sort();
    for (final id in ids) {
      final c = _stateContracts[id]!;
      if (!c.boundaries.contains(kind)) continue;
      final wire = _boundaryWire(kind);
      final identity = 'state-preservation:$wire:$id';
      final key = '$wire:$id';
      if (phase == ReproItBoundaryPhase.before) {
        final value = _sampleState(c.sample);
        if (!_validState(value)) {
          out.add(_unknown(identity));
          continue;
        }
        _stateBaselines[key] = value!;
        if (kind == ReproItStateBoundary.processRecreation &&
            (c.saveBaseline == null ||
                _safeBool(() => c.saveBaseline!(kind, value)) != true)) {
          _stateBaselines.remove(key);
          out.add(_unknown(identity));
        } else {
          out.add(_validResult(identity));
        }
        continue;
      }
      final before = kind == ReproItStateBoundary.processRecreation
          ? (c.loadBaseline == null
              ? null
              : _sampleState(() => c.loadBaseline!(kind)))
          : _stateBaselines[key];
      final after = _sampleState(c.sample);
      _stateBaselines.remove(key);
      if (!_validState(before) || !_validState(after)) {
        out.add(_unknown(identity));
      } else if (before!.key == after!.key && before.state == after.state) {
        out.add(_validResult(identity));
      } else {
        out.add(_proven(identity,
            'declared structural state was not preserved across $wire'));
      }
    }
    _publishContracts(out);
    return out;
  }

  static void actionEffect(String id, ReproItActionEffectContract contract) {
    if (id.isNotEmpty) _actionContracts[id] = contract;
  }

  static List<ReproItContractResult> actionBegin(String id) {
    final c = _actionContracts[id];
    final value = c == null ? null : _sampleAction(c.sample);
    final out = !_validAction(value)
        ? [_unknown('action-effect:$id')]
        : [_validResult('action-effect:$id')];
    if (_validAction(value)) _actionBefore[id] = value!;
    _publishContracts(out);
    return out;
  }

  static List<ReproItContractResult> actionEnd(String id) {
    final c = _actionContracts[id];
    final before = _actionBefore.remove(id);
    final after = c == null ? null : _sampleAction(c.sample);
    if (c == null || !_validAction(before) || !_validAction(after)) {
      final out = [_unknown('action-effect:$id')];
      _publishContracts(out);
      return out;
    }
    final out = <ReproItContractResult>[];
    if (c.route != null) {
      _checkTarget(out, id, 'route', c.route!.target, after!.route);
    }
    if (c.state != null) {
      _checkChange(out, id, 'state', c.state!, before!.state, after!.state);
    }
    if (out.isEmpty) out.add(_unknown('action-effect:$id'));
    _publishContracts(out);
    return out;
  }

  static void _publishContracts(List<ReproItContractResult> results) {
    final marker = _contractMarker(results);
    if (marker == null) return;
    final path = invchan.invariantFilePath();
    if (path != null) {
      invchan.appendInvariantLine(path, marker);
    } else {
      for (final result
          in results.where((r) => r.status == ReproItContractStatus.violation)) {
        _i?._captureContractBug(result);
      }
    }
  }

  static String _boundaryWire(ReproItStateBoundary kind) {
    switch (kind) {
      case ReproItStateBoundary.rotation:
        return 'rotation';
      case ReproItStateBoundary.backgroundForeground:
        return 'background-foreground';
      case ReproItStateBoundary.navigationRoundTrip:
        return 'navigation-round-trip';
      case ReproItStateBoundary.processRecreation:
        return 'process-recreation';
    }
  }

  static ReproItStructuralObservation? _sampleState(
      ReproItStructuralObservation? Function() f) {
    try {
      return f();
    } catch (_) {
      return null;
    }
  }

  static ReproItActionEffectObservation? _sampleAction(
      ReproItActionEffectObservation? Function() f) {
    try {
      return f();
    } catch (_) {
      return null;
    }
  }

  static bool _safeBool(bool Function() f) {
    try {
      return f();
    } catch (_) {
      return false;
    }
  }

  static bool _validState(ReproItStructuralObservation? o) =>
      o != null &&
      o.authoritative &&
      o.settled &&
      o.key.isNotEmpty &&
      o.state.isNotEmpty;
  static bool _validAction(ReproItActionEffectObservation? o) =>
      o != null && o.authoritative && o.settled;
  static ReproItContractResult _unknown(String id) =>
      ReproItContractResult(ReproItContractStatus.abstain, id);
  static ReproItContractResult _validResult(String id) =>
      ReproItContractResult(ReproItContractStatus.satisfied, id);
  static ReproItContractResult _proven(String id, String message) =>
      ReproItContractResult(ReproItContractStatus.violation, id, message);

  static void _checkTarget(List<ReproItContractResult> out, String id,
      String kind, String target, String? after) {
    final identity = 'action-effect:$id:$kind';
    out.add(target.isEmpty || after == null
        ? _unknown(identity)
        : after == target
            ? _validResult(identity)
            : _proven(identity, 'declared $kind effect did not occur'));
  }

  static void _checkChange(List<ReproItContractResult> out, String id,
      String kind, ReproItChangeEffect effect, String? before, String? after) {
    final identity = 'action-effect:$id:$kind';
    if (after == null ||
        (effect.target == null && (effect.changed == null || before == null))) {
      out.add(_unknown(identity));
      return;
    }
    final ok = effect.target != null
        ? after == effect.target
        : (after != before) == effect.changed;
    out.add(ok
        ? _validResult(identity)
        : _proven(identity, 'declared $kind effect did not occur'));
  }

  static String? _contractMarker(List<ReproItContractResult> results) {
    final items = results
        .where((r) => r.status == ReproItContractStatus.violation)
        .map((r) => {'id': r.id, 'message': r.message ?? r.id})
        .toList();
    return items.isEmpty
        ? null
        : 'REPROIT_INVARIANT ${jsonEncode({'sig': '', 'items': items})}';
  }

  @visibleForTesting
  static void debugClearStructuralContracts() {
    _stateContracts.clear();
    _stateBaselines.clear();
    _actionContracts.clear();
    _actionBefore.clear();
  }

  static String? _focusMarker() {
    final items = <Map<String, String>>[];
    for (final id in _focusContracts.keys.toList()..sort()) {
      final c = _focusContracts[id]!;
      ReproItFocusObservation? o;
      try {
        o = c.sample();
      } catch (_) {
        o = null;
      }
      final valid = o != null &&
          o.key.isNotEmpty &&
          o.focusedEditable &&
          o.exactKeyboardRect &&
          !o.animating &&
          o.transformsResolved &&
          !o.intentionalHiddenEditor &&
          !o.systemUi &&
          <double>[
            o.field.left,
            o.field.top,
            o.field.width,
            o.field.height,
            o.usableViewport.left,
            o.usableViewport.top,
            o.usableViewport.width,
            o.usableViewport.height
          ].every((value) => value.isFinite) &&
          o.field.width > 0 &&
          o.field.height > 0 &&
          o.usableViewport.width > 0 &&
          o.usableViewport.height > 0;
      if (!valid) {
        _focusAttempted.remove(id);
        _focusPrior.remove(id);
        _focusCounts.remove(id);
        continue;
      }
      if (o.field.overlaps(o.usableViewport)) {
        _focusAttempted.remove(id);
        _focusPrior.remove(id);
        _focusCounts.remove(id);
        continue;
      }
      if (!_focusAttempted.contains(id)) {
        bool safe = false;
        try {
          safe = c.reveal();
        } catch (_) {}
        if (!safe) continue;
        _focusAttempted.add(id);
        continue;
      }
      final fp = [o.field, o.usableViewport]
          .expand((r) => [r.left, r.top, r.width, r.height])
          .map((v) => (v * 2).round())
          .join(',');
      final n = _focusPrior[id] == fp ? (_focusCounts[id] ?? 0) + 1 : 1;
      _focusPrior[id] = fp;
      _focusCounts[id] = n;
      if (n >= 2)
        items.add({
          'id': 'focused-input-obscured:${o.key}',
          'message': 'focused editable has no usable visible rectangle after '
              'its owning scroll container attempted reveal'
        });
    }
    return items.isEmpty
        ? null
        : 'REPROIT_INVARIANT ${jsonEncode({'sig': '', 'items': items})}';
  }

  @visibleForTesting
  static String? debugFocusMarker() => _focusMarker();

  @visibleForTesting
  static void debugClearFocusedInputs() {
    _focusContracts.clear();
    _focusAttempted.clear();
    _focusPrior.clear();
    _focusCounts.clear();
  }

  /// Declare an indicator's semantic owner and container. The callback returns
  /// global rectangles, normally from `RenderBox.localToGlobal`. ReproIt waits
  /// for two identical settled samples and abstains while animated or unresolved.
  static void indicator(String id,
      {required String dependentKey,
      required String ownerKey,
      required String containerKey,
      double maxGap = 8,
      required ReproItIndicatorGeometry? Function() sample}) {
    if (id.isEmpty ||
        dependentKey.isEmpty ||
        ownerKey.isEmpty ||
        containerKey.isEmpty ||
        !maxGap.isFinite ||
        maxGap < 0) return;
    _indicatorContracts[id] = _IndicatorContract(
        dependentKey, ownerKey, containerKey, maxGap, sample);
  }

  static String? _relationMarker() {
    final checks = <Map<String, Object>>[];
    for (final id in _indicatorContracts.keys.toList()..sort()) {
      final c = _indicatorContracts[id]!;
      ReproItIndicatorGeometry? g;
      try {
        g = c.sample();
      } catch (_) {
        g = null;
      }
      var outcome = 'ABSTAIN';
      String? violation;
      var fp = 'ABSTAIN';
      bool validRect(Rect r) =>
          r.left.isFinite &&
          r.top.isFinite &&
          r.width.isFinite &&
          r.height.isFinite &&
          r.width > 0 &&
          r.height > 0;
      if (g != null &&
          !g.animating &&
          g.transformsResolved &&
          validRect(g.indicator) &&
          validRect(g.owner) &&
          validRect(g.container)) {
        final i = g.indicator, o = g.owner, box = g.container;
        final escaped = i.left < box.left - .5 ||
            i.top < box.top - .5 ||
            i.right > box.right + .5 ||
            i.bottom > box.bottom + .5;
        final dx = max(0.0, max(o.left - i.right, i.left - o.right));
        final dy = max(0.0, max(o.top - i.bottom, i.top - o.bottom));
        final detached = sqrt(dx * dx + dy * dy) > c.maxGap + .5;
        violation =
            escaped ? 'escaped-container' : (detached ? 'detached' : null);
        outcome = violation == null ? 'SATISFIED' : 'VIOLATION';
        fp = [i, o, box]
                .expand((r) => [r.left, r.top, r.width, r.height])
                .map((v) => (v * 2).round())
                .join(',') +
            '|${violation ?? 'valid'}';
      }
      final count =
          _indicatorPrior[id] == fp ? (_indicatorCounts[id] ?? 0) + 1 : 1;
      _indicatorPrior[id] = fp;
      _indicatorCounts[id] = count;
      if (count < 2) continue;
      checks.add(<String, Object>{
        'kind': 'indicator-anchor',
        'dependentKey': c.dependentKey,
        'ownerKey': c.ownerKey,
        'containerKey': c.containerKey,
        'outcome': outcome,
        if (violation != null) 'violation': violation
      });
    }
    if (checks.isEmpty) return null;
    return 'REPROIT_RELATION ${jsonEncode({
          'stableSamples': 2,
          'checks': checks
        })}';
  }

  @visibleForTesting
  static String? debugIndicatorMarker() => _relationMarker();

  @visibleForTesting
  static void debugClearIndicators() {
    _indicatorContracts.clear();
    _indicatorPrior.clear();
    _indicatorCounts.clear();
    _indicatorRetry?.cancel();
    _indicatorRetry = null;
  }

  static void _maybeEmitRelations() {
    if (_indicatorContracts.isEmpty && _focusContracts.isEmpty) return;
    final path = invchan.invariantFilePath();
    if (path == null) return;
    final marker = _relationMarker();
    final focusMarker = _focusMarker();
    if (focusMarker != null) invchan.appendInvariantLine(path, focusMarker);
    if (marker != null) {
      invchan.appendInvariantLine(path, marker);
    }
    if (_indicatorRetry == null &&
        (marker == null || _focusContracts.isNotEmpty)) {
      final retryRelation = marker == null;
      _indicatorRetry = Timer(const Duration(milliseconds: 50), () {
        _indicatorRetry = null;
        if (retryRelation) {
          final confirmed = _relationMarker();
          if (confirmed != null) invchan.appendInvariantLine(path, confirmed);
        }
        final focusConfirmed = _focusMarker();
        if (focusConfirmed != null) {
          invchan.appendInvariantLine(path, focusConfirmed);
        } else {
          Timer(const Duration(milliseconds: 50), () {
            final finalFocus = _focusMarker();
            if (finalFocus != null)
              invchan.appendInvariantLine(path, finalFocus);
          });
        }
      });
    }
  }

  /// Register an app invariant: a predicate that must hold in EVERY visited
  /// state (a running total never negative, the selected tab always
  /// highlighted). [test] returns truthy / `true` / [InvariantResult.ok] when
  /// it holds, or `false` / `null` / a thrown error / [InvariantResult.violated]
  /// when it is VIOLATED (a thrown error's text, or the result's message,
  /// becomes the finding message). Registration is idempotent by [id]
  /// (re-registering replaces) and INERT in production: the predicate is stored
  /// but only evaluated when the SDK detects it is running under the reproit
  /// fuzzer, so this is zero-overhead until a run reproduces it. Under the
  /// fuzzer a violated invariant is reported as an `invariant` finding. Mirrors
  /// the web SDK's `ReproIt.invariant`.
  static void invariant(String id, Object? Function() test) {
    _invariants[id] = test;
  }

  /// Evaluate every registered invariant; return one `{id,message}` entry per
  /// VIOLATED invariant (held ones omitted). Each predicate is isolated in a
  /// try/catch so one throwing predicate cannot suppress the others. Does NOT
  /// apply the fuzzer gate (that lives in [_maybeEmitInvariants]); exposed for
  /// host tests.
  @visibleForTesting
  static List<Map<String, String>> evaluateInvariants() {
    final out = <Map<String, String>>[];
    _invariants.forEach((id, test) {
      var ok = true;
      var message = '';
      try {
        final r = test();
        if (r is InvariantResult) {
          ok = r.ok;
          message = r.message;
        } else if (r == null || r == false) {
          ok = false;
        }
      } catch (e) {
        ok = false;
        message = e.toString();
      }
      if (!ok) out.add(<String, String>{'id': id, 'message': message});
    });
    return out;
  }

  /// The `REPROIT_INVARIANT` marker line for the current violations, or null
  /// when none are violated (silent). The sig is left empty (""); the explorer
  /// substitutes the state signature it is currently on.
  static String? _invariantMarker() {
    final items = evaluateInvariants();
    if (items.isEmpty) return null;
    return 'REPROIT_INVARIANT ${jsonEncode(<String, Object>{
          'sig': '',
          'items': items,
        })}';
  }

  /// Under the reproit fuzzer ONLY, evaluate the registered invariants and
  /// APPEND any violations to the runner-provisioned marker file. The gate is
  /// the presence of `REPROIT_INVARIANT_FILE` (set by the Flutter backend), so
  /// production, with no such file, never evaluates a predicate. A no-op on web.
  static void _maybeEmitInvariants() {
    if (_invariants.isEmpty) return;
    final path = invchan.invariantFilePath();
    if (path == null) return;
    final marker = _invariantMarker();
    if (marker == null) return;
    invchan.appendInvariantLine(path, marker);
  }

  /// Test hook: evaluate the registered invariants and append any violations to
  /// [path], exercising the real evaluate + format + file-append path without
  /// the `REPROIT_INVARIANT_FILE` env gate. Returns the marker line written, or
  /// null when nothing was violated.
  @visibleForTesting
  static String? debugEmitInvariantsTo(String path) {
    final marker = _invariantMarker();
    if (marker == null) return null;
    invchan.appendInvariantLine(path, marker);
    return marker;
  }

  /// Test hook: clear the invariant registry (tests should not leak predicates
  /// into each other, since the registry is process-static).
  @visibleForTesting
  static void debugClearInvariants() => _invariants.clear();

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
      if ((_cfg.buildVersion ?? '').isNotEmpty ||
          (_cfg.buildCommit ?? '').isNotEmpty)
        'build': <String, String>{
          if ((_cfg.buildVersion ?? '').isNotEmpty)
            'version': _cfg.buildVersion!,
          if ((_cfg.buildCommit ?? '').isNotEmpty) 'commit': _cfg.buildCommit!,
        },
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
      } else if (t.contains('Button') ||
          t.contains('Chip') ||
          t.contains('Tab')) {
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
      final id =
          (roleIds != null && idx < roleIds.length) ? roleIds[idx] : null;
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
    var any = false;
    _walk((d, _) {
      any = true;
      final label = _labelOf(d);
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
    return _Snapshot(sig, unique.take(_cfg.maxLabels).toList());
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
  /// Obscured fields (`obscureText`, e.g. passwords) are skipped entirely: they
  /// are flagged [SemanticsFlag.isObscured] in the semantics tree, and we never
  /// fingerprint or read the value of such a node. This matches the privacy
  /// contract in docs/data-handling.md ("Password and hidden fields ... are never
  /// read at all, not even to fingerprint them") and the Web/RN SDKs, which skip
  /// password fields. Even the masked form (which would still leak the real
  /// length and the field's identity) is never captured. Fields with no value
  /// contribute `isEmpty:true`.
  List<Map<String, Object>> _collectFields() {
    final out = <Map<String, Object>>[];
    var index = 0;
    _walk((d, _) {
      if (!d.hasFlag(SemanticsFlag.isTextField)) return;
      // Never read or fingerprint obscured (password) fields.
      if (d.hasFlag(SemanticsFlag.isObscured)) return;
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

  /// The structural selector and accessible name of the deepest tappable node under [point].
  /// [point] is a pointer position in logical pixels; the semantics tree is in
  /// physical pixels, so scale by devicePixelRatio before hit-testing.
  _TapTarget? _tapTargetAt(Offset point) {
    final dpr = WidgetsBinding
            .instance.platformDispatcher.implicitView?.devicePixelRatio ??
        1.0;
    final p = point * dpr;
    final keyedByRole = _keyedIdsByRole();
    final perRole = <String, int>{};
    _TapTarget? best;
    _walk((d, rect) {
      final role = roleFromSemantics(d);
      final tappable = _isTappable(d);
      final idx = tappable ? (perRole[role] ?? 0) : -1;
      if (tappable) perRole[role] = idx + 1;
      if (!rect.contains(p)) return;
      if (!tappable) return;
      final label = _labelOf(d);
      final roleIds = keyedByRole[role];
      final id = (roleIds != null && idx >= 0 && idx < roleIds.length)
          ? roleIds[idx]
          : null;
      best = _TapTarget(
        id != null ? 'key:$id' : 'role:$role#$idx',
        label.isEmpty ? null : _clipLabel(label),
      ); // deepest wins
    });
    return best;
  }

  // ---- event capture -------------------------------------------------------

  void _onPointer(PointerEvent e) {
    if (_disposed) return;
    if (e is PointerDownEvent) {
      _causalActionIndex++;
      final target = _tapTargetAt(e.position);
      _pendingStep = _PendingStep(
        target != null ? 'tap:${target.selector}' : 'tap:?',
        target?.label,
      );
    }
  }

  void _onRoute(String? routeName) {
    _causalActionIndex++;
    // Prefer an explicit nav action over a stale tap if a route just changed.
    _pendingStep = _PendingStep(
      routeName != null && routeName.isNotEmpty ? 'nav:$routeName' : 'nav',
    );
    // The route name is the screen anchor: it prefixes the structural signature
    // (docs/signature.md "Anchor short-circuit semantics"). A null/empty name
    // leaves the anchor empty, which still emits the `A:` prefix line.
    if (routeName != null && routeName.isNotEmpty) _anchor = routeName;
  }

  void _maybeSnapshot() {
    if (_disposed) return;
    final snap = _snapshot();
    if (snap == null) return;
    // App-invariant channel: under the fuzzer, append any violated predicates
    // to REPROIT_INVARIANT_FILE for the explorer to scrape. Runs on every
    // settle (independent of whether the signature changed); inert in
    // production (no such file), a no-op on web.
    _maybeEmitInvariants();
    _maybeEmitRelations();
    if (_currentSig == null) {
      // initial state
      _currentSig = snap.sig;
      _emitEdge(from: null, action: 'load', to: snap, append: true);
      return;
    }
    if (snap.sig == _currentSig) return;
    final step = _pendingStep ?? _PendingStep('auto');
    _pendingStep = null;
    _emitEdgeStep(from: _currentSig, step: step, to: snap, append: true);
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

  void _emitEdgeStep({
    required String? from,
    required _PendingStep step,
    required _Snapshot to,
    required bool append,
  }) {
    if (append) {
      _path.add(step.toStep(from ?? '', _cfg.redactLabels));
      if (_path.length > _cfg.pathCap) _path.removeAt(0);
    }
    final ev = <String, dynamic>{
      'kind': 'edge',
      if (from != null) 'from': from,
      'action': step.action,
      if (!_cfg.redactLabels && step.label != null) 'label': step.label,
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
        : stack
            .toString()
            .split('\n')
            .where((l) => l.trim().isNotEmpty)
            .take(8)
            .toList();
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
      // A genuine uncaught error IS the `crash` oracle firing; tag it so the
      // cloud can gate ingest on oracle-grade findings.
      'oracle': 'crash',
      'sig': _currentSig ?? '',
      // Include the in-flight action: a tap whose handler throws synchronously
      // (the crashing tap) sets `_pendingStep` but crashes before its debounced
      // snapshot records it, so the bare path stops one step short of the bug.
      // Append it so the captured path contains the step that actually crashes.
      'path': <Map<String, dynamic>>[
        ..._path.map((s) => s.toJson()),
        if (_pendingStep != null)
          _pendingStep!.toStep(_currentSig ?? '', _cfg.redactLabels).toJson(),
      ],
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

  bool _captureBug() {
    if (_disposed) return false;
    final snap = _snapshot();
    if (snap == null) return false;
    if (_currentSig == null) {
      _currentSig = snap.sig;
      _path.add(_Step(snap.sig, 'load'));
    } else if (_currentSig != snap.sig) {
      final step = _pendingStep ?? _PendingStep('auto');
      _path.add(step.toStep(_currentSig!, _cfg.redactLabels));
      _currentSig = snap.sig;
      _pendingStep = null;
    }
    if (_path.length > _cfg.pathCap) {
      _path.removeRange(0, _path.length - _cfg.pathCap);
    }
    final trigger = _path.isEmpty ? 'load' : _path.last.action;
    final ev = <String, dynamic>{
      'kind': 'error',
      'oracle': 'tester-capture',
      'sig': snap.sig,
      'path': _path.map((s) => s.toJson()).toList(),
      'message': 'Tester observed a bug in this state',
      'findingIdentity': {
        'oracle': 'tester-capture',
        'invariant': 'tester-observed-failure',
        'kind': 'structural-state',
        'message': '',
        'frame': '',
        'trigger': trigger,
        'boundary': snap.sig,
      },
      't': DateTime.now().millisecondsSinceEpoch,
    };
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
    scheduleMicrotask(_flush);
    return true;
  }

  bool _captureContractBug(ReproItContractResult result) {
    if (_disposed || result.status != ReproItContractStatus.violation)
      return false;
    final snap = _snapshot();
    if (snap == null) return false;
    if (_currentSig == null) {
      _currentSig = snap.sig;
      _path.add(_Step(snap.sig, 'load'));
    } else if (_currentSig != snap.sig) {
      final step = _pendingStep ?? _PendingStep('auto');
      _path.add(step.toStep(_currentSig!, _cfg.redactLabels));
      _currentSig = snap.sig;
      _pendingStep = null;
    }
    if (_path.length > _cfg.pathCap) {
      _path.removeRange(0, _path.length - _cfg.pathCap);
    }
    final trigger = _path.isEmpty ? 'load' : _path.last.action;
    _enqueue({
      'kind': 'error',
      'oracle': 'invariant',
      'sig': snap.sig,
      'path': _path.map((s) => s.toJson()).toList(),
      'message': result.message ?? result.id,
      'findingIdentity': {
        'oracle': 'invariant',
        'invariant': result.id,
        'kind': 'structural-contract',
        'message': result.message ?? result.id,
        'frame': '',
        'trigger': trigger,
        'boundary': snap.sig,
      },
      't': DateTime.now().millisecondsSinceEpoch,
    });
    scheduleMicrotask(_flush);
    return true;
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
