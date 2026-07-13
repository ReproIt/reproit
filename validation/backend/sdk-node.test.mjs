import assert from 'node:assert/strict';
import test from 'node:test';
import { beginBackendTrace } from './sdk-node.mjs';

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
  });
  trace.effect('write', { resource: 'messages', key: 'm1', tenant: 'team-a' });
  trace.finish({ id: 'm1' }, 201, true, true);
  const events = JSON.parse(Buffer.from(trace.header(), 'base64url').toString('utf8'));
  assert.equal(events.length, 3);
  assert.equal(events[0].traceId, 'rpt-a-1-0');
  assert.equal(events[0].actionIndex, 1);
  assert.equal(events[0].input.email, '<reproit:string:length=18>');
  assert.notEqual(events[0].idempotencyKey, 'retry-secret');
  assert.match(events[0].idempotencyKey, /^sha256:[0-9a-f]{24}$/);
  assert.equal(events[1].tenant, 'team-a');
  assert.equal(events[1].effectTenant, 'team-a');
  assert.equal(events[2].kind, 'return');
  assert.equal(events[2].effectsComplete, true);
});
