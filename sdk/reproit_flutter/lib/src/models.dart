part of '../reproit_flutter.dart';

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
