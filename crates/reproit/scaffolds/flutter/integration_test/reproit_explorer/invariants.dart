part of '../reproit_explorer.dart';

// ===========================================================================
// APP-INVARIANT oracle (EXPLORE:INVARIANT) - the app's OWN predicates.
//
// The app declares invariants that must hold in every visited state via the
// reproit SDK (`ReproIt.invariant("id", () => holds)`). Under the fuzzer the
// SDK evaluates them on each settle and APPENDS a
// `REPROIT_INVARIANT {"sig":"","items":[{id,message}]}` line per violation to
// the file named by REPROIT_INVARIANT_FILE (the env var the Flutter backend
// set, which is ALSO the SDK's fuzzer-detection gate). We run in the same
// isolate as the app, so on each observe we read the lines the SDK appended
// since the last observe (tracked by a byte offset) and re-emit them as
// EXPLORE:INVARIANT for the state the explorer is currently on, substituting
// the empty SDK sig. De-duped per (sig,id) so re-settling one state does not
// re-emit a held-over violation. The app owns this ground truth, so a reported
// violation is real (FP-free); silent when none are registered or all held.

/// Bytes of REPROIT_INVARIANT_FILE already consumed (so each appended marker is
/// read once), reset if the file is truncated/rotated under us.
int _invariantFileOffset = 0;

/// (sig id) pairs already emitted, so the same violation is not re-emitted
/// across settles of one state.
final Set<String> _emittedInvariants = <String>{};
final Set<String> _emittedRelations = <String>{};

/// Parse one line for the SDK marker `REPROIT_INVARIANT {json}`; returns its
/// `{id,message}` items (message defaults to ""), or an empty list when the
/// line is not a marker or its JSON is malformed.
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

/// Read the REPROIT_INVARIANT markers the SDK appended since the last observe
/// and emit EXPLORE:INVARIANT for the current state [sig]/[route]. De-dups per
/// (sig,id). Silent when no new violations. Best-effort file I/O.
void scrapeInvariants(String sig, String? route) {
  final path = Platform.environment['REPROIT_INVARIANT_FILE'];
  if (path == null || path.isEmpty) return;
  final file = File(path);
  if (!file.existsSync()) return;
  final len = file.lengthSync();
  if (len < _invariantFileOffset) _invariantFileOffset = 0; // truncated
  if (len <= _invariantFileOffset) return;
  List<int> bytes;
  try {
    final raf = file.openSync();
    raf.setPositionSync(_invariantFileOffset);
    bytes = raf.readSync(len - _invariantFileOffset);
    raf.closeSync();
  } catch (_) {
    return;
  }
  _invariantFileOffset = len;
  final items = <Map<String, String>>[];
  for (final line in utf8.decode(bytes).split('\n')) {
    if (line.trim().isEmpty) continue;
    const relationMark = 'REPROIT_RELATION ';
    final relationAt = line.indexOf(relationMark);
    if (relationAt >= 0) {
      try {
        final obj = jsonDecode(
          line.substring(relationAt + relationMark.length),
        );
        if (obj is Map &&
            (obj['stableSamples'] as num?)?.toInt() != null &&
            (obj['stableSamples'] as num).toInt() >= 2 &&
            obj['checks'] is List) {
          final checks = (obj['checks'] as List)
              .whereType<Map>()
              .map((x) => Map<String, dynamic>.from(x))
              .where(
                (x) =>
                    x['kind'] == 'indicator-anchor' &&
                    x['dependentKey'] is String &&
                    x['ownerKey'] is String &&
                    x['containerKey'] is String &&
                    const ['PROVEN', 'VALID', 'UNKNOWN'].contains(x['outcome']),
              )
              .toList();
          if (checks.isNotEmpty &&
              _emittedRelations.add('$sig ${jsonEncode(checks)}')) {
            final outcome = checks.any((x) => x['outcome'] == 'PROVEN')
                ? 'PROVEN'
                : (checks.every((x) => x['outcome'] == 'VALID')
                      ? 'VALID'
                      : 'UNKNOWN');
            emitJson('EXPLORE:RELATIONSTATUS', {
              "sig": sig,
              "route": ?route,
              "outcome": outcome,
              "checks": checks,
            });
            if (outcome == 'PROVEN') {
              emitJson('EXPLORE:RELATION', {
                "sig": sig,
                "route": ?route,
                "items": checks.where((x) => x['outcome'] == 'PROVEN').toList(),
              });
            }
          }
        }
      } catch (_) {}
    }
    for (final it in parseInvariantItems(line)) {
      if (_emittedInvariants.add('$sig ${it['id']}')) items.add(it);
    }
  }
  if (items.isEmpty) return;
  emitJson('EXPLORE:INVARIANT', {"sig": sig, "route": ?route, "items": items});
}
