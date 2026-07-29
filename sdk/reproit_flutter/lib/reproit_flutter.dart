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

part 'src/models.dart';
part 'src/runtime.dart';
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
  int _batchSequence = 0;

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
      for (final result in results
          .where((r) => r.status == ReproItContractStatus.violation)) {
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

  static String signatureOfTree(String? anchor, RNode tree) =>
      signature(anchor, tree);

  /// PII-safe fingerprint of a single text value (FEATURES, never the value).
  /// Exposed for unit tests and advanced use. See [ReproItFingerprint].
  static Map<String, Object> fingerprintValue(String value) =>
      ReproItFingerprint.fingerprintValue(value);

  static String _structuralMessage(String message) => message
      .replaceAll(RegExp(r'''(["']).*?\1'''), '<q>')
      .replaceAll(RegExp(r'[0-9][0-9.,]*'), '#');

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
