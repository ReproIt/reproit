// ReproIt journey helpers. Vendored into the customer repo (next to the
// integration_test/ journeys) so generated tests run with or without ReproIt.
//
// Conventions the orchestrator relies on:
//   - `logStep` lines start with JOURNEY and are parsed into actions.jsonl.
//   - `shoot(name)` prints SHOOT:<name>; the orchestrator captures an
//     OS-level screenshot on that marker (works with any renderer, includes
//     native views).
//   - The integration test binding prints "All tests passed" or
//     "Some tests failed"; those are the done markers.

import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';

/// The role this device plays in a multi-actor journey ('a', 'b', ...).
/// Claimed from the backend so ONE build serves all concurrent devices.
String role = 'a';
bool get isA => role == 'a';

/// Ask the backend which role this device is. The dev endpoint alternates
/// roles per claim and is reset between runs (part of the reset contract).
/// Pass the same base URL the app targets.
Future<String> claimRole(String apiBaseUrl) async {
  try {
    final c = HttpClient();
    final req = await c.postUrl(Uri.parse('$apiBaseUrl/dev/claim-role'));
    final resp = await req.close();
    final body = await resp.transform(utf8.decoder).join();
    c.close();
    final claimed =
        ((jsonDecode(body) as Map<String, dynamic>)['role'] as String?) ?? 'a';
    role = claimed;
    debugPrint('JOURNEY claimed role=$role');
    return claimed;
  } catch (_) {
    debugPrint('JOURNEY claimed role=a (claim failed, defaulting)');
    return 'a';
  }
}

/// Structured action log line; parsed into the evidence bundle.
void logStep(String message) => debugPrint('JOURNEY[$role] step: $message');

/// Journey-declared completion: print as the LAST statement of a role's
/// branch. The orchestrator counts this device as done-and-passed without
/// waiting for the runner verdict (which can linger forever for observer
/// roles). An explicit test failure still overrides it.
void journeyDone() => debugPrint('JOURNEY DONE');

/// OS-level screenshot marker; the orchestrator captures on it.
Future<void> shoot(WidgetTester t, String name) async {
  await settle(t, 600);
  debugPrint('SHOOT:$name');
  await settle(t, 400); // hold still while the host captures
}

/// Bounded settle: pump for [ms] in 100ms steps. Use instead of
/// pumpAndSettle, which hangs forever on infinite animations (spinners).
Future<void> settle(WidgetTester t, int ms) async {
  for (var i = 0; i < ms ~/ 100; i++) {
    await t.pump(const Duration(milliseconds: 100));
  }
}

/// Poll for a finder to appear. The polling assertion for realtime flows:
/// generous-but-bounded, never a bare sleep.
Future<bool> waitFor(WidgetTester t, Finder f, {int timeoutMs = 15000}) async {
  for (var i = 0; i < timeoutMs ~/ 100; i++) {
    if (f.evaluate().isNotEmpty) return true;
    await t.pump(const Duration(milliseconds: 100));
  }
  return f.evaluate().isNotEmpty;
}

/// Assert that a finder appears within the bound. Fails the test (and
/// therefore the run) when it does not.
Future<void> expectEventually(
  WidgetTester t,
  Finder f, {
  int timeoutMs = 20000,
  String? reason,
}) async {
  final found = await waitFor(t, f, timeoutMs: timeoutMs);
  expect(found, isTrue, reason: reason ?? 'expected $f within ${timeoutMs}ms');
}

/// Best-effort tap: taps if present, logs and returns false otherwise.
/// Use for steps where partial footage beats an aborted run; use
/// [expectEventually] + a plain tap for load-bearing assertions.
Future<bool> tapIf(WidgetTester t, Finder f, {int timeoutMs = 15000}) async {
  if (!await waitFor(t, f, timeoutMs: timeoutMs)) {
    logStep('missing finder $f');
    return false;
  }
  await t.tap(f.first);
  await settle(t, 700);
  return true;
}
