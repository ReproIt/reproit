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
import 'dart:ui' show CheckedState, Tristate;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_test/flutter_test.dart';

// APP-SPECIFIC: import your app's root widget.
import 'package:reproit_flutter_example/operability_fixture.dart';

part 'operability_fixture_model.dart';
part 'operability_fixture_driver.dart';

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
        final root = rootSemanticsNode(tester);
        if (root != null) {
          void walk(SemanticsNode n) {
            if (target != null) return;
            final d = n.getSemanticsData();
            if (!d.flagsCollection.isHidden) {
              final tappable = d.hasAction(SemanticsAction.tap) &&
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
        final root = rootSemanticsNode(tester);
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
