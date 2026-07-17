import 'dart:convert';

import 'package:http/http.dart' as http;

final RegExp _secret = RegExp(
  r'password|passwd|secret|token|authorization|cookie|email|phone|'
  r'api[-_. ]?key|publishable[-_. ]?key|private[-_. ]?key|'
  r'access[-_. ]?key|signing[-_. ]?key',
  caseSensitive: false,
);

Object? redactCausal(Object? value) {
  if (value is List) return value.map(redactCausal).toList();
  if (value is Map) {
    final out = <String, Object?>{};
    final entries = value.entries.toList()
      ..sort((a, b) => a.key.toString().compareTo(b.key.toString()));
    for (final entry in entries) {
      final key = entry.key.toString();
      final child = entry.value;
      if (_secret.hasMatch(key)) {
        final type = child is String
            ? 'string:length=${child.runes.length}'
            : child.runtimeType.toString().toLowerCase();
        out[key] = '<reproit:$type>';
      } else {
        out[key] = redactCausal(child);
      }
    }
    return out;
  }
  return value;
}

Map<String, String> _headers(Map<String, String> headers) => {
      for (final entry
          in (headers.entries.toList()..sort((a, b) => a.key.compareTo(b.key))))
        entry.key:
            _secret.hasMatch(entry.key) ? '<reproit:secret>' : entry.value,
    };

String _canonicalUrl(Uri uri) {
  final pairs = <MapEntry<String, String>>[];
  uri.queryParametersAll.forEach((key, values) {
    for (final value in values) pairs.add(MapEntry(key, value));
  });
  pairs.sort((a, b) {
    final byKey = a.key.compareTo(b.key);
    return byKey != 0 ? byKey : a.value.compareTo(b.value);
  });
  final base = uri.replace(query: '').toString();
  if (pairs.isEmpty) return base;
  final query = pairs.map((entry) {
    final key = Uri.encodeQueryComponent(entry.key);
    final value = Uri.encodeQueryComponent(entry.value);
    return '$key=$value';
  }).join('&');
  return '$base?$query';
}

class ReproItCausalClient extends http.BaseClient {
  final http.Client _inner;
  final int Function() actionIndex;
  final String actor;
  final List<Map<String, dynamic>>? _replay;
  final Set<int> _used = {};
  int _priorAction = 0;
  int _ordinal = 0;

  ReproItCausalClient({
    required this.actionIndex,
    this.actor = 'a',
    http.Client? inner,
    List<Map<String, dynamic>>? replay,
  })  : _inner = inner ?? http.Client(),
        _replay = replay {
    print(
        'REPROIT:CAPABILITIES {"http":{"status":"captured"},"http_replay":{"status":"captured"}}');
  }

  factory ReproItCausalClient.fromEnvironment(
      {required int Function() actionIndex}) {
    const raw = String.fromEnvironment('REPROIT_CAPSULE_JSON');
    List<Map<String, dynamic>>? exchanges;
    if (raw.isNotEmpty) {
      final decoded = jsonDecode(raw) as Map<String, dynamic>;
      exchanges = (decoded['exchanges'] as List<dynamic>? ?? const [])
          .whereType<Map>()
          .map((value) => value.map((k, v) => MapEntry(k.toString(), v)))
          .toList();
    }
    return ReproItCausalClient(actionIndex: actionIndex, replay: exchanges);
  }

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async {
    final action = actionIndex();
    if (action != _priorAction) {
      _priorAction = action;
      _ordinal = 0;
    }
    final ordinal = _ordinal++;
    final canonicalUrl = _canonicalUrl(request.url);
    if (_replay != null) {
      var match = -1;
      for (var i = 0; i < _replay!.length; i++) {
        final exchange = _replay![i];
        if (!_used.contains(i) &&
            exchange['required'] == true &&
            exchange['actor'] == actor &&
            exchange['actionIndex'] == action &&
            exchange['method'].toString().toUpperCase() ==
                request.method.toUpperCase() &&
            _canonicalUrl(Uri.parse(exchange['url'].toString())) ==
                canonicalUrl) {
          match = i;
          break;
        }
      }
      if (match < 0) {
        print('CAPSULE:MISS ${request.method} ${request.url} action=$action');
        throw StateError(
            'CAPSULE:MISS ${request.method} ${request.url} action=$action');
      }
      _used.add(match);
      final exchange = _replay![match];
      print('CAPSULE:HIT ${exchange['id']}');
      final body = exchange['responseBody'];
      final bytes = utf8.encode(body is String ? body : jsonEncode(body ?? ''));
      return http.StreamedResponse(
        http.ByteStream.fromBytes(bytes),
        exchange['status'] as int,
        headers: (exchange['responseHeaders'] as Map? ?? const {})
            .map((k, v) => MapEntry(k.toString(), v.toString())),
        request: request,
      );
    }

    Object? requestBody;
    if (request is http.Request && request.body.isNotEmpty) {
      try {
        requestBody = redactCausal(jsonDecode(request.body));
      } catch (_) {
        requestBody =
            '<reproit:body:length=${utf8.encode(request.body).length}>';
      }
    }
    final response = await _inner.send(request);
    final bytes = await response.stream.toBytes();
    Object? responseBody;
    if ((response.headers['content-type'] ?? '').contains('json')) {
      try {
        responseBody = redactCausal(jsonDecode(utf8.decode(bytes)));
      } catch (_) {
        responseBody = '<reproit:invalid-json>';
      }
    } else {
      responseBody = '<reproit:body:length=${bytes.length}>';
    }
    final exchange = <String, Object?>{
      'id': '$actor-$action-$ordinal',
      'actor': actor,
      'actionIndex': action,
      'ordinal': ordinal,
      'protocol': request.url.scheme,
      'method': request.method,
      'url': request.url.toString(),
      'requestHeaders': _headers(request.headers),
      if (requestBody != null) 'requestBody': requestBody,
      'status': response.statusCode,
      'responseHeaders': _headers(response.headers),
      'responseBody': responseBody,
      'required': true,
    };
    print('REPROIT:EXCHANGE ${jsonEncode(exchange)}');
    return http.StreamedResponse(
      http.ByteStream.fromBytes(bytes),
      response.statusCode,
      contentLength: bytes.length,
      request: request,
      headers: response.headers,
      isRedirect: response.isRedirect,
      persistentConnection: response.persistentConnection,
      reasonPhrase: response.reasonPhrase,
    );
  }

  @override
  void close() => _inner.close();
}
