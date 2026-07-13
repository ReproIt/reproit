import assert from 'node:assert/strict';
import test from 'node:test';
import { backendHttpInput, beginBackendTrace } from './sdk-node.mjs';

test('HTTP parameter evidence is decoded and canonical', () => {
  assert.deepEqual(backendHttpInput({
    body: { name: 'demo' },
    path: { project: 'p1' },
    query: { tag: ['a', 'b'] },
    headers: { 'X-Mode': 'safe' },
  }), {
    body: { name: 'demo' },
    path: { project: 'p1' },
    query: { tag: ['a', 'b'] },
    headers: { 'x-mode': 'safe' },
  });
});

test('inactive requests have zero instrumentation behavior', () => {
  assert.equal(beginBackendTrace({}, { operation: 'createMessage' }), null);
});

test('trace emits redacted, correlated structural evidence', () => {
  const trace = beginBackendTrace({
    'x-reproit-trace': 'rpt-a-1-0',
    'x-reproit-actor': 'a',
    'x-reproit-action': '1',
  }, {
    operation: 'createMessage',
    tenant: 'team-a',
    idempotencyKey: 'retry-secret',
    input: { body: 'hello', email: 'person@example.com' },
    selections: [{ schemaPath: 'message.author.id', responsePath: 'message.owner.userId' }],
  });
  trace.effect('write', { resource: 'messages', key: 'm1', tenant: 'team-a' });
  trace.finish({
    id: 'm1',
    apiKey: 'sk_live_secret',
    publishable_key: 'pk_live_secret',
    'private-key': 'private-secret',
    'access key': 'access-secret',
    signingKey: 'signing-secret',
    monkey: 'harmless',
  }, 201, true, true);
  const events = JSON.parse(Buffer.from(trace.header(), 'base64url').toString('utf8'));
  assert.equal(events.length, 3);
  assert.equal(events[0].traceId, 'rpt-a-1-0');
  assert.equal(events[0].actionIndex, 1);
  assert.deepEqual(events[0].input.email, {
    $reproit: { redacted: true, type: 'string', length: 18 },
  });
  assert.notEqual(events[0].idempotencyKey, 'retry-secret');
  assert.match(events[0].idempotencyKey, /^sha256:[0-9a-f]{24}$/);
  assert.deepEqual(events[0].selections, [{
    schemaPath: 'message.author.id', responsePath: 'message.owner.userId',
  }]);
  assert.equal(events[1].tenant, 'team-a');
  assert.equal(events[1].effectTenant, 'team-a');
  assert.equal(events[2].kind, 'return');
  assert.equal(events[2].effectsComplete, true);
  for (const field of ['apiKey', 'publishable_key', 'private-key', 'access key', 'signingKey']) {
    assert.equal(events[2].output[field].$reproit.redacted, true);
  }
  assert.equal(events[2].output.monkey, 'harmless');
  for (const secret of ['sk_live_secret', 'pk_live_secret', 'private-secret', 'access-secret', 'signing-secret']) {
    assert.equal(JSON.stringify(events).includes(secret), false);
  }
});
