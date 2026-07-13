/**
 * Unit tests for the PII-safe input fingerprint (tier-3 on-error context).
 * Parity cases mirror the other four SDKs: capture FEATURES, never raw values.
 */
import { fingerprintValue, fingerprintFields, FP_VERSION } from '../src/fingerprint';

describe('fingerprintValue, PII-safe features (not values)', () => {
  test('"José🎉" counts code points and is unicode + emoji', () => {
    const r = fingerprintValue('José🎉');
    expect(r.len).toBe(5); // code points, not UTF-16 length
    expect(r.charset).toBe('unicode');
    expect(r.hasEmoji).toBe(true);
    expect(r.isEmpty).toBe(false);
    expect(r.isRtl).toBe(false);
  });

  test('"12345" is numeric', () => {
    const r = fingerprintValue('12345');
    expect(r.charset).toBe('numeric');
    expect(r.len).toBe(5);
    expect(r.hasEmoji).toBe(false);
  });

  test('"hello" is ascii', () => {
    expect(fingerprintValue('hello').charset).toBe('ascii');
  });

  test('"" isEmpty', () => {
    const r = fingerprintValue('');
    expect(r.isEmpty).toBe(true);
    expect(r.len).toBe(0);
    expect(r.charset).toBe('ascii');
  });

  test('whitespace-only isEmpty', () => {
    expect(fingerprintValue('   ').isEmpty).toBe(true);
  });

  test('Arabic string isRtl + unicode', () => {
    const r = fingerprintValue('مرحبا');
    expect(r.isRtl).toBe(true);
    expect(r.charset).toBe('unicode');
    expect(r.hasEmoji).toBe(false);
  });

  test('Hebrew string isRtl', () => {
    expect(fingerprintValue('שלום').isRtl).toBe(true);
  });

  test('Turkish dotless i is unicode not ascii', () => {
    const r = fingerprintValue('ıstanbul');
    expect(r.charset).toBe('unicode');
    expect(r.isRtl).toBe(false);
  });

  test('312-char name reports exact length', () => {
    const r = fingerprintValue('a'.repeat(312));
    expect(r.len).toBe(312);
    expect(r.charset).toBe('ascii');
  });

  test('null / undefined treated as empty', () => {
    expect(fingerprintValue(null).isEmpty).toBe(true);
    expect(fingerprintValue(undefined).isEmpty).toBe(true);
  });

  test('never echoes the raw value', () => {
    const raw = 'secret-pii-value';
    expect(JSON.stringify(fingerprintValue(raw))).not.toContain(raw);
  });
});

describe('fingerprintValue v2 features (bytes / scripts / combining / zero-width / newline / ws)', () => {
  test('bytes is UTF-8 length, distinct from code-point len', () => {
    const r = fingerprintValue('José\u{1F389}'); // J o s é(2B) 🎉(4B) -> 9 bytes, 5 code points
    expect(r.len).toBe(5);
    expect(r.bytes).toBe(9);
    expect(fingerprintValue('hello').bytes).toBe(5); // ascii: bytes == len
  });

  test('graphemes counts user-visible clusters', () => {
    expect(fingerprintValue('hello').graphemes).toBe(5);
    expect(fingerprintValue('e\u{0301}').len).toBe(2);
    expect(fingerprintValue('e\u{0301}').graphemes).toBe(1);
    expect(fingerprintValue('👨‍👩‍👧‍👦').graphemes).toBe(1);
  });

  test('scripts lists buckets present, sorted, mixed-script', () => {
    expect(fingerprintValue('hello').scripts).toEqual(['Latin']);
    expect(fingerprintValue('hello').bytes).toBe(5);
    expect(fingerprintValue('مرحبا').scripts).toEqual(['Arabic']);
    expect(fingerprintValue('مرحبا').isRtl).toBe(true);
    expect(fingerprintValue('hi مرحبا').scripts).toEqual(['Arabic', 'Latin']);
    expect(fingerprintValue('日本語').scripts).toEqual(['CJK']);
    expect(fingerprintValue('12345').scripts).toEqual([]); // digits are no script
  });

  test('hasNewline detects LF and CR', () => {
    expect(fingerprintValue('line1\nline2').hasNewline).toBe(true);
    expect(fingerprintValue('a\rb').hasNewline).toBe(true);
    expect(fingerprintValue('oneline').hasNewline).toBe(false);
  });

  test('hasZeroWidth detects invisible code points', () => {
    expect(fingerprintValue('a\u{200B}b').hasZeroWidth).toBe(true); // ZWSP
    expect(fingerprintValue('ab').hasZeroWidth).toBe(false);
  });

  test('hasCombiningMarks detects decomposed accents', () => {
    expect(fingerprintValue('e\u{0301}').hasCombiningMarks).toBe(true); // e + combining acute
    expect(fingerprintValue('\u{00E9}').hasCombiningMarks).toBe(false); // precomposed é
    expect(fingerprintValue('e').hasCombiningMarks).toBe(false);
  });

  test('leadingTrailingWhitespace flags edge whitespace', () => {
    expect(fingerprintValue(' hello').leadingTrailingWhitespace).toBe(true);
    expect(fingerprintValue('hello ').leadingTrailingWhitespace).toBe(true);
    expect(fingerprintValue('hello').leadingTrailingWhitespace).toBe(false);
    expect(fingerprintValue('a\tb').leadingTrailingWhitespace).toBe(false); // interior only
  });

  test('FP_VERSION is exported and is 2', () => {
    expect(FP_VERSION).toBe(2);
  });
});

describe('fingerprintFields', () => {
  test('keeps the field label, drops the value, emits features', () => {
    const out = fingerprintFields([
      { field: 'email', value: 'a@b.co' },
      { field: '#1', value: '12345' },
      { field: 'note', value: '' },
    ]);
    expect(out).toHaveLength(3);
    expect(out[0].field).toBe('email');
    expect(out[1].charset).toBe('numeric');
    expect(out[2].isEmpty).toBe(true);
    // no raw values anywhere in the output
    expect(JSON.stringify(out)).not.toContain('a@b.co');
  });
});
