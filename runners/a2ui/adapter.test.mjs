import { readFile } from 'node:fs/promises';
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  capture,
  oracleIdentity,
  parseJsonl,
  rendererMatrix,
  replay,
  sha256,
  shrink,
  validateCapture,
} from './adapter.mjs';

const CATALOG_ID = 'https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json';
const protocolDocument = {
  $id: 'https://a2ui.org/specification/v0_9/server_to_client.json',
  oneOf: ['createSurface', 'updateComponents', 'updateDataModel', 'deleteSurface'],
};
const catalog = {
  id: CATALOG_ID,
  document: {
    catalogId: CATALOG_ID,
    components: { Text: {}, Column: {}, Card: {}, TextField: {}, Button: {} },
  },
};
const renderer = { name: '@a2ui/react', version: '0.10.1', platform: 'web' };

function message(key, value) {
  return { version: 'v0.9', [key]: value };
}

function failingCapture(overrides = {}) {
  const stream = overrides.stream || [
    message('createSurface', { surfaceId: 'main', catalogId: CATALOG_ID, sendDataModel: true }),
    message('updateComponents', {
      surfaceId: 'main',
      components: [
        { id: 'root', component: 'Column', children: ['title'] },
        { id: 'title', component: 'Text', text: 'Checkout' },
      ],
    }),
    message('updateDataModel', { surfaceId: 'main', path: '/unrelated', value: 42 }),
    message('updateDataModel', { surfaceId: 'main', path: '/checkout/total', value: 99 }),
    message('updateDataModel', { surfaceId: 'main', path: '/another', value: 'noise' }),
  ];
  return capture({
    protocolVersion: 'v0.9',
    protocolDocument,
    catalog,
    stream,
    renderer: overrides.renderer || renderer,
    clientDataSnapshots: overrides.clientDataSnapshots ?? [
      { sequence: 3, surfaceId: 'main', data: { checkout: { total: 99 } } },
    ],
    actions: [
      {
        sequence: 4,
        surfaceId: 'main',
        action: { name: 'submit', context: { total: 99 } },
        clientData: { checkout: { total: 99 } },
      },
    ],
    oracle: { kind: 'data-model-value', surfaceId: 'main', path: '/checkout/total', expected: 100 },
    agent: overrides.agent || { inputSha256: sha256('checkout prompt') },
    observation: overrides.observation,
  });
}

test(
  'captures hashes, ordered stream, renderer metadata, snapshots, actions,' +
    ' and exact oracle identity',
  () => {
    const capsule = failingCapture();
    assert.equal(capsule.catalog.sha256, sha256(catalog.document));
    assert.equal(capsule.protocolSha256, sha256(protocolDocument));
    assert.equal(capsule.streamSha256, sha256(capsule.stream.map((item) => item.message)));
    assert.deepEqual(
      capsule.stream.map((item) => item.sequence),
      [0, 1, 2, 3, 4],
    );
    assert.deepEqual(capsule.renderer, renderer);
    assert.equal(capsule.clientDataSnapshots[0].data.checkout.total, 99);
    assert.equal(capsule.actions[0].action.context.total, 99);
    assert.equal(capsule.oracle.identity, oracleIdentity(capsule.oracle));
  },
);

test('deterministically replays an official A2UI v0.9 contact-form stream', async () => {
  const jsonl = await readFile(
    new URL('./fixtures/official-contact-form.jsonl', import.meta.url),
    'utf8',
  );
  const capsule = capture({
    protocolVersion: 'v0.9',
    protocolDocument,
    catalog,
    stream: parseJsonl(jsonl),
    renderer,
    oracle: { kind: 'protocol-valid' },
  });
  const first = replay(capsule);
  const second = replay(capsule);
  assert.deepEqual(first, second);
  assert.equal(first.status, 'pass');
  assert.deepEqual(first.state, {});
});

test('rejects protocol-order, graph, and integrity failures separately', () => {
  const capsule = failingCapture();
  capsule.stream[0].message = message('updateDataModel', {
    surfaceId: 'missing',
    path: '/',
    value: {},
  });
  const result = replay(capsule);
  assert.equal(result.classification, 'protocol_invalidity');
  assert.match(result.protocolErrors.join('\n'), /stream hash mismatch/);
  assert.match(result.protocolErrors.join('\n'), /precedes createSurface/);
});

test('ddmin removes irrelevant messages only while the exact failing oracle ' + 'survives', () => {
  const capsule = failingCapture();
  const minimized = shrink(capsule);
  assert.ok(minimized.minimizedMessages < minimized.originalMessages);
  assert.equal(minimized.oracleIdentity, capsule.oracle.identity);
  const result = replay(minimized.capsule);
  assert.equal(result.status, 'fail');
  assert.equal(result.oracleIdentity, capsule.oracle.identity);
  assert.equal(result.actual, 99);
});

test('shrink never carries a renderer observation onto an unrendered ' + 'minimized stream', () => {
  const capsule = failingCapture({
    observation: { status: 'fail', structuralSha256: sha256('original renderer tree') },
  });
  const minimized = shrink(capsule).capsule;
  assert.equal(minimized.observation, undefined);
  const matrix = rendererMatrix([minimized]);
  assert.equal(matrix.runs[0].observation.structuralSha256, matrix.runs[0].result.stateSha256);
  assert.notEqual(matrix.runs[0].observation.structuralSha256, sha256('original renderer tree'));
});

test('renderer matrix separates protocol invalidity', () => {
  const invalid = failingCapture();
  invalid.stream[0].message.createSurface.catalogId = 'wrong';
  invalid.streamSha256 = sha256(invalid.stream.map((item) => item.message));
  const matrix = rendererMatrix([invalid]);
  assert.equal(matrix.protocolInvalidity.length, 1);
  assert.deepEqual(matrix.rendererDivergence, []);
});

test('renderer matrix separates same-stream renderer divergence', () => {
  const react = failingCapture({
    observation: { status: 'fail', structuralSha256: sha256('react tree') },
  });
  const lit = failingCapture({
    renderer: { name: '@a2ui/lit', version: '0.10.1', platform: 'web' },
    observation: { status: 'pass', structuralSha256: sha256('lit tree') },
  });
  const matrix = rendererMatrix([react, lit]);
  assert.equal(matrix.rendererDivergence.length, 1);
  assert.deepEqual(matrix.appUiFailure, []);
});

test('renderer matrix attributes the same exact failure on every renderer to ' + 'the app', () => {
  const structuralSha256 = sha256('same failing tree');
  const react = failingCapture({ observation: { status: 'fail', structuralSha256 } });
  const lit = failingCapture({
    renderer: { name: '@a2ui/lit', version: '0.10.1', platform: 'web' },
    observation: { status: 'fail', structuralSha256 },
  });
  const matrix = rendererMatrix([react, lit]);
  assert.equal(matrix.appUiFailure.length, 1);
  assert.deepEqual(matrix.rendererDivergence, []);
});

test('renderer matrix separates agent nondeterminism for the same input', () => {
  const first = failingCapture();
  const second = failingCapture({
    stream: [
      message('createSurface', { surfaceId: 'main', catalogId: CATALOG_ID }),
      message('updateDataModel', { surfaceId: 'main', path: '/checkout/total', value: 98 }),
    ],
    renderer: { name: '@a2ui/lit', version: '0.10.1', platform: 'web' },
  });
  const matrix = rendererMatrix([first, second]);
  assert.equal(matrix.agentNondeterminism.length, 1);
  assert.equal(matrix.agentNondeterminism[0].streamSha256.length, 2);
});

test('renderer observations are integrity-bound to the exact oracle', () => {
  const capsule = failingCapture({
    observation: { status: 'fail', structuralSha256: sha256('tree') },
  });
  assert.equal(capsule.observation.oracleIdentity, capsule.oracle.identity);
  capsule.observation.status = 'pass';
  assert.match(validateCapture(capsule).join('\n'), /capture evidence hash mismatch/);
});

test('capture rejects a renderer observation for a different oracle', () => {
  assert.throws(
    () =>
      failingCapture({
        observation: {
          status: 'fail',
          oracleIdentity: 'a2ui:wrong',
          structuralSha256: sha256('tree'),
        },
      }),
    /observation.oracleIdentity does not match/,
  );
});

test('capture rejects sparse arrays instead of normalizing holes', () => {
  const sparse = [];
  sparse.length = 1;
  assert.throws(() => failingCapture({ clientDataSnapshots: sparse }), /sparse array entry/);
});
