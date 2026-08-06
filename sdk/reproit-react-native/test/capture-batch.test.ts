import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';

import { buildCaptureBatch, buildEnvelope, replaySeed, validToken } from '../src/capture-batch';
import { buildHttpExchange } from '../src/exchange';

const WORKSPACE = resolve(__dirname, '../../..');

function sampleBatch() {
  const exchange = buildHttpExchange(
    {
      method: 'GET',
      url: 'https://api.test/prices?tier=gold',
      headers: { 'content-type': 'application/json' },
      body: null,
    },
    {
      status: 200,
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ prices: null }),
    },
  );
  return buildCaptureBatch({
    appId: 'app-demo',
    sessionId: 'rn-1753747200000',
    batchId: 'cb-rn-1753747200000-1',
    deployment: { version: '1.4.2', commit: 'abc123' },
    observedAt: '2026-07-27T12:00:00Z',
    occurrence: {
      operation: 'tap:key:testid:quote',
      trigger: { path: [{ sig: 'a1b2c3d4', action: 'tap:key:testid:quote' }] },
      exchanges: [{ ...exchange, at: 1753747200009, monoNs: 9000000 }],
      failure: {
        oracle: 'crash',
        summary: "TypeError: Cannot read property '0' of null",
        signature: 'crash:a1b2c3d4',
        observationPoint: 'QuoteScreen',
      },
      envelope: buildEnvelope({
        observedAtMs: 1753747200000,
        platform: 'ios',
        osVersion: '18.2',
        locale: 'en-US',
        timezone: 'Europe/Berlin',
        replaySeed: 'c0ffee00c0ffee00',
      }),
    },
  });
}

describe('capture batch emission', () => {
  test('the emitted batch passes the protocol validator', () => {
    const batch = sampleBatch();
    const output = execFileSync(
      'cargo',
      ['run', '-q', '-p', 'reproit-protocol', '--bin', 'capture-validate'],
      { cwd: WORKSPACE, input: JSON.stringify(batch), encoding: 'utf8' },
    );
    expect(output).toContain('portable');
  }, 300_000);

  test('the batch carries the trigger, exchanges, envelope, and observation in order', () => {
    const kinds = sampleBatch().events.map((event) => event.event.kind);
    expect(kinds).toEqual([
      'operation-start',
      'trigger',
      'checkpoint',
      'dependency',
      'operation-end',
      'observation',
    ]);
  });

  test('the dependency event nests the raw exchange for the replay projection', () => {
    const dependency = sampleBatch().events.find((event) => event.event.kind === 'dependency');
    const value = dependency?.event.value as { representation: string; value: Record<string, unknown> };
    expect(value.representation).toBe('replayable');
    expect(value.value.kind).toBe('effect');
    const exchange = value.value.exchange as { response: { body: unknown; status: number } };
    expect(exchange.response.status).toBe(200);
    expect(exchange.response.body).toEqual({ prices: null });
    // Real monotonic offsets ride from the recorded exchange, not the ordinal.
    expect(dependency?.monotonicNs).toBe(9000000);
  });

  test('the determinism envelope states what a device can honestly know', () => {
    const checkpoint = sampleBatch().events.find((event) => event.event.kind === 'checkpoint');
    expect(checkpoint?.event.name).toBe('determinism-envelope');
    const attributes = checkpoint?.event.attributes as Record<string, unknown>;
    expect(attributes.runtime).toBe('react-native');
    expect(attributes.tz).toBe('Europe/Berlin');
    expect(attributes.os).toBe('ios');
    expect(attributes.replaySeed).toBe('c0ffee00c0ffee00');
    expect(attributes.observedAtMs).toBe(1753747200000);
    // A device has no process arch or image digest; those are omitted, not guessed.
    expect(attributes.arch).toBeUndefined();
    expect(attributes.imageDigest).toBeUndefined();
  });

  test('deployment identity rides when supplied and is omitted when absent', () => {
    expect(sampleBatch().deployment).toEqual({ version: '1.4.2', commit: 'abc123' });
    const batch = buildCaptureBatch({
      appId: 'app-demo',
      sessionId: 'rn-1',
      batchId: 'cb-rn-1',
      deployment: null,
      occurrence: {
        operation: 'load',
        trigger: null,
        exchanges: [],
        failure: {
          oracle: 'crash',
          summary: 'boom',
          signature: 'crash:x',
          observationPoint: 'load',
        },
        envelope: buildEnvelope({ observedAtMs: 1, replaySeed: replaySeed() }),
      },
    });
    expect(batch.deployment).toBeUndefined();
    expect(validToken(batch.batchId)).toBe(true);
    expect(validToken(batch.projectId)).toBe(true);
  });

  test('the replay seed is a bounded hex token', () => {
    expect(replaySeed()).toMatch(/^[0-9a-f]{16}$/);
  });
});
