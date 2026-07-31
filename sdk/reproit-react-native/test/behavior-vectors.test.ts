// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// This SDK is one of the two that independently shipped the trigger token
// `user-action`, which is not in the protocol vocabulary; the validator caught
// both. The triggerTokens group pins it so a third instance cannot ship.

import { readFileSync } from 'fs';
import { join } from 'path';

import {
  MAX_EXCHANGE_BODY_BYTES,
  MAX_EXCHANGE_HEADERS,
  boundedBody,
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
      expect(actual).toEqual(expected);
    }
  });

  test('redaction type vectors', () => {
    for (const kase of VECTORS.redaction.typeCases) {
      expect(redactExchangeValue(kase.input)).toEqual(kase.expect);
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
