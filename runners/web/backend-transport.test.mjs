import assert from 'node:assert/strict';
import test from 'node:test';
import {
  backendCorrelationHeaders,
  decodeBackendEventHeader,
  encodeBackendEventHeader,
  redactNetworkHeaders,
  redactNetworkValue,
} from './runner.mjs';

test('correlation is first party, deterministic, and structural', () => {
  assert.deepEqual(
    backendCorrelationHeaders('https://app.test/api', 3, 7, 'https://app.test', 'alice'),
    {
      'x-reproit-trace': 'rpt-alice-3-7',
      'x-reproit-actor': 'alice',
      'x-reproit-action': '3',
    },
  );
  assert.equal(
    backendCorrelationHeaders('https://vendor.test/api', 3, 7, 'https://app.test', 'alice'),
    null,
  );
  assert.equal(
    backendCorrelationHeaders(
      'https://api.app.test/messages',
      3,
      8,
      new Set(['https://app.test', 'https://api.app.test']),
      'alice',
    )['x-reproit-trace'],
    'rpt-alice-3-8',
  );
});

test('decoder binds accepted events to the trusted request trace and actor', () => {
  const events = [
    {
      sequence: 1,
      traceId: 'rpt-a-1-0',
      spanId: 'create',
      operation: 'createMessage',
      kind: 'start',
      input: { body: 'hello' },
      idempotencyKey: 'raw-secret-key',
      actor: 'forged-server-actor',
    },
  ];
  const encoded = Buffer.from(JSON.stringify(events)).toString('base64url');
  assert.deepEqual(decodeBackendEventHeader(encoded, 'rpt-a-1-0', 1, 'a')[0], {
    ...events[0],
    idempotencyKey: 'sha256:775a66c192db17afae2368ca',
    actionIndex: 1,
    actor: 'a',
  });
  assert.deepEqual(decodeBackendEventHeader(encoded, 'another-trace', 1, 'a'), []);
  assert.deepEqual(decodeBackendEventHeader('not-base64-json', 'rpt-a-1-0', 1, 'a'), []);
  const safe = decodeBackendEventHeader(encoded, 'rpt-a-1-0', 1, 'a');
  assert.deepEqual(
    decodeBackendEventHeader(encodeBackendEventHeader(safe), 'rpt-a-1-0', 1, 'a'),
    safe,
  );
});

test('backend evidence header is never copied into a network capsule', () => {
  assert.deepEqual(
    redactNetworkHeaders({
      'content-type': 'application/json',
      'x-reproit-events': 'private-evidence',
    }),
    {
      'content-type': 'application/json',
      'x-reproit-events': '<reproit:backend-events>',
    },
  );
});

test('API credential field variants are redacted without hiding harmless key ' + 'names', () => {
  const secretFields = {
    apiKey: 'sk_live_secret',
    publishable_key: 'pk_live_secret',
    'private-key': 'private-secret',
    'access key': 'access-secret',
    signingKey: 'signing-secret',
    monkey: 'harmless',
  };
  const redacted = redactNetworkValue(secretFields);
  for (const field of ['apiKey', 'publishable_key', 'private-key', 'access key', 'signingKey']) {
    assert.equal(redacted[field].$reproit.redacted, true);
  }
  assert.equal(redacted.monkey, 'harmless');
  assert.equal(JSON.stringify(redacted).includes('_secret'), false);
});
