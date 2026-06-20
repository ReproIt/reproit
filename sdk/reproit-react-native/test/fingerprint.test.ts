/**
 * Unit tests for the PII-safe input fingerprint (tier-3 on-error context).
 * Parity cases mirror the other four SDKs: capture FEATURES, never raw values.
 */
import { fingerprintValue, fingerprintFields } from '../src/fingerprint';

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
