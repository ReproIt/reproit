import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:reproit_flutter/reproit_flutter.dart';

// Unit tests for the PII-safe input fingerprint (tier-3 on-error context).
// Parity cases mirror the other four SDKs: capture FEATURES, never raw values.
void main() {
  group('fingerprintValue, PII-safe features (not values)', () {
    test('"José🎉" counts code points and is unicode + emoji', () {
      final r = ReproIt.fingerprintValue('José🎉');
      expect(r['len'], 5); // code points, not UTF-16 length
      expect(r['charset'], 'unicode');
      expect(r['hasEmoji'], true);
      expect(r['isEmpty'], false);
      expect(r['isRtl'], false);
    });

    test('"12345" is numeric', () {
      final r = ReproIt.fingerprintValue('12345');
      expect(r['charset'], 'numeric');
      expect(r['len'], 5);
      expect(r['hasEmoji'], false);
    });

    test('"hello" is ascii', () {
      expect(ReproIt.fingerprintValue('hello')['charset'], 'ascii');
    });

    test('"" isEmpty', () {
      final r = ReproIt.fingerprintValue('');
      expect(r['isEmpty'], true);
      expect(r['len'], 0);
      expect(r['charset'], 'ascii');
    });

    test('whitespace-only isEmpty', () {
      expect(ReproIt.fingerprintValue('   ')['isEmpty'], true);
    });

    test('Arabic string isRtl + unicode', () {
      final r = ReproIt.fingerprintValue('مرحبا');
      expect(r['isRtl'], true);
      expect(r['charset'], 'unicode');
      expect(r['hasEmoji'], false);
    });

    test('Hebrew string isRtl', () {
      expect(ReproIt.fingerprintValue('שלום')['isRtl'], true);
    });

    test('Turkish dotless i is unicode not ascii', () {
      final r = ReproIt.fingerprintValue('ıstanbul');
      expect(r['charset'], 'unicode');
      expect(r['isRtl'], false);
    });

    test('312-char name reports exact length', () {
      final r = ReproIt.fingerprintValue('a' * 312);
      expect(r['len'], 312);
      expect(r['charset'], 'ascii');
    });

    test('never echoes the raw value', () {
      const raw = 'secret-pii-value';
      expect(jsonEncode(ReproIt.fingerprintValue(raw)).contains(raw), isFalse);
    });
  });
}
