import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import {
  ADK_A2UI_SUPPORT,
  extractAdkA2uiMessages,
  instrumentAdkA2ui,
  preflightAdkA2ui,
  validateOfficialAdkA2uiMessages,
  validateWithOfficialWebSdk,
} from './adk-a2a.mjs';

const catalogId = 'https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json';
const common = {
  protocolVersion: 'v0.9',
  protocolDocument: { $id: 'server_to_client.json' },
  catalog: { id: catalogId, document: { catalogId } },
  renderer: { name: 'official-adk-client', version: 'fixture', platform: 'web' },
  oracle: { kind: 'data-model-value', surfaceId: 'main', path: '/total', expected: 100 },
  validateMessages: () => [],
};

async function fixture() {
  return JSON.parse(
    await readFile(new URL('./fixtures/adk-a2a-events-v0.9.json', import.meta.url), 'utf8'),
  );
}

test(
  'official ADK fixture passes through unchanged and cumulative ' +
    'createSurface is captured once',
  async () => {
    const input = await fixture();
    const observed = instrumentAdkA2ui(input, common);
    const output = [];
    for await (const event of observed.events) output.push(event);
    const evidence = await observed.result;
    assert.deepEqual(output, input);
    assert.equal(evidence.replay.status, 'pass');
    assert.deepEqual(
      evidence.capsule.stream.map((item) => item.message),
      [input[0].status.message.parts[1].data, input[1].status.message.parts[1].data],
    );
  },
);

test('extracts message and status-update variants with both official MIME ' + 'spellings', () => {
  const create = { version: 'v0.9', createSurface: { surfaceId: 'main', catalogId } };
  assert.deepEqual(
    extractAdkA2uiMessages({
      kind: 'message',
      parts: [{ kind: 'data', data: create, mimeType: 'application/a2ui+json' }],
    }),
    [create],
  );
  assert.deepEqual(
    extractAdkA2uiMessages({
      kind: 'status-update',
      status: {
        message: {
          parts: [
            {
              kind: 'data',
              data: create,
              metadata: { mimeType: 'application/json+a2ui' },
            },
          ],
        },
      },
    }),
    [create],
  );
});

test('ignores ordinary A2A events, text parts, and non-A2UI data', () => {
  assert.deepEqual(extractAdkA2uiMessages({ kind: 'artifact-update' }), []);
  assert.deepEqual(
    extractAdkA2uiMessages({ kind: 'status-update', status: { state: 'working' } }),
    [],
  );
  assert.deepEqual(
    extractAdkA2uiMessages({
      kind: 'message',
      parts: [
        { kind: 'text', text: 'hello' },
        { kind: 'data', data: { answer: 42 }, mimeType: 'application/json' },
      ],
    }),
    [],
  );
});

test('unknown or ambiguous A2A envelopes fail closed', () => {
  const create = { version: 'v0.9', createSurface: { surfaceId: 'main', catalogId } };
  assert.throws(
    () => extractAdkA2uiMessages({ kind: 'custom', parts: [] }),
    /unsupported A2A event/,
  );
  assert.throws(
    () => extractAdkA2uiMessages({ kind: 'message', parts: [{ kind: 'data', data: create }] }),
    /no supported A2UI MIME/,
  );
  assert.throws(
    () =>
      extractAdkA2uiMessages({
        kind: 'message',
        parts: [
          {
            kind: 'data',
            data: create,
            mimeType: 'application\/a2ui+json',
            metadata: { mimeType: 'application\/json+a2ui' },
          },
        ],
      }),
    /conflicting mimeType/,
  );
  assert.throws(
    () =>
      extractAdkA2uiMessages({
        kind: 'message',
        parts: [
          {
            kind: 'text',
            text: 'bad',
            mimeType: 'application\/a2ui+json',
          },
        ],
      }),
    /not a data part/,
  );
  assert.throws(
    () =>
      extractAdkA2uiMessages({
        kind: 'message',
        parts: [
          {
            kind: 'data',
            data: [],
            mimeType: 'application\/a2ui+json',
          },
        ],
      }),
    /must contain one object/,
  );
});

test(
  'changed duplicate createSurface is not hidden by cumulative-event ' + 'deduplication',
  async () => {
    const input = await fixture();
    input[1].status.message.parts[0].data.createSurface.sendDataModel = true;
    const observed = instrumentAdkA2ui(input, common);
    for await (const _event of observed.events) {
      /* consume */
    }
    const evidence = await observed.result;
    assert.equal(evidence.replay.status, 'invalid');
    assert.match(evidence.replay.protocolErrors.join('\n'), /already exists/);
  },
);

test(
  'default validator is pinned to the official web SDK and fails closed ' + 'outside its contract',
  async () => {
    assert.equal(ADK_A2UI_SUPPORT.upstreamCommit, '96abfdc60de0657c6322028d10c1cc7bc25c237c');
    const result = await validateOfficialAdkA2uiMessages([], { protocolVersion: 'v0.9' });
    assert.match(JSON.stringify(result), /exact pinned v0.9 server_to_client/);
    assert.match(JSON.stringify(result), /exact official v0.9 basic catalog/);
    assert.deepEqual(await validateOfficialAdkA2uiMessages([], { protocolVersion: 'v1.0' }), [
      'unsupported ADK A2UI protocol version: v1.0',
    ]);
  },
);

test(
  'fixture messages pass the validator built from the pinned official ' + 'checkout',
  async (t) => {
    const checkout = process.env.A2UI_CHECKOUT ?? '/private/tmp/a2ui-research';
    let sdk;
    try {
      sdk = await import(`${checkout}/renderers/web_core/dist/src/v0_9/index.js`);
    } catch (error) {
      if (process.env.A2UI_CHECKOUT) throw error;
      t.skip(`pinned official SDK checkout is unavailable: ${error.message}`);
      return;
    }
    const input = await fixture();
    const messages = input.flatMap(extractAdkA2uiMessages);
    const catalog = JSON.parse(
      await readFile(`${checkout}/specification/v0_9/catalogs/basic/catalog.json`, 'utf8'),
    );
    const protocolDocument = JSON.parse(
      await readFile(`${checkout}/specification/v0_9/json/server_to_client.json`, 'utf8'),
    );
    assert.deepEqual(
      validateWithOfficialWebSdk(sdk, messages, {
        protocolVersion: 'v0.9',
        protocolDocument,
        catalog: { id: catalogId, document: catalog },
      }),
      [],
    );
    const unknown = [
      ...messages,
      {
        version: 'v0.9',
        updateComponents: {
          surfaceId: 'main',
          components: [{ id: 'mystery', component: 'NotInCatalog' }],
        },
      },
    ];
    assert.match(
      JSON.stringify(
        validateWithOfficialWebSdk(sdk, unknown, {
          protocolVersion: 'v0.9',
          protocolDocument,
          catalog: { id: catalogId, document: catalog },
        }),
      ),
      /unknown_catalog_component/,
    );
    const mutatedCatalog = structuredClone(catalog);
    mutatedCatalog.components.Text.description = 'same ID, changed contract';
    assert.match(
      JSON.stringify(
        validateWithOfficialWebSdk(sdk, messages, {
          protocolVersion: 'v0.9',
          protocolDocument,
          catalog: { id: catalogId, document: mutatedCatalog },
        }),
      ),
      /exact official v0.9 basic catalog/,
    );
    const mutatedProtocol = structuredClone(protocolDocument);
    mutatedProtocol.description = 'same ID, changed contract';
    assert.match(
      JSON.stringify(
        validateWithOfficialWebSdk(sdk, messages, {
          protocolVersion: 'v0.9',
          protocolDocument: mutatedProtocol,
          catalog: { id: catalogId, document: catalog },
        }),
      ),
      /exact pinned v0.9 server_to_client/,
    );
  },
);

test('buffered ADK preflight never delivers before the complete source ' + 'passes', async () => {
  const input = await fixture();
  let sourceComplete = false;
  const deliveries = [];
  async function* events() {
    for (const event of input) yield event;
    sourceComplete = true;
  }
  const result = await preflightAdkA2ui(events(), {
    ...common,
    deliver(messages) {
      assert.equal(sourceComplete, true);
      deliveries.push(messages);
    },
  });
  assert.equal(result.replay.status, 'pass');
  assert.equal(deliveries.length, 1);
  assert.deepEqual(deliveries[0], result.messages);
});

test('buffered ADK preflight never delivers an invalid or unverified stream', async () => {
  const input = await fixture();
  input[1].status.message.parts[1].data.updateDataModel.value = 99;
  const deliveries = [];
  await assert.rejects(
    preflightAdkA2ui(input, {
      ...common,
      maxRepairs: 0,
      deliver(messages) {
        deliveries.push(messages);
      },
    }),
    /failed preflight and no verified repair/,
  );
  assert.deepEqual(deliveries, []);
});

test(
  'buffered ADK preflight delivers only the independently verified ' + 'repaired messages',
  async () => {
    const input = await fixture();
    input[1].status.message.parts[1].data.updateDataModel.value = 99;
    const deliveries = [];
    const result = await preflightAdkA2ui(input, {
      ...common,
      maxRepairs: 1,
      repair({ messages, feedback }) {
        assert.equal(feedback.actual, 99);
        const repaired = structuredClone(messages);
        repaired.find((message) => message.updateDataModel).updateDataModel.value = 100;
        return repaired;
      },
      deliver(messages) {
        deliveries.push(messages);
      },
    });
    assert.equal(result.repairAttempts, 1);
    assert.equal(result.replay.status, 'pass');
    assert.deepEqual(deliveries, [result.messages]);
  },
);
