/**
 * Canonical signature parity gate. Loads the golden vectors in
 * `signature_vectors.json` (repo root) and asserts the RN SDK's structural
 * `signatureOf(anchor, tree)` reproduces every `expected_sig` bit-for-bit, so
 * production RN telemetry buckets to the SAME node as the Rust oracle, the web
 * SDK, and the fuzz runners. This is the same three-line gate every other
 * implementation runs (see crates/reproit/src/model/signature.rs).
 */
import { readFileSync } from 'node:fs';
import * as path from 'node:path';
import { signatureOf, descriptorOf, fnv1a32hex, valueClass, type Node } from '../src/signature';

interface Vector {
  description: string;
  anchor: string | null;
  tree: Node;
  expected_sig: string;
}

function loadVectors(): Vector[] {
  // signature_vectors.json lives at the repo root; this test file is at
  // <repo>/sdk/reproit-react-native/test, so go up three levels.
  const p = path.join(__dirname, '..', '..', '..', 'signature_vectors.json');
  return JSON.parse(readFileSync(p, 'utf8')) as Vector[];
}

describe('canonical structural signature, golden vectors', () => {
  const vectors = loadVectors();

  test('all golden vectors are present (structural + value-state + unicode)', () => {
    expect(vectors.length).toBe(25);
  });

  for (const v of vectors) {
    test(`vector: ${v.description.slice(0, 80)}`, () => {
      const got = signatureOf(v.anchor, v.tree);
      if (got !== v.expected_sig) {
        // Surface the exact descriptor on mismatch for fast debugging.
        // eslint-disable-next-line no-console
        console.error('descriptor =', JSON.stringify(descriptorOf(v.anchor, v.tree)));
      }
      expect(got).toBe(v.expected_sig);
    });
  }
});

describe('cross-vector relationships the spec promises', () => {
  const vectors = loadVectors();
  const by = (needle: string): string => {
    const v = vectors.find((x) => x.description.includes(needle));
    if (!v) throw new Error(`no vector matching ${JSON.stringify(needle)}`);
    return v.expected_sig;
  };

  test('text-exclusion + transient-drop collapse to the basic login', () => {
    const login = by('basic login');
    expect(by('locale-invariance')).toBe(login);
    expect(by('transient-drop (spinner)')).toBe(login);
    expect(by('transient-drop (snackbar')).toBe(login);
  });

  test('collapse drops the count (3 items == 5 items)', () => {
    expect(by('repeated-collapse (3 items)')).toBe(by('repeated-collapse (5 items'));
  });

  test('discriminators split (type, icon)', () => {
    const login = by('basic login');
    expect(by('collision-fix via input type')).not.toBe(login);
    expect(by('collision-fix via icon')).not.toBe(login);
    expect(by('collision-fix via input type')).not.toBe(by('collision-fix via icon'));
  });

  test('anchor semantics (route is part of identity)', () => {
    const settings = by('same route + same structure');
    expect(by('different route + same structure')).not.toBe(settings);
    expect(by('same route + different structure')).not.toBe(settings);
    expect(by('parameterized route (item 42)')).toBe(by('parameterized route (item 99)'));
  });

  test('value-state (Layer 2): EMPTY / ZERO / POS1 are three distinct states', () => {
    const vEmpty = by('empty value-class');
    const vZero = by('zero value-class');
    const vPos1 = by('POS1 value-class');
    expect(vEmpty).not.toBe(vZero);
    expect(vEmpty).not.toBe(vPos1);
    expect(vZero).not.toBe(vPos1);
  });

  test('value-state: numeric counter 0 vs 5 -> ZERO vs POS1 distinct', () => {
    expect(by('counter at 0')).not.toBe(by('counter at 5'));
  });

  test('value-state: chrome label with text is backward-compatible (no V: section)', () => {
    // A header (chrome role) carrying a value is identical to the same structure
    // with no value field: the empty-anchor structural form, hand-built here.
    const s: Node = { role: 'screen', children: [{ role: 'header', id: 'title' }] };
    expect(signatureOf('/home', s)).toBe(by('chrome label with text'));
  });

  test('value-state: grouped/locale number is locale-safe (NONEMPTY), distinct', () => {
    const vGrouped = by('grouped/locale number');
    expect(vGrouped).not.toBe(by('POS1 value-class'));
    expect(vGrouped).not.toBe(by('zero value-class'));
  });

  test('value-state: two different POS1 values (3 vs 7) bucket the same', () => {
    expect(by('two different POS1 values bucket the same (3)')).toBe(
      by('two different POS1 values bucket the same (7)'),
    );
  });
});

describe('value-class bucketer (Layer 2)', () => {
  test('all buckets, trimming applied', () => {
    expect(valueClass('')).toBe('EMPTY');
    expect(valueClass('   ')).toBe('EMPTY');
    expect(valueClass('0')).toBe('ZERO');
    expect(valueClass('-0')).toBe('ZERO');
    expect(valueClass('-3')).toBe('NEG');
    expect(valueClass('3')).toBe('POS1');
    expect(valueClass('+7')).toBe('POS1');
    expect(valueClass('10')).toBe('POS2');
    expect(valueClass('100')).toBe('POS3');
    expect(valueClass('1000')).toBe('POSL');
    expect(valueClass('  42  ')).toBe('POS2');
  });
  test('ambiguous / locale formats fall back to NONEMPTY', () => {
    expect(valueClass('1,234')).toBe('NONEMPTY');
    expect(valueClass('$5')).toBe('NONEMPTY');
    expect(valueClass('1e3')).toBe('NONEMPTY');
    expect(valueClass('3.')).toBe('NONEMPTY');
    expect(valueClass('.5')).toBe('NONEMPTY');
    expect(valueClass('hello')).toBe('NONEMPTY');
  });
});

describe('FNV-1a known values', () => {
  test('empty string is the offset basis', () => {
    expect(fnv1a32hex('')).toBe('811c9dc5');
  });
  test('"a" is the known FNV-1a 32 value', () => {
    expect(fnv1a32hex('a')).toBe('e40c292c');
  });
});
