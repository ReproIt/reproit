import assert from 'node:assert/strict';
import test from 'node:test';
import { A2uiPreflightError, instrumentA2ui, preflightA2ui } from './integration.mjs';

const catalogId = 'https://a2ui.org/test/catalog.json';
const common = {
  protocolVersion: 'v0.9',
  protocolDocument: { $id: 'server_to_client.json' },
  catalog: { id: catalogId, document: { catalogId } },
  renderer: { name: 'GenUI', version: 'test-fixture', platform: 'flutter' },
  oracle: { kind: 'data-model-value', surfaceId: 'main', path: '/total', expected: 100 },
  validateMessages: async () => [],
};

function totalStream(value) {
  return [
    { version: 'v0.9', createSurface: { surfaceId: 'main', catalogId } },
    { version: 'v0.9', updateDataModel: { surfaceId: 'main', path: '/total', value } },
  ];
}

test(
  'wrapper preserves an ADK-like event stream and captures only extracted ' + 'A2UI messages',
  async () => {
    const input = [
      { id: 'text', a2ui: [] },
      {
        id: 'create',
        a2ui: [{ version: 'v0.9', createSurface: { surfaceId: 'main', catalogId } }],
      },
      {
        id: 'data',
        a2ui: [
          { version: 'v0.9', updateDataModel: { surfaceId: 'main', path: '/total', value: 99 } },
        ],
      },
    ];
    let callback;
    const { events, result } = instrumentA2ui(input, {
      ...common,
      extractMessages: (event) => event.a2ui,
      onResult: (evidence) => {
        callback = evidence;
      },
    });
    const observed = [];
    for await (const event of events) observed.push(event);
    const evidence = await result;
    assert.deepEqual(observed, input);
    assert.equal(evidence, callback);
    assert.equal(evidence.replay.status, 'fail');
    assert.equal(evidence.feedback.actual, 99);
    assert.equal(evidence.feedback.reproduction.minimizedMessages, 2);
    assert.equal(evidence.feedback.reproduction.messages.length, 2);
  },
);

test('wrapper stays transparent for a passing stream', async () => {
  const source = [
    { version: 'v0.9', createSurface: { surfaceId: 'main', catalogId } },
    { version: 'v0.9', updateDataModel: { surfaceId: 'main', path: '/total', value: 100 } },
  ];
  const { events, result } = instrumentA2ui(source, common);
  const output = [];
  for await (const event of events) output.push(event);
  const evidence = await result;
  assert.deepEqual(output, source);
  assert.equal(evidence.replay.status, 'pass');
  assert.equal(evidence.feedback, undefined);
});

test('capture-only sanitizer never changes yielded events', async () => {
  const source = [
    { version: 'v0.9', createSurface: { surfaceId: 'main', catalogId } },
    { version: 'v0.9', updateDataModel: { surfaceId: 'main', path: '/total', value: 100 } },
  ];
  const { events, result } = instrumentA2ui(source, {
    ...common,
    oracle: { ...common.oracle, expected: '[number]' },
    sanitizeMessage(message) {
      const safe = structuredClone(message);
      if (safe.updateDataModel) safe.updateDataModel.value = '[number]';
      return safe;
    },
  });
  const output = [];
  for await (const event of events) output.push(event);
  const evidence = await result;
  assert.deepEqual(output, source);
  assert.equal(evidence.capsule.stream[1].message.updateDataModel.value, '[number]');
  assert.equal(evidence.replay.status, 'pass');
});

test('partial consumption rejects incomplete evidence', async () => {
  const source = [
    { version: 'v0.9', createSurface: { surfaceId: 'main', catalogId } },
    { version: 'v0.9', updateDataModel: { surfaceId: 'main', path: '/total', value: 100 } },
  ];
  const observed = instrumentA2ui(source, common);
  for await (const _event of observed.events) break;
  await assert.rejects(observed.result, /not fully consumed/);
});

test('stream observation fails closed without an exact protocol and catalog ' + 'validator', () => {
  assert.throws(
    () => instrumentA2ui(totalStream(100), { ...common, validateMessages: undefined }),
    new RegExp(
      'observer requires an exact protocol and catalog validateMessages ' + 'callback',
      '',
    ),
  );
});

test('preflight releases an already-passing stream once and in source order', async () => {
  const source = totalStream(100);
  const releases = [];
  let repairs = 0;
  const result = await preflightA2ui(source, {
    ...common,
    repair() {
      repairs++;
    },
    release(messages) {
      releases.push(messages);
    },
  });
  assert.equal(result.replay.status, 'pass');
  assert.equal(result.repairAttempts, 0);
  assert.equal(repairs, 0);
  assert.deepEqual(result.messages, source);
  assert.deepEqual(releases, [source]);
});

test(
  'preflight supplies exact minimized feedback and releases only an ' +
    'independently verified repair',
  async () => {
    const releases = [];
    const result = await preflightA2ui(totalStream(99), {
      ...common,
      maxRepairs: 1,
      repair({ attempt, feedback, oracleIdentity }) {
        assert.equal(attempt, 1);
        assert.equal(feedback.code, 'REPROIT_A2UI_FAILURE');
        assert.equal(feedback.actual, 99);
        assert.equal(feedback.oracle.identity, oracleIdentity);
        assert.equal(feedback.reproduction.minimizedMessages, 2);
        return totalStream(100);
      },
      release(messages) {
        releases.push(messages);
      },
    });
    assert.equal(result.replay.status, 'pass');
    assert.equal(result.repairAttempts, 1);
    assert.deepEqual(releases, [totalStream(100)]);
  },
);

test('preflight rejects an unverified repair without releasing it', async () => {
  const releases = [];
  await assert.rejects(
    preflightA2ui(totalStream(99), {
      ...common,
      maxRepairs: 1,
      repair: () => totalStream(98),
      release(messages) {
        releases.push(messages);
      },
    }),
    (error) => {
      assert.ok(error instanceof A2uiPreflightError);
      assert.equal(error.detail.repairAttempts, 1);
      assert.equal(error.detail.lastReplay.status, 'fail');
      return true;
    },
  );
  assert.deepEqual(releases, []);
});

test('preflight stops at the repair bound and never releases failed ' + 'candidates', async () => {
  const releases = [];
  const attempts = [];
  await assert.rejects(
    preflightA2ui(totalStream(99), {
      ...common,
      maxRepairs: 2,
      repair({ attempt }) {
        attempts.push(attempt);
        return totalStream(99 - attempt);
      },
      release(messages) {
        releases.push(messages);
      },
    }),
    (error) => error instanceof A2uiPreflightError && error.detail.repairAttempts === 2,
  );
  assert.deepEqual(attempts, [1, 2]);
  assert.deepEqual(releases, []);
});

test('preflight never partially releases a source that fails during ' + 'buffering', async () => {
  const releases = [];
  let repairs = 0;
  async function* interrupted() {
    yield totalStream(100)[0];
    throw new Error('transport interrupted');
  }
  await assert.rejects(
    preflightA2ui(interrupted(), {
      ...common,
      repair() {
        repairs++;
      },
      release(messages) {
        releases.push(messages);
      },
    }),
    /transport interrupted/,
  );
  assert.equal(repairs, 0);
  assert.deepEqual(releases, []);
});

test('preflight fails closed without an exact protocol and catalog validator', async () => {
  await assert.rejects(
    preflightA2ui(totalStream(100), {
      ...common,
      validateMessages: undefined,
    }),
    /requires an exact protocol and catalog validateMessages callback/,
  );
});

test(
  'preflight rejects a component outside the exact catalog without ' + 'releasing it',
  async () => {
    const releases = [];
    const source = [
      { version: 'v0.9', createSurface: { surfaceId: 'main', catalogId } },
      {
        version: 'v0.9',
        updateComponents: {
          surfaceId: 'main',
          components: [{ id: 'root', component: 'DefinitelyNotInCatalog' }],
        },
      },
    ];
    await assert.rejects(
      preflightA2ui(source, {
        ...common,
        oracle: { kind: 'protocol-valid' },
        validateMessages(messages, { catalog }) {
          const allowed = new Set(Object.keys(catalog.document.components || {}));
          const invalid = messages
            .flatMap((message) => message.updateComponents?.components || [])
            .find((component) => !allowed.has(component.component));
          return invalid ? [`component ${invalid.component} is not in catalog`] : [];
        },
        catalog: { id: catalogId, document: { components: { Column: {} } } },
        release(messages) {
          releases.push(messages);
        },
      }),
      (error) =>
        error instanceof A2uiPreflightError &&
        error.detail.lastReplay.protocolErrors[0] ===
          'component DefinitelyNotInCatalog is not in catalog',
    );
    assert.deepEqual(releases, []);
  },
);

test('preflight rejects non-JSON values instead of verifying a normalized ' + 'copy', async () => {
  const releases = [];
  await assert.rejects(
    preflightA2ui(totalStream(Number.NaN), {
      ...common,
      oracle: { ...common.oracle, expected: null },
      release(messages) {
        releases.push(messages);
      },
    }),
    /finite JSON numbers/,
  );
  assert.deepEqual(releases, []);
});

test(
  'preflight shrinking rejects reductions that violate the exact catalog ' + 'validator',
  async () => {
    const source = [
      { version: 'v0.9', createSurface: { surfaceId: 'main', catalogId } },
      {
        version: 'v0.9',
        updateComponents: {
          surfaceId: 'main',
          components: [{ id: 'root', component: 'Column', children: [] }],
        },
      },
      { version: 'v0.9', updateDataModel: { surfaceId: 'main', path: '/total', value: 99 } },
    ];
    await assert.rejects(
      preflightA2ui(source, {
        ...common,
        maxRepairs: 0,
        validateMessages(messages) {
          return messages.some((message) => message.updateComponents)
            ? []
            : ['catalog requires a root component update'];
        },
      }),
      (error) => {
        assert.ok(error instanceof A2uiPreflightError);
        assert.equal(error.detail.feedback.reproduction.originalMessages, 3);
        assert.equal(error.detail.feedback.reproduction.minimizedMessages, 3);
        return true;
      },
    );
  },
);
