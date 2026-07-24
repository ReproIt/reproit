// Semantics parity tests against sdk/reproit-backend-rs/src/lib.rs.
'use strict';

const assert = require('node:assert');
const test = require('node:test');

const {
  BackendTrace,
  MAX_EVENTS,
  MAX_HEADER_BYTES,
  traceContextFromHeaders,
  selection,
  httpInput,
} = require('../index.js');

function context(overrides = {}) {
  return {
    traceId: 'trace-a',
    actor: null,
    actionIndex: 0,
    build: null,
    configContract: null,
    ...overrides,
  };
}

test('emits bounded correlated redacted events', () => {
  const headers = {
    'x-reproit-trace': 'trace-a',
    'x-reproit-actor': 'alice',
    'x-reproit-action': '7',
    'x-reproit-build': 'build-a',
    'x-reproit-config-contract': 'contract-a',
  };
  const parsed = traceContextFromHeaders((name) => headers[name]);
  const trace = BackendTrace.begin(parsed, 'createProject', {
    tenant: 'org-1',
    idempotencyKey: 'retry-secret',
    input: { name: 'demo', password: 'abcdefgh' },
    selections: [selection('project.id', 'projectId')],
  });
  trace.effect('write', { resource: 'projects', key: '1', tenant: 'org-1' });
  trace.finish(
    {
      id: 1,
      apiKey: 'sk_live_secret',
      publishable_key: 'pk_live_secret',
      'private-key': 'private-secret',
      'access key': 'access-secret',
      signingKey: 'signing-secret',
      monkey: 'harmless',
    },
    201,
    true,
    true,
  );
  assert.ok(trace.header().length < MAX_HEADER_BYTES);
  const events = trace.events();
  assert.strictEqual(events[0].actionIndex, 7);
  assert.strictEqual(events[0].build, 'build-a');
  assert.strictEqual(events[0].configContract, 'contract-a');
  assert.strictEqual(events[0].input.password.$reproit.length, 8);
  assert.notStrictEqual(events[0].idempotencyKey, 'retry-secret');
  assert.match(events[0].idempotencyKey, /^sha256:[0-9a-f]{24}$/);
  for (const field of ['apiKey', 'publishable_key', 'private-key', 'access key', 'signingKey']) {
    assert.strictEqual(events[2].output[field].$reproit.redacted, true);
  }
  assert.strictEqual(events[2].output.monkey, 'harmless');
  assert.strictEqual(events[2].effectsComplete, true);
});

test('stays inactive without a trace header', () => {
  assert.strictEqual(traceContextFromHeaders(() => undefined), null);
  assert.strictEqual(traceContextFromHeaders((n) => (n === 'x-reproit-trace' ? '  ' : null)), null);
});

test('header is unpadded base64url of the canonical event json', () => {
  const trace = BackendTrace.begin(context(), 'op', { input: { b: 1, a: 2 } });
  trace.finish({ ok: true }, 200, true, true);
  const header = trace.header();
  assert.doesNotMatch(header, /[+/=]/);
  const decoded = JSON.parse(Buffer.from(header, 'base64url').toString('utf8'));
  assert.deepStrictEqual(decoded, JSON.parse(JSON.stringify(trace.events())));
  // Keys are sorted (serde_json BTreeMap order in the Rust adapter).
  const raw = Buffer.from(header, 'base64url').toString('utf8');
  assert.ok(raw.indexOf('"a":2') < raw.indexOf('"b":1'));
});

test('rejects effects after return and a second return', () => {
  const trace = BackendTrace.begin(context(), 'op', { input: null });
  trace.finish(null, 200, true, false);
  assert.throws(() => trace.effect('read', {}), { code: 'AlreadyFinished' });
  assert.throws(() => trace.finish(null, 200, true, false), { code: 'AlreadyFinished' });
});

test('header before finish is rejected, oversized header is rejected', () => {
  const open = BackendTrace.begin(context(), 'op', { input: null });
  assert.throws(() => open.header(), { code: 'AlreadyFinished' });
  const big = BackendTrace.begin(context(), 'op', { input: null });
  big.finish({ blob: 'x'.repeat(MAX_HEADER_BYTES) }, 200, true, true);
  assert.throws(() => big.header(), { code: 'HeaderTooLarge' });
});

test('event count is capped at 256', () => {
  const trace = BackendTrace.begin(context(), 'op', { input: null });
  for (let i = 1; i < MAX_EVENTS; i++) trace.effect('emit', { event: 'tick' });
  assert.throws(() => trace.effect('emit', {}), { code: 'TooManyEvents' });
  assert.throws(() => trace.finish(null, 200, true, false), { code: 'TooManyEvents' });
});

test('typed effects only, bounded identifiers only', () => {
  const trace = BackendTrace.begin(context(), 'op', { input: null });
  assert.throws(() => trace.effect('mutate', {}), { code: 'InvalidOperation' });
  assert.throws(() => BackendTrace.begin(context(), '', {}), { code: 'InvalidOperation' });
  assert.throws(() => BackendTrace.begin(context(), 'x'.repeat(257), {}), {
    code: 'InvalidOperation',
  });
});

test('effect detail keeps only before, after, payload after redaction', () => {
  const trace = BackendTrace.begin(context(), 'op', { input: null });
  trace.effect('write', {
    resource: 'users',
    detail: { before: { email: 'a@b.c' }, after: { name: 'z' }, extra: 'dropped' },
  });
  const effect = trace.events()[1];
  assert.strictEqual(effect.before.email.$reproit.redacted, true);
  assert.strictEqual(effect.after.name, 'z');
  assert.strictEqual('extra' in effect, false);
});

test('canonical http input lowercases headers and preserves repeated values', () => {
  const input = httpInput({
    body: { name: 'demo' },
    path: { project: 'p1' },
    query: { tag: ['a', 'b'] },
    headers: { 'X-Mode': 'safe' },
  });
  assert.strictEqual(input.headers['x-mode'], 'safe');
  assert.deepStrictEqual(input.query.tag, ['a', 'b']);
  assert.deepStrictEqual(httpInput({ path: {}, query: {}, headers: {} }), {});
});

test('selections validate their paths', () => {
  assert.notStrictEqual(selection('project.id', 'projectId'), null);
  assert.notStrictEqual(selection('items[].id', 'rows[].id', 'Widget'), null);
  assert.strictEqual(selection('1bad', 'ok'), null);
  assert.strictEqual(selection('ok', 'ok', 'Bad.Condition'), null);
});
