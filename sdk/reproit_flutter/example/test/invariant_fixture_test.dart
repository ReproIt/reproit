// Dogfood for the APP-INVARIANT oracle (ReproIt.invariant plus the Flutter
// scaffold's parseInvariantItems/scrapeInvariants path). Exercises both
// directions live through the real file
// channel:
//   1. SDK side: ReproIt.invariant registers a predicate; the real evaluate +
//      JSON-format + file-append path (ReproIt.debugEmitInvariantsTo, the same
//      code _maybeEmitInvariants runs under the fuzzer) writes a
//      `REPROIT_INVARIANT {..}` marker line to REPROIT_INVARIANT_FILE.
//   2. Explorer side: a parity copy of the scaffold functions reads the file
//      with byte-offset tracking, parses the marker, de-dups per (sig,id), and
//      emits EXPLORE:INVARIANT for the current state.
// A VIOLATING invariant yields a marker + an EXPLORE:INVARIANT line carrying the
// app's id + message; a HOLDING one yields nothing. If the scaffold scrape
// logic changes, change the parity copy below too.
//
// This is NOT a full end-to-end `flutter drive`/`flutter test` pump: it drives
// the SDK registry+append and the explorer scrape as the two real halves joined
// by the real file, which is the pair this port adds. The env-var gate itself
// (Platform.environment['REPROIT_INVARIANT_FILE']) is a one-line resolution in
// the SDK/explorer and the Rust backend that sets it; it cannot be mutated at
// runtime, so it is validated by the backend's env injection, not here.
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:reproit_flutter/reproit_flutter.dart';

// PARITY COPY of Flutter scaffold :: parseInvariantItems.
List<Map<String, String>> parseInvariantItems(String line) {
  const mark = 'REPROIT_INVARIANT ';
  final at = line.indexOf(mark);
  if (at < 0) return const [];
  try {
    final obj = jsonDecode(line.substring(at + mark.length));
    if (obj is! Map) return const [];
    final items = obj['items'];
    if (items is! List) return const [];
    final out = <Map<String, String>>[];
    for (final it in items) {
      if (it is Map && it['id'] != null) {
        out.add(<String, String>{
          'id': '${it['id']}',
          'message': it['message'] == null ? '' : '${it['message']}',
        });
      }
    }
    return out;
  } catch (_) {
    return const [];
  }
}

// PARITY COPY of Flutter scaffold :: scrapeInvariants (byte-offset read +
// (sig,id) de-dup). Wrapped in a class so each test isolates the offset/de-dup
// state (the template keeps it in top-level vars for the single explorer run),
// and emits into [sink] instead of debugPrint so the test can assert the line.
class InvariantScraper {
  InvariantScraper(this.path);
  final String path;
  int _offset = 0;
  final Set<String> _emitted = <String>{};

  void observe(String sig, String? route, List<String> sink) {
    final file = File(path);
    if (!file.existsSync()) return;
    final len = file.lengthSync();
    if (len < _offset) _offset = 0; // truncated
    if (len <= _offset) return;
    final raf = file.openSync();
    raf.setPositionSync(_offset);
    final bytes = raf.readSync(len - _offset);
    raf.closeSync();
    _offset = len;
    final items = <Map<String, String>>[];
    for (final line in utf8.decode(bytes).split('\n')) {
      if (line.trim().isEmpty) continue;
      for (final it in parseInvariantItems(line)) {
        if (_emitted.add('$sig ${it['id']}')) items.add(it);
      }
    }
    if (items.isEmpty) return;
    sink.add(
      'EXPLORE:INVARIANT ${jsonEncode({
            "sig": sig,
            if (route != null) "route": route,
            "items": items
          })}',
    );
  }
}

Map<String, dynamic> decodeExplore(String line) =>
    jsonDecode(line.substring('EXPLORE:INVARIANT '.length))
        as Map<String, dynamic>;

void main() {
  late Directory tmp;
  late String file;

  setUp(() {
    ReproIt.debugClearInvariants();
    tmp = Directory.systemTemp.createTempSync('reproit_invariant_');
    // The Rust backend truncates this file fresh at run start; mirror that.
    file = '${tmp.path}/invariant.ndjson';
    File(file).writeAsStringSync('');
  });

  tearDown(() {
    ReproIt.debugClearInvariants();
    if (tmp.existsSync()) tmp.deleteSync(recursive: true);
  });

  test(
      'VIOLATION: a failing invariant surfaces as EXPLORE:INVARIANT with its '
      'id and message', () {
    // The app declares an invariant that is VIOLATED in this state.
    var cartTotal = -5;
    ReproIt.invariant('cart-total-nonneg', () {
      if (cartTotal < 0) {
        return const InvariantResult.violated('cart total went negative');
      }
      return true;
    });

    // 1) SDK side: real evaluate + append to the marker file.
    final marker = ReproIt.debugEmitInvariantsTo(file);
    expect(marker, isNotNull,
        reason: 'a violated invariant must emit a marker');
    expect(marker, startsWith('REPROIT_INVARIANT '));
    // The SDK leaves sig empty; the explorer substitutes the state sig.
    expect(
        jsonDecode(marker!.substring('REPROIT_INVARIANT '.length))['sig'], '');

    // 2) Explorer side: scrape the file, emit EXPLORE:INVARIANT for this state.
    final emitted = <String>[];
    InvariantScraper(file).observe('SIG-A', 'HomeRoute', emitted);

    expect(emitted, hasLength(1),
        reason: 'exactly one EXPLORE:INVARIANT for the violating state');
    final decoded = decodeExplore(emitted.single);
    expect(decoded['sig'], 'SIG-A', reason: 'explorer substitutes its own sig');
    expect(decoded['route'], 'HomeRoute');
    final items = (decoded['items'] as List).cast<Map>();
    expect(items, hasLength(1));
    expect(items.single['id'], 'cart-total-nonneg');
    expect(items.single['message'], 'cart total went negative');
  });

  test(
      'CLEAN: the SAME invariant, satisfied, produces no marker and no '
      'EXPLORE:INVARIANT', () {
    var cartTotal = 5; // now non-negative: the invariant HOLDS
    ReproIt.invariant('cart-total-nonneg', () {
      if (cartTotal < 0) {
        return const InvariantResult.violated('cart total went negative');
      }
      return true;
    });

    final marker = ReproIt.debugEmitInvariantsTo(file);
    expect(marker, isNull, reason: 'a held invariant is silent (no marker)');

    final emitted = <String>[];
    InvariantScraper(file).observe('SIG-A', 'HomeRoute', emitted);
    expect(emitted, isEmpty,
        reason: 'no violation on the channel -> no EXPLORE:INVARIANT');
  });

  test(
      'DE-DUP: the same violation is emitted once per (sig,id) across settles '
      'of one state, but re-emits on a new state', () {
    ReproIt.invariant('always-fails', () => false); // bool false => violated

    // The SDK appends a marker on several settles of the SAME state.
    ReproIt.debugEmitInvariantsTo(file);
    ReproIt.debugEmitInvariantsTo(file);
    ReproIt.debugEmitInvariantsTo(file);

    final scraper = InvariantScraper(file);
    final emitted = <String>[];
    scraper.observe('SIG-A', null, emitted); // first settle of SIG-A: emits
    scraper.observe('SIG-A', null, emitted); // re-settle SIG-A: silent
    expect(emitted, hasLength(1),
        reason: 'one (sig,id) emits once per state despite 3 SDK markers');

    // A NEW state (different sig) with the same invariant id re-emits.
    ReproIt.debugEmitInvariantsTo(file);
    scraper.observe('SIG-B', null, emitted);
    expect(emitted, hasLength(2),
        reason: 'the same id on a different sig is a distinct violation');
    expect(decodeExplore(emitted.last)['sig'], 'SIG-B');
  });

  test(
      'PREDICATE SEMANTICS: a throwing predicate is violated with the thrown '
      "message, and one thrower does not suppress the others", () {
    ReproIt.invariant('throws', () => throw StateError('boom'));
    ReproIt.invariant('holds-true', () => true);
    ReproIt.invariant('holds-result', () => const InvariantResult.ok());
    ReproIt.invariant('null-is-violation', () => null);

    final items = ReproIt.evaluateInvariants();
    final byId = {for (final it in items) it['id']: it['message']};
    expect(byId.keys, containsAll(<String>['throws', 'null-is-violation']));
    expect(byId.containsKey('holds-true'), isFalse);
    expect(byId.containsKey('holds-result'), isFalse);
    expect(byId['throws'], contains('boom'));
    expect(byId['null-is-violation'], '');
  });
}
