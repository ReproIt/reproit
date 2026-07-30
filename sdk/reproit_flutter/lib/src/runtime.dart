part of '../reproit_flutter.dart';

extension _ReproItRuntime on ReproIt {
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

  String _labelOf(SemanticsData d) =>
      d.label.trim().split('\n').first.trim();

  bool _isTappable(SemanticsData d) =>
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
    // Statics on ReproIt are not in scope unqualified inside this extension
    // (extension members resolve against the extension, not the on-type).
    ReproIt._maybeEmitInvariants();
    ReproIt._maybeEmitRelations();
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

  Future<void> _flush() async {
    if (_disposed || _queue.isEmpty) return;
    final endpoint = _cfg.endpoint;
    if (endpoint == null) {
      _queue.clear();
      return;
    }
    final batch = _queue.toList();
    _queue.clear();
    final sentAt = DateTime.now().millisecondsSinceEpoch;
    _batchSequence += 1;
    final batchId = 'sdk-$sentAt-$_batchSequence';
    final body = jsonEncode({
      'version': 1,
      'batchId': batchId,
      'appId': _cfg.appId,
      if ((_cfg.buildVersion ?? '').isNotEmpty ||
          (_cfg.buildCommit ?? '').isNotEmpty)
        'deployment': {
          if ((_cfg.buildVersion ?? '').isNotEmpty)
            'version': _cfg.buildVersion!,
          if ((_cfg.buildCommit ?? '').isNotEmpty)
            'commit': _cfg.buildCommit!,
        },
      'frames': [
        for (var index = 0; index < batch.length; index += 1)
          {
            'runId': batchId,
            'sequence': index + 1,
            'scope': {'domain': 'shared'},
            'event': _protocolEvent(batch[index]),
          },
      ],
      'evidence': <Object?>[],
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

  Map<String, Object?> _protocolEvent(Map<String, dynamic> event) {
    if (event['kind'] == 'edge') {
      return {
        'kind': 'graph-edge',
        'from': event['from'] ?? '∅',
        'action': event['action'] ?? 'auto',
        'to': event['to'] ?? '?',
      };
    }

    if (event['kind'] != 'error') {
      return {'kind': 'stream-defect', 'reason': 'invalid-event'};
    }

    final path = (event['path'] as List<dynamic>? ?? const <dynamic>[]);
    final message = event['message']?.toString() ?? '';
    final identity = event['findingIdentity'] ??
        {
          'oracle': event['oracle']?.toString() ?? 'crash',
          'invariant': 'no-exception',
          'kind': 'exception',
          'message': ReproIt._structuralMessage(message),
          'frame': '',
          'trigger': path.isEmpty
              ? ''
              : (path.last as Map<String, dynamic>)['action']?.toString() ?? '',
          'boundary': null,
        };
    final eventContext = event['context'];
    return {
      'kind': 'finding',
      'signature': event['sig']?.toString() ?? '?',
      'message': message,
      'identity': identity,
      'path': [
        for (final step in path)
          {
            'signature': (step as Map<String, dynamic>)['sig'] ?? '?',
            'action': step['action'] ?? 'auto',
            'label': step['label'],
          },
      ],
      'context': {
        ..._context,
        if (eventContext is Map<String, dynamic>) ...eventContext,
      },
    };
  }

}
