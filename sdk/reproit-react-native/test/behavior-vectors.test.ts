// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// This SDK is one of the two that independently shipped the trigger token
// `user-action`, which is not in the protocol vocabulary; the validator caught
// both. The triggerTokens group pins it so a third instance cannot ship.
//
// The remaining groups are harvested from defects, not invented. Each names
// the one it pins:
//
//   bounds            a budget measured in string length rather than encoded
//                     bytes records 4096 characters of "€" inline, 12288
//                     bytes, past a budget the replayer trusts.
//   headers           this SDK capped the 32 headers in insertion order, the
//                     Go defect verbatim, so the retained subset changed run
//                     to run. The cap is defined over NAME SORTED order, so
//                     the generated case is fed scrambled on purpose.
//   redaction.type    the $reproit stub must report the ORIGINAL type and
//                     length; a stub claiming "string" for everything makes
//                     the recorded shape unreplayable.
//   redaction.folding secret detection folds case and separators and matches
//                     substrings, so `X-Authorization` and `tokenizer` are
//                     secret and `username` is not.
//   redaction.nesting redaction recurses through objects AND arrays; a
//                     top-level-only scrub shipped nested keys in plaintext.
//   redaction.structure  redaction preserves shape: no key dropped, no array
//                     shortened, an explicit null stays a null VALUE. An
//                     encoder dropping null values made a capsule say
//                     {"symbol":"ACME"} where production sent
//                     {"prices":null}, and replay reproduced a DIFFERENT bug.

import { readFileSync } from 'fs';
import { join } from 'path';

import {
  MAX_EXCHANGE_BODY_BYTES,
  MAX_EXCHANGE_HEADERS,
  boundedBody,
  boundedHeaders,
  redactExchangeValue,
} from '../src/exchange';

const VECTORS = JSON.parse(
  readFileSync(join(__dirname, '../../capture-behavior-v1.json'), 'utf8'),
);

function bodyOf(spec: Record<string, unknown>): unknown {
  const repeat = spec.bodyRepeat as [string, number] | undefined;
  if (repeat) return repeat[0].repeat(repeat[1]);
  return spec.body;
}

// jest matchers take no message argument, so the case name is folded into both
// sides of the comparison: a failure then prints WHICH vector failed rather
// than an anonymous diff.
function labelled(name: string, value: unknown): { case: string; value: unknown } {
  return { case: name, value };
}

// Build the generated header table in an order that is neither ascending nor
// descending: 17 is coprime with 40, so `index * 17 % count` is a permutation.
// A cap applied before sorting therefore keeps a visibly wrong subset instead
// of accidentally passing on an already-sorted input.
function scrambledHeaders(spec: {
  headerCount: number;
  namePattern: string;
  value: string;
}): Record<string, string> {
  const headers: Record<string, string> = {};
  for (let step = 0; step < spec.headerCount; step += 1) {
    const index = (step * 17) % spec.headerCount;
    headers[spec.namePattern.replace('%02d', String(index).padStart(2, '0'))] = spec.value;
  }
  return headers;
}

describe('shared behavior vectors', () => {
  test('constants match the shared vectors', () => {
    expect(MAX_EXCHANGE_BODY_BYTES).toBe(VECTORS.constants.maxExchangeBodyBytes);
    expect(MAX_EXCHANGE_HEADERS).toBe(VECTORS.constants.maxExchangeHeaders);
  });

  test('bounds vectors', () => {
    for (const kase of VECTORS.bounds.cases) {
      const actual = boundedBody(
        bodyOf(kase.input) as string,
        kase.input.contentType as string,
      );
      const expected = { ...kase.expect };
      if (expected.body && Array.isArray(expected.body.repeat)) {
        expected.body = expected.body.repeat[0].repeat(expected.body.repeat[1]);
      }
      expect(labelled(kase.name, actual)).toEqual(labelled(kase.name, expected));
    }
  });

  test('header vectors', () => {
    for (const kase of VECTORS.headers.cases) {
      if (kase.input) {
        const actual = boundedHeaders(kase.input.headers);
        expect(labelled(kase.name, actual)).toEqual(labelled(kase.name, kase.expect));
        continue;
      }
      const actual = boundedHeaders(scrambledHeaders(kase.inputGenerated));
      const names = Object.keys(actual.headers as Record<string, string>).sort();
      expect(labelled(kase.name, names.length)).toEqual(
        labelled(kase.name, kase.expect.headerCount),
      );
      // The cap must be over sorted names, not the order the headers arrived.
      expect(names[0]).toBe(kase.expect.firstName);
      expect(names[names.length - 1]).toBe(kase.expect.lastName);
    }
  });

  test('redaction type vectors', () => {
    for (const kase of VECTORS.redaction.typeCases) {
      const label = JSON.stringify(kase.input);
      expect(labelled(label, redactExchangeValue(kase.input))).toEqual(
        labelled(label, kase.expect),
      );
    }
  });

  test('redaction key folding vectors', () => {
    for (const kase of VECTORS.redaction.foldingCases) {
      const out = redactExchangeValue({ [kase.field]: 'value' }) as Record<
        string,
        { $reproit?: unknown }
      >;
      const redacted = Boolean(out[kase.field] && out[kase.field].$reproit);
      expect(redacted).toBe(kase.secret);
    }
  });

  test('redaction nesting vectors', () => {
    for (const kase of VECTORS.redaction.nestingCases) {
      const label = JSON.stringify(kase.input);
      expect(labelled(label, redactExchangeValue(kase.input))).toEqual(
        labelled(label, kase.expect),
      );
    }
  });

  test('redaction structure vectors', () => {
    for (const kase of VECTORS.redaction.structureCases) {
      // toStrictEqual, not toEqual: a key present as `undefined` is a key the
      // matcher no longer walks, so it must fail exactly like a dropped one.
      expect(labelled(kase.name, redactExchangeValue(kase.input))).toStrictEqual(
        labelled(kase.name, kase.expect),
      );
    }
  });

  // The defect this SDK shipped: `user-action` is not in the vocabulary.
  test('the trigger token is in the protocol vocabulary', () => {
    const token = VECTORS.triggerTokens.bySdkKind.mobile;
    expect(VECTORS.triggerTokens.allowed).toContain(token);
    const source = readFileSync(join(__dirname, '../src/capture-batch.ts'), 'utf8');
    expect(source).toContain(`'${token}'`);
    for (const bad of VECTORS.triggerTokens.rejected) {
      expect(source).not.toContain(`'${bad}'`);
    }
  });
});

/**
 * The invariant ledger recorded that all three mobile SDKs emit the structured
 * marker ALONGSIDE the frozen runner contract, and that nothing asserted both
 * are emitted together. A platform silently dropping the structured marker
 * would misreport a mobile divergence through the CLI, which is the exact
 * defect that addition existed to fix.
 */
describe('vocabularies', () => {
  const vocab = VECTORS.vocabularies;

  it('emits BOTH divergence markers on an unmatched call, never one instead', () => {
    const source = readFileSync(join(__dirname, '../src/causal.ts'), 'utf8');
    expect(source).toContain(vocab.divergenceMarkers.structured);
    expect(source).toContain(vocab.divergenceMarkers.runnerContract);
    // The frozen contract must still be the thrown error, so the fuzz harness
    // that consumes it byte for byte keeps working.
    expect(source).toMatch(/throw new Error\(`CAPSULE:MISS/);
  });

  it('keeps the header and body redaction placeholders distinct by type', () => {
    const source = readFileSync(join(__dirname, '../src/exchange.ts'), 'utf8');
    expect(source).toContain(vocab.redaction.headerPlaceholder);
    expect(source).toContain(vocab.redaction.bodyPlaceholderKey);
  });
});
