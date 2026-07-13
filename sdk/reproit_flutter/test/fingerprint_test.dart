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

  // ---- v2 features (bytes / scripts / combining / zero-width / newline / ws) --
  // Parity with the web reference (reproit-web.js) and its fingerprint_test.js.
  group('fingerprintValue v2 features', () {
    test('bytes is UTF-8 length, distinct from code-point len', () {
      // J o s é(2B) 🎉(4B) -> 9 bytes, 5 code points.
      final r = ReproIt.fingerprintValue('Jos\u{00E9}\u{1F389}');
      expect(r['len'], 5);
      expect(r['bytes'], 9);
      expect(
          ReproIt.fingerprintValue('hello')['bytes'], 5); // ascii: bytes == len
    });

    test('graphemes counts user-visible clusters', () {
      expect(ReproIt.fingerprintValue('hello')['graphemes'], 5);
      expect(ReproIt.fingerprintValue('e\u{0301}')['len'], 2);
      expect(ReproIt.fingerprintValue('e\u{0301}')['graphemes'], 1);
      expect(ReproIt.fingerprintValue('👨‍👩‍👧‍👦')['graphemes'], 1);
    });

    test('scripts lists buckets present, sorted, mixed-script', () {
      expect(ReproIt.fingerprintValue('hello')['scripts'], ['Latin']);
      expect(ReproIt.fingerprintValue('hello')['bytes'], 5);
      // Arabic "مرحبا".
      final ar =
          ReproIt.fingerprintValue('\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}');
      expect(ar['scripts'], ['Arabic']);
      expect(ar['isRtl'], true);
      // Mixed Latin + Arabic.
      expect(
        ReproIt.fingerprintValue(
            'hi \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}')['scripts'],
        ['Arabic', 'Latin'],
      );
      // CJK "日本語".
      expect(
        ReproIt.fingerprintValue('\u{65E5}\u{672C}\u{8A9E}')['scripts'],
        ['CJK'],
      );
      // Digits are no script.
      expect(ReproIt.fingerprintValue('12345')['scripts'], <String>[]);
    });

    test('hasNewline detects LF and CR', () {
      expect(ReproIt.fingerprintValue('line1\nline2')['hasNewline'], true);
      expect(ReproIt.fingerprintValue('a\rb')['hasNewline'], true);
      expect(ReproIt.fingerprintValue('oneline')['hasNewline'], false);
    });

    test('hasZeroWidth detects invisible code points', () {
      expect(
          ReproIt.fingerprintValue('a\u{200B}b')['hasZeroWidth'], true); // ZWSP
      expect(ReproIt.fingerprintValue('ab')['hasZeroWidth'], false);
    });

    test('hasCombiningMarks detects decomposed accents', () {
      // e + combining acute.
      expect(ReproIt.fingerprintValue('e\u{0301}')['hasCombiningMarks'], true);
      expect(ReproIt.fingerprintValue('e')['hasCombiningMarks'], false);
      // Precomposed é.
      expect(ReproIt.fingerprintValue('\u{00E9}')['hasCombiningMarks'], false);
    });

    test('leadingTrailingWhitespace flags edge whitespace', () {
      expect(ReproIt.fingerprintValue(' hello')['leadingTrailingWhitespace'],
          true);
      expect(ReproIt.fingerprintValue('hello ')['leadingTrailingWhitespace'],
          true);
      expect(ReproIt.fingerprintValue('hello')['leadingTrailingWhitespace'],
          false);
      // Interior tab only.
      expect(
          ReproIt.fingerprintValue('a\tb')['leadingTrailingWhitespace'], false);
    });

    test('fpVersion is 2', () {
      expect(ReproItFingerprint.fpVersion, 2);
    });
  });
}
