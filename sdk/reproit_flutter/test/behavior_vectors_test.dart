// Executes the shared behavioral vectors for the FROZEN runner wire, which is
// deliberately not the capture wire. This SDK is replay only: it never records
// a capture batch, so it has no inline body budget, no header table and no
// $reproit placeholder. Its whole shared surface with the rest of the fleet is
// the secret-key predicate, and eight languages hand implement that predicate.
// A divergence about which keys count as secret is silent in both directions:
// too narrow and a credential ships inside a capsule, too wide and a field
// replay needs is scrubbed into a placeholder that never matches.
// ../capture-behavior-v1.json states the predicate once so a defect is found
// once instead of eight times.
//
// One difference from the capture wire is deliberate and is asserted here so it
// cannot be closed by accident: idempotency_key IS secret on the capture wire
// and is NOT secret here. The runner list is thirteen parts, one shorter,
// because changing it would change bytes the fuzz harness compares.
//
// redactCausal folds a secret string to its length form; the bare
// <reproit:secret> placeholder is produced only by the private header slot, so
// the second test drives a real exchange for the fields that are legal HTTP
// header names.
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:reproit_flutter/src/causal.dart';

final Map<String, dynamic> _vectors =
    (jsonDecode(File('../capture-behavior-v1.json').readAsStringSync())
        as Map<String, dynamic>)['causalRedaction'] as Map<String, dynamic>;
final List<dynamic> _foldingCases = _vectors['foldingCases'] as List<dynamic>;
final RegExp _headerName = RegExp(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$");

class _FakeClient extends http.BaseClient {
  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async =>
      http.StreamedResponse(http.ByteStream.fromBytes(utf8.encode('{}')), 200,
          headers: {'content-type': 'application/json'}, request: request);
}

void main() {
  test('causalRedaction folding cases fold exactly as the shared vector says',
      () {
    expect(_foldingCases, isNotEmpty);
    for (final entry in _foldingCases) {
      final field = entry['field'] as String;
      final secret = entry['secret'] as bool;
      final safe = redactCausal({field: 'raw-value'}) as Map<String, Object?>;
      expect(safe[field], secret ? '<reproit:string:length=9>' : 'raw-value',
          reason: '$field should${secret ? '' : ' not'} be treated as secret');
    }
  });

  test('causalRedaction placeholder is what the header slot emits', () async {
    final cases = _foldingCases
        .where((entry) => _headerName.hasMatch(entry['field'] as String))
        .toList();
    final lines = <String>[];
    await runZoned(
      () async {
        final client =
            ReproItCausalClient(actionIndex: () => 0, inner: _FakeClient());
        await client.get(
          Uri.parse('https://app.test/feed'),
          headers: {
            for (final entry in cases) entry['field'] as String: 'raw-value'
          },
        );
      },
      zoneSpecification:
          ZoneSpecification(print: (_, __, ___, line) => lines.add(line)),
    );
    final marker =
        lines.firstWhere((line) => line.startsWith('REPROIT:EXCHANGE '));
    final emitted = (jsonDecode(marker.substring('REPROIT:EXCHANGE '.length))
        as Map<String, dynamic>)['requestHeaders'] as Map<String, dynamic>;
    // Header names survive with whatever case the transport used, so both
    // sides are folded before comparing.
    final headers = emitted.map((k, v) => MapEntry(k.toLowerCase(), v));
    for (final entry in cases) {
      final field = entry['field'] as String;
      final secret = entry['secret'] as bool;
      expect(headers[field.toLowerCase()],
          secret ? _vectors['placeholder'] : 'raw-value',
          reason:
              'header $field should${secret ? '' : ' not'} be treated as secret');
    }
  });
}
