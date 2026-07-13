import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:reproit_flutter/src/causal.dart';

class _FakeClient extends http.BaseClient {
  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async {
    return http.StreamedResponse(
      http.ByteStream.fromBytes(utf8.encode(jsonEncode({
        'profile': {'email': 'a@example.com'},
        'author': null,
      }))),
      200,
      headers: {'content-type': 'application/json'},
      request: request,
    );
  }
}

void main() {
  test('capture redacts before emitting a universal marker', () async {
    final lines = <String>[];
    await runZoned(
      () async {
        final client = ReproItCausalClient(actionIndex: () => 1, inner: _FakeClient());
        final response = await client.post(
          Uri.parse('https://app.test/feed'),
          headers: {'authorization': 'Bearer raw', 'content-type': 'application/json'},
          body: jsonEncode({'token': 'raw', 'query': 'ok'}),
        );
        expect(response.statusCode, 200);
      },
      zoneSpecification: ZoneSpecification(
        print: (_, __, ___, line) => lines.add(line),
      ),
    );
    final marker = lines.firstWhere((line) => line.startsWith('REPROIT:EXCHANGE '));
    final exchange = jsonDecode(marker.substring('REPROIT:EXCHANGE '.length));
    expect(exchange['requestHeaders']['authorization'], '<reproit:secret>');
    expect(exchange['requestBody']['token'], '<reproit:string:length=3>');
    expect(exchange['responseBody']['profile']['email'], '<reproit:string:length=13>');
  });

  test('replay matches exactly and blocks a missing request', () async {
    final replay = <Map<String, dynamic>>[
      {
        'id': 'a-0-0', 'actor': 'a', 'actionIndex': 0, 'ordinal': 0,
        'protocol': 'https', 'method': 'GET', 'url': 'https://app.test/config',
        'status': 200, 'responseHeaders': {'content-type': 'application/json'},
        'responseBody': {'enabled': true}, 'required': true,
      }
    ];
    final client = ReproItCausalClient(actionIndex: () => 0, replay: replay);
    expect(jsonDecode((await client.get(Uri.parse('https://app.test/config'))).body), {'enabled': true});
    expect(
      () => client.get(Uri.parse('https://app.test/other')),
      throwsA(isA<StateError>()),
    );
  });

  test('redaction is structural', () {
    expect(redactCausal({'phone': '123', 'nested': {'ok': 1}}), {
      'nested': {'ok': 1},
      'phone': '<reproit:string:length=3>',
    });
  });
}
