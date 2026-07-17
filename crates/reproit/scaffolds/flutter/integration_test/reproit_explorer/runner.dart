part of '../reproit_explorer.dart';

void registerExplorer({
  required ExplorerRuntime runtime,
  required PumpExplorerApp pumpApp,
}) {
  _runtime = runtime;

  Future<void> settle(WidgetTester t, int ms) async {
    for (var i = 0; i < ms ~/ 100; i++) {
      await t.pump(const Duration(milliseconds: 100));
    }
  }

  testWidgets(runtime.testName, (tester) async {
    final semantics = tester.ensureSemantics();
    // Ready marker so the orchestrator starts recording promptly. In scenario
    // mode the real role is claimed from the conductor below (which prints its
    // own `claimed role=` marker), so don't assert role=a here.
    if (envBarrier.isEmpty) {
      runtime.emit('JOURNEY claimed role=a');
    }

    // Force the requested run locale BEFORE the app first pumps, so every screen
    // renders in that language. Scoped to the run: cleared in the teardown
    // below. A per-seed fuzz.locale still overrides this for that seed.
    if (envLocale.isNotEmpty) {
      applyLocale(tester, envLocale);
      runtime.emit('JOURNEY[a] step: locale=$envLocale');
    }

    // Signal "disable animations" BEFORE the app first pumps, so every screen
    // renders with animation-dependent timing pinned (capture determinism).
    // Scoped to the run: cleared in the teardown below.
    applyReducedMotion(tester);

    // PERMISSION-WALK sweep: under REPROIT_DENY_PERMISSION, mock the permission
    // channel to deny every request BEFORE the app first pumps, so a screen that
    // gates on the permission takes its denied branch. Scoped to the run (cleared
    // in teardown). observe()'s marker is gated on this flag AND on a denial
    // having actually fired.
    final permissionDeny = installPermissionDenial(tester, envDenyPermission);

    // Simulator runs collect real frame timings. The headless runtime keeps
    // this hook inert because the widget-test clock is synthetic.
    runtime.startSession(tester);

    // Last-resort: resolve a tappable by its (localized) visible text. Kept ONLY
    // for backward compatibility with old `tap:<label>` replay configs; the
    // explorer itself never emits label selectors anymore. find.byKey / the
    // role+index path below are the locale-invariant routes.
    Finder? findByLabel(String label) {
      final isClipped =
          label.length == maxLabelLen &&
          RegExp(r'#[0-9a-f]{8}$').hasMatch(label);
      if (isClipped) {
        final prefix = label.substring(0, label.lastIndexOf('#'));
        final re = RegExp('^${RegExp.escape(prefix)}');
        var f = find.bySemanticsLabel(re);
        if (f.evaluate().isNotEmpty) return f;
        f = find.textContaining(re);
        if (f.evaluate().isNotEmpty) return f;
        return null;
      }
      var f = find.bySemanticsLabel(label);
      if (f.evaluate().isNotEmpty) return f;
      f = find.bySemanticsLabel(RegExp(RegExp.escape(label)));
      if (f.evaluate().isNotEmpty) return f;
      f = find.text(label);
      if (f.evaluate().isNotEmpty) return f;
      return null;
    }

    // STRUCTURAL tap: resolve a locale-invariant selector and tap it. Returns
    // true on success.
    //   key:<keyString>   -> find.byKey (replays in ANY locale)
    //   role:<role>#<idx>  -> the idx-th tappable of that role, in document
    //                         order, tapped via the semantics action (no text)
    //   <anything else>    -> legacy label fallback (find by visible text)
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
        final root = _semanticsRoot(tester);
        if (root != null) {
          void walk(SemanticsNode n) {
            if (target != null) return;
            final d = n.getSemanticsData();
            if (!d.flagsCollection.isHidden) {
              final tappable =
                  d.hasAction(SemanticsAction.tap) &&
                  !d.flagsCollection.isTextField;
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
      // Label selector: an explicit `label:` prefix, or a bare string (legacy),
      // resolved by visible/semantic label. An ACTION selector only has to be
      // stable within the run's locale, so resolving by (localized) label is
      // fine; the state SIGNATURE stays structural and locale-invariant. This is
      // parity with fillField (already label-based) and with how Playwright/
      // Appium address by visible name. Use key:/role: to override when a label
      // is ambiguous or you want locale-proof selection.
      final label = sel.startsWith('label:')
          ? sel.substring('label:'.length)
          : sel;
      final f = findByLabel(label);
      if (f == null) return false;
      try {
        await tester.tap(f.first, warnIfMissed: false);
        return true;
      } catch (_) {
        return false;
      }
    }

    Future<bool> goBack() async {
      try {
        final nav = tester.state<NavigatorState>(find.byType(Navigator).first);
        final popped = await nav.maybePop();
        await settle(tester, 900);
        return popped;
      } catch (_) {
        return false;
      }
    }

    // Property-matched replay: type a synthesized value into the text field that
    // matches `field` (by a11y label, then by a positional "#<n>" / digit index
    // into the on-screen EditableTexts). Returns true if it filled something, so
    // the caller can mark that input done and not retype it every step.
    Future<bool> fillField(String field, String value) async {
      // 1) By semantics label (a TextField's labelText becomes its a11y label).
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
      // 2) Positional fallback: "#2" / "field2" -> the Nth ON-SCREEN field.
      // Index only VISIBLE (hit-testable) fields, so a field built but offstage
      // on another PageView/IndexedStack/Tab page can't shift the index (the bug
      // that made "first field" land on an offstage page). Same visible-only
      // discipline the tap path uses; fall back to the full set only if nothing
      // is hit-testable.
      var edits = find.byType(EditableText).hitTestable();
      if (edits.evaluate().isEmpty) {
        edits = find.byType(EditableText);
      }
      final n = edits.evaluate().length;
      final digits = field.replaceAll(RegExp(r'[^0-9]'), '');
      final idx = int.tryParse(digits);
      if (idx != null && idx < n) {
        try {
          await tester.enterText(edits.at(idx), value);
          await settle(tester, 500);
          return true;
        } catch (_) {}
      }
      return false;
    }

    // One seed's walk. Identical to the single-seed path so the determinism
    // contract is unchanged: the action SEQUENCE is fully determined by
    // (seed, fresh app build). seen/tried sets are per-seed so each seed is
    // independent. The caller re-pumps a fresh widget tree before this, so
    // intentionally-leaked state (e.g. an undisposed AnimationController) is
    // exactly what surfaces as a finding.
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
        final root = _semanticsRoot(tester);
        if (root != null) {
          void walk(SemanticsNode n) {
            final d = n.getSemanticsData();
            if (!d.flagsCollection.isHidden && roleOf(d) == wantRole) {
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
        runtime.emit(
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
        runtime.emit(
          'FUZZ:ASSERT $result count $finder want=$want got=$got actor=$who',
        );
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

      Future<Snapshot> observe() async {
        final snap = snapshotWith(tester, valueSelectors);
        final sig = effectiveSigOf(snap);
        emitJson('FUZZ:OBS', {
          "sig": sig,
          if (snap.anchor != null) "route": snap.anchor,
          "labels": snap.labels.take(maxLabelsPerState).toList(),
          "elements": roleElements(snap),
        });
        if (seenStates.add(sig)) {
          // sig: STRUCTURAL + value-state (roles + shape + keys + V: classes),
          // locale-invariant. labels: DISPLAY-ONLY visible text (map --show),
          // never in the sig. elements: structural selectors for replay; `nokey`
          // flags a tappable that has no developer key (the map layer can warn).
          emitJson('EXPLORE:STATE', {
            "sig": sig,
            if (snap.anchor != null) "route": snap.anchor,
            "labels": snap.labels.take(maxLabelsPerState).toList(),
            "elements": stateElements(snap),
          });
          // Operability/a11y ground-truth for the SAME sig: graph1 (operable) x
          // graph2 (semantics role/name) + keyboard reachability/activation.
          runtime.emit(
            'EXPLORE:GROUNDTRUTH ${jsonEncode(groundTruth(tester, sig))}',
          );
          // CONTENT-BUG for this newly-seen state, keyed by the SAME sig. Pure
          // semantics-label scan (no pixels, no timing), so it reproduces on
          // replay. Silent when no broken-content artifact is rendered.
          final cbug = detectContentBugs(tester);
          if (cbug.isNotEmpty) {
            emitJson('EXPLORE:CONTENTBUG', {
              "sig": sig,
              if (snap.anchor != null) "route": snap.anchor,
              "items": cbug,
            });
          }
          // STUCK-KEYBOARD for this newly-seen state, keyed by the SAME sig.
          // IME visibility + focus tree, both platform ground truth. Silent
          // (no marker) when the screen is clean.
          if (detectStuckKeyboard(tester)) {
            emitJson('EXPLORE:STUCKKEYBOARD', {
              "sig": sig,
              if (snap.anchor != null) "route": snap.anchor,
            });
          }
          // SAFE-AREA for this newly-seen state, keyed by the SAME sig. Pure
          // inset-vs-rect geometry in logical px (no pixels, no timing), so it
          // reproduces on replay. Silent when no control sits in a device inset
          // (and always silent on a device/test with no insets at all).
          final safeArea = detectSafeArea(tester);
          if (safeArea.isNotEmpty) {
            emitJson('EXPLORE:SAFEAREA', {
              "sig": sig,
              if (snap.anchor != null) "route": snap.anchor,
              "items": safeArea,
            });
          }
          // PERMISSION-WALK: under a denial sweep, once a permission request has
          // actually been denied, mark each newly-seen screen as reached AFTER
          // the denial. The Rust invariant fires only for a marked screen that is
          // ALSO a graph dead end, so a screen with a working exit is recorded
          // but never flagged. Silent outside a denial sweep.
          if (permissionDeny && permissionDenialSeen) {
            emitJson('EXPLORE:PERMISSIONWALK', {
              "sig": sig,
              "permission": envDenyPermission,
              if (snap.anchor != null) "route": snap.anchor,
            });
          }
          // BLANK-SCREEN for this newly-seen state, keyed by the SAME sig.
          // Fires only when the settled tree shows NOTHING (no labels, no
          // tappables, no text fields, no images) in a non-zero window; a
          // screen with ANY content stays silent, as does an unavailable
          // semantics tree.
          final blank = detectBlankScreen(tester);
          if (blank.isNotEmpty) {
            emitJson('EXPLORE:BLANKSCREEN', {
              "sig": sig,
              if (snap.anchor != null) "route": snap.anchor,
              "items": blank,
            });
          }
          // BROKEN-ASSET (tofu) for this newly-seen state, keyed by the SAME
          // sig. A rendered U+FFFD is an encoding failure leaked to the
          // screen; pure label scan, silent when every label is clean.
          final tofu = detectTofu(tester);
          if (tofu.isNotEmpty) {
            emitJson('EXPLORE:BROKENASSET', {
              "sig": sig,
              if (snap.anchor != null) "route": snap.anchor,
              "items": tofu,
            });
          }
          if (fuzz.replay == null) {
            // SCROLL ROUND-TRIP for this newly-seen state, keyed by the SAME sig.
            // Scrolls the primary list to the end and back and flags content that
            // differs at a pinned offset (a list-recycling / virtualization bug),
            // self-restoring to the list's start offset. Exploration only, so a
            // replay's action indices are not perturbed. Silent when the list is
            // stable or nothing scrolls.
            final srt = await detectScrollRoundTrip(tester);
            if (srt.isNotEmpty) {
              emitJson('EXPLORE:SCROLLROUNDTRIP', {
                "sig": sig,
                if (snap.anchor != null) "route": snap.anchor,
                "items": srt,
              });
            }
          }
        }
        // APP-INVARIANT: fold in any predicate violations the SDK appended to
        // REPROIT_INVARIANT_FILE since the last observe, attributed to THIS
        // state. Runs on every observe (not just newly-seen states) so a
        // violation that first appears on a revisit is still emitted; de-duped
        // per (sig,id) so re-settling a state does not re-emit it.
        scrapeInvariants(sig, snap.anchor);
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

      // Lifecycle-metamorphic oracles (rotation, background-restore): each
      // distinct state sig is transform-tested once. Both are self-restoring.
      final rotChecked = <String>{};
      final bgChecked = <String>{};
      // ROTATION-stability: swap the surface width/height (portrait <-> landscape
      // / split-screen), reflow, then rotate BACK to the original orientation and
      // re-observe. A correct screen reflows but rebuilds the SAME structure once
      // the original orientation is restored; an app that mishandles the metric
      // change and loses content/state that never comes back regresses the
      // STRUCTURAL signature (value-state excluded, so a clock never trips it).
      // Round-trip identity is false-positive-free (a legit OrientationBuilder
      // branch is symmetric and restores). Guarded on the pre-transform state
      // having content; self-restoring. Returns the re-observed state.
      Future<Snapshot> rotationCheck(Snapshot snap) async {
        final expected = structuralSignature(snap.anchor, snap.tree);
        final hadContent = snap.tappables.isNotEmpty;
        final view = tester.view;
        final origPhys = view.physicalSize;
        try {
          view.physicalSize = Size(origPhys.height, origPhys.width);
          await settle(tester, 400);
          view.physicalSize = origPhys;
          await settle(tester, 400);
        } catch (_) {
          try {
            view.physicalSize = origPhys;
          } catch (_) {}
        }
        final after = await observe();
        final got = structuralSignature(after.anchor, after.tree);
        if (hadContent && got != expected) {
          emitJson('EXPLORE:ROTATION', {
            "sig": sigOf(snap),
            if (snap.anchor != null) "route": snap.anchor,
            "expected": expected,
            "got": got,
          });
        }
        return after;
      }

      // BACKGROUND-RESTORE-stability: drive the app lifecycle to the background
      // (inactive -> paused) then restore it (inactive -> resumed) and re-observe.
      // A correct app returns to the SAME screen with state intact; one that drops
      // you on a different screen or loses state regresses the STRUCTURAL
      // signature. No size change; guarded on the pre-transform state having
      // content. Returns the re-observed state.
      Future<Snapshot> backgroundCheck(Snapshot snap) async {
        final expected = structuralSignature(snap.anchor, snap.tree);
        final hadContent = snap.tappables.isNotEmpty;
        try {
          // Drive the lifecycle to the background. Do NOT pump while paused:
          // in current Flutter the scheduler disables frame production when the
          // app is `hidden`/`paused`/`detached` (SchedulerBinding.framesEnabled
          // goes false), so a WidgetTester.pump() that awaits a frame never
          // completes and the walk deadlocks before the first action. The
          // lifecycle observers (didChangeAppLifecycleState) fire synchronously
          // on dispatch, so the background transition is delivered without a
          // pump; we settle only once the app is resumed and frames are enabled
          // again. This is version-robust: pumping only in a frame-enabled state
          // is always safe on older Flutter too.
          tester.binding.handleAppLifecycleStateChanged(
            AppLifecycleState.inactive,
          );
          tester.binding.handleAppLifecycleStateChanged(
            AppLifecycleState.paused,
          );
          tester.binding.handleAppLifecycleStateChanged(
            AppLifecycleState.inactive,
          );
          tester.binding.handleAppLifecycleStateChanged(
            AppLifecycleState.resumed,
          );
          await settle(tester, 600);
        } catch (_) {}
        final after = await observe();
        final got = structuralSignature(after.anchor, after.tree);
        if (hadContent && got != expected) {
          emitJson('EXPLORE:BGRESTORE', {
            "sig": sigOf(snap),
            if (snap.anchor != null) "route": snap.anchor,
            "expected": expected,
            "got": got,
          });
        }
        return after;
      }

      final rng = Rng(fuzz.seed);
      if (fuzz.seed != 0) {
        runtime.emit('JOURNEY[a] step: fuzz seed=${fuzz.seed}');
      }
      if (fuzz.replay != null) {
        runtime.emit(
          'JOURNEY[a] step: replaying ${fuzz.replay!.length} actions',
        );
      }

      // Property-matched replay: drive the locale (best-effort) and type each
      // synthesized input into its matching field as that field appears. Filled
      // once each; emits FUZZ:FILL so the reproduction is visible in the log.
      if (fuzz.locale != null && fuzz.locale!.isNotEmpty) {
        applyLocale(tester, fuzz.locale!);
        runtime.emit('JOURNEY[a] step: locale=${fuzz.locale}');
      }
      final filledFields = <String>{};
      Future<void> applyInputs() async {
        for (final inp in fuzz.inputs) {
          final field = inp['field'] ?? '';
          if (field.isEmpty || filledFields.contains(field)) continue;
          final value = inp['value'] ?? '';
          if (await fillField(field, value)) {
            filledFields.add(field);
            runtime.emit(
              'FUZZ:FILL ${jsonEncode({"field": field, "len": value.runes.length})}',
            );
          }
        }
      }

      var current = await observe();
      await applyInputs();
      var stuck = 0;
      final prefixLen = fuzz.prefix?.length ?? 0;
      final budget = fuzz.replay?.length ?? (fuzz.budget + prefixLen);
      for (var actions = 0; actions < budget && stuck < 3; actions++) {
        await applyInputs();
        // LIFECYCLE-metamorphic oracles (rotation, background-restore): once per
        // distinct state, drive a device-lifecycle transform and assert the
        // structural signature survives it. Self-restoring, so `current` is
        // refreshed to the (restored) reality; skipped in replay so a recorded
        // clip is not perturbed.
        if (fuzz.replay == null) {
          if (rotChecked.add(sigOf(current))) {
            current = await rotationCheck(current);
          }
          if (bgChecked.add(sigOf(current))) {
            current = await backgroundCheck(current);
          }
        }
        // Choose: exact replay > frontier prefix > seeded random > systematic.
        String? act;
        if (fuzz.replay != null) {
          act = fuzz.replay![actions];
        } else if (actions < prefixLen) {
          act = fuzz.prefix![actions];
        } else if (fuzz.seed != 0) {
          // Inverse-visit-count weighted pick: weight each candidate edge by
          // 1/(1+globalVisits) from the edgeWeights snapshot, plus 'back'.
          // Seeded + deterministic, so replays reproduce exactly.
          // Candidates addressed by STRUCTURAL selector (key, else role+index),
          // never by visible text, so the seeded pick and any replay are
          // locale-invariant.
          final taps = current.tappables.map((e) => e.sel).toList()..sort();
          final ew = fuzz.edgeWeights[sigOf(current)] ?? const {};
          final options = [...taps.map((s) => 'tap:$s'), 'back'];
          final weights = options
              .map(
                (o) =>
                    (fuzz.contractActions.contains(o) ? 4.0 : 1.0) /
                    (1 + (ew[o] ?? 0)),
              )
              .toList();
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

        runtime.emit('FUZZ:ACT $act');
        if (act == 'back') {
          final popped = await goBack();
          final next = await observe();
          // An edge is emitted whenever the structural+value STATE changed. The
          // stuck counter resets on any EFFECTIVE action (state OR content moved),
          // so a value-state screen (counter/calculator) does not stall the walk.
          if (popped && sigOf(next) != sigOf(current)) {
            emitJson('EXPLORE:EDGE', {
              "from": sigOf(current),
              "action": "back",
              "to": sigOf(next),
            });
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
            if (!await fillSelector(finder, value)) {
              runtime.emit('FUZZ:MISS $a');
            }
          } else if (a.startsWith('assert:')) {
            await execAssert(a.substring('assert:'.length), 'a');
          }
          // auth: is a no-op on the flutter runner (session restore unsupported).
          await settle(tester, 600);
          current = await observe();
          continue;
        }
        final sel = a.substring('tap:'.length);
        triedEdges.add('${sigOf(current)}|$sel');
        final fromSig = sigOf(current);
        final ok = await tapSelector(sel);
        if (!ok) {
          runtime.emit('FUZZ:MISS $act');
          stuck++;
          continue;
        }
        // The simulator applies the real-clock hang watchdog. Headless settles
        // with its fake clock and drains framework exceptions instead.
        final hangBucket = await runtime.settleAfterTap(tester, settle, 1200);
        if (hangBucket != null) {
          emitJson('EXPLORE:HANG', {
            "from": fromSig,
            "action": "tap:$sel",
            "bucket": hangBucket,
          });
        }
        final next = await observe();
        if (sigOf(next) != sigOf(current)) {
          emitJson('EXPLORE:EDGE', {
            "from": sigOf(current),
            "action": "tap:$sel",
            "to": sigOf(next),
          });
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

      runtime.emit('JOURNEY[a] step: explored ${seenStates.length} states');
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
      runtime.emit('JOURNEY claimed role=$role');

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
        emitJson('FUZZ:OBS', {
          "sig": snap.sig,
          if (snap.anchor != null) "route": snap.anchor,
          "labels": snap.labels.take(maxLabelsPerState).toList(),
          "elements": roleElements(snap),
        });
        if (scenarioSeen.add(snap.sig)) {
          emitJson('EXPLORE:STATE', {
            "sig": snap.sig,
            if (snap.anchor != null) "route": snap.anchor,
            "labels": snap.labels.take(maxLabelsPerState).toList(),
            "elements": stateElements(snap),
          });
          runtime.emit(
            'EXPLORE:GROUNDTRUTH ${jsonEncode(groundTruth(tester, snap.sig))}',
          );
        }
        return snap.sig;
      }

      String? lastSig = observeScenario();

      // exec() below uses the shared waitFor/textPresent/countMatching/
      // fillSelector/execAssert hoisted to the testWidgets scope (so the
      // single-actor replay loop runs the exact same verbs).
      Future<void> exec(String act) async {
        runtime.emit('FUZZ:ACT $role $act');
        if (act == 'back') {
          await goBack();
          return;
        }
        if (act.startsWith('auth:')) {
          // Session-restore login is not yet wired on the Flutter runner; use
          // `login(<account>)` (UI flow) for multi-user auth. No-op so ordering
          // still advances, but flag it loudly.
          runtime.emit(
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
            ok =
                await waitFor(() => countMatching(finder) > 0) &&
                await fillSelector(finder, value);
          }
          if (!ok) runtime.emit('FUZZ:MISS $role $act');
          return;
        }
        // default: tap:<selector>
        final sel = act.startsWith('tap:') ? act.substring('tap:'.length) : act;
        var ok = await tapSelector(sel);
        if (!ok) {
          // The target may be peer-produced and not on screen yet: retry.
          final sw = Stopwatch()..start();
          while (!ok && sw.elapsed < const Duration(seconds: 8)) {
            await Future.delayed(const Duration(milliseconds: 250));
            await tester.pump(const Duration(milliseconds: 100));
            ok = await tapSelector(sel);
          }
        }
        if (!ok) runtime.emit('FUZZ:MISS $role $act');
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
          emitJson('EXPLORE:EDGE', {
            "from": lastSig,
            "action": act == 'back' ? 'back' : act,
            "to": newSig,
          });
        }
        lastSig = newSig;
        try {
          await hit('POST', '/done?device=$role');
        } catch (_) {}
      }

      client.close();
      runtime.finishFrames();
      runtime.emit('JOURNEY DONE');
      await settle(tester, 1000);
      clearLocale(tester);
      clearReducedMotion(tester);
      clearPermissionDenial(tester);
      semantics.dispose();
      return;
    }

    // Run every seed in this session in sequence. Between seeds, re-pump a
    // FRESH widget tree so each seed starts from a clean app state and the
    // seeds stay independent. SEED:BEGIN/END boundary markers let the Rust side
    // attribute states/edges/exceptions/FUZZ:ACT per seed from the one log.
    final batch = FuzzCfg.loadBatch();
    for (final fuzz in batch) {
      runtime.emit('SEED:BEGIN ${fuzz.seed}');
      runtime.beforeSeed(tester);
      await pumpApp(tester);
      await settle(tester, runtime.seedStartupMs);
      runtime.afterFirstPump(tester);
      await runSeed(fuzz);
      await runtime.afterSeed(tester);
      runtime.emit('SEED:END ${fuzz.seed}');
    }

    runtime.finishFrames();
    runtime.emit('JOURNEY DONE');
    await runtime.afterRun(tester, settle);
    // Scope the locale override to this run only.
    clearLocale(tester);
    clearReducedMotion(tester);
    clearPermissionDenial(tester);
    semantics.dispose();
  });
}
