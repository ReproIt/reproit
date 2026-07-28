import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import test from 'node:test';
import { retainProductionChain } from './retain-production-chain.mjs';

const ORIGIN = 'hosted Cloud disposable project, strict protocol-v1 findings';
// Deliberately hyphenated so the fixtures still match the redaction patterns
// under test without resembling a real provider key to a secret scanner.
const ACCOUNT_KEY = 'sk_live_EXAMPLE-NOT-A-REAL-KEY';
const PUBLISHABLE_KEY = 'pk_live_EXAMPLE-NOT-A-REAL-KEY';

async function harnessWork(overrides = {}) {
  const root = await mkdtemp(resolve(tmpdir(), 'reproit-prod-chain-'));
  const files = {
    'project.json': JSON.stringify({
      appId: 'app_1234',
      apiKey: ACCOUNT_KEY,
      publishableKey: PUBLISHABLE_KEY,
    }),
    'hosted.json': JSON.stringify({ bucketId: 'bkt_9999', ingestedFindings: 500 }),
    'pull.log': `pulled bkt_9999 with ${ACCOUNT_KEY}\n`,
    'check.json': JSON.stringify({ outcome: 'fail', id: 'bkt_9999' }),
    'direct-replay.log': 'REPRODUCED: TypeError ReproitContractError\n',
    'delete.json': JSON.stringify({ deleted: true }),
    ...overrides,
  };
  for (const [name, body] of Object.entries(files)) {
    if (body === null) continue;
    await writeFile(join(root, name), body);
  }
  return {
    root,
    output: join(root, 'retained'),
    async dispose() {
      await rm(root, { recursive: true, force: true });
    },
  };
}

test('a complete chain is retained, digested, and qualified', async () => {
  const area = await harnessWork();
  try {
    const record = await retainProductionChain(area.root, area.output, {
      originSummary: ORIGIN,
    });
    assert.equal(record.qualification, 'FixtureQualified');
    assert.deepEqual(record.missingRequiredStages, []);
    assert.match(record.chainSha256, /^sha256:[a-f0-9]{64}$/);
    for (const stage of record.stages) {
      if (!stage.present) continue;
      assert.match(stage.rawSha256, /^sha256:[a-f0-9]{64}$/);
      assert.match(stage.sanitizedSha256, /^sha256:[a-f0-9]{64}$/);
    }
    const stageIds = record.stages.map((stage) => stage.id);
    assert.deepEqual(stageIds.slice(0, 5), [
      'production-signal',
      'cloud-ingestion',
      'local-materialization',
      'exact-local-reproduction',
      'direct-replay',
    ]);
  } finally {
    await area.dispose();
  }
});

test('no live credential survives into the retained bytes or the record', async () => {
  const area = await harnessWork();
  try {
    await retainProductionChain(area.root, area.output, { originSummary: ORIGIN });
    for (const name of ['project.json', 'pull.log', 'record.json']) {
      const body = await readFile(join(area.output, name), 'utf8');
      assert.ok(!body.includes(ACCOUNT_KEY), `${name} leaked the account key`);
      assert.ok(!body.includes(PUBLISHABLE_KEY), `${name} leaked the publishable key`);
    }
    const project = JSON.parse(await readFile(join(area.output, 'project.json'), 'utf8'));
    assert.equal(project.apiKey, '[REDACTED]');
    assert.equal(project.appId, 'app_1234', 'redaction must not destroy the record');
  } finally {
    await area.dispose();
  }
});

test('a missing required stage disqualifies the record instead of hiding', async () => {
  const area = await harnessWork({ 'check.json': null });
  try {
    const record = await retainProductionChain(area.root, area.output, {
      originSummary: ORIGIN,
      qualification: 'IndependentQualified',
    });
    assert.equal(record.qualification, 'Unqualified');
    assert.deepEqual(record.missingRequiredStages, ['exact-local-reproduction']);
    const stage = record.stages.find((entry) => entry.id === 'exact-local-reproduction');
    assert.equal(stage.present, false);
  } finally {
    await area.dispose();
  }
});

test('an optional stage may be absent without disqualifying the chain', async () => {
  const area = await harnessWork({ 'delete.json': null });
  try {
    const record = await retainProductionChain(area.root, area.output, {
      originSummary: ORIGIN,
    });
    assert.equal(record.qualification, 'FixtureQualified');
    assert.deepEqual(record.missingRequiredStages, []);
  } finally {
    await area.dispose();
  }
});

test('a record must describe where the production signal came from', async () => {
  const area = await harnessWork();
  try {
    await assert.rejects(
      retainProductionChain(area.root, area.output, {}),
      /originSummary must describe/,
    );
  } finally {
    await area.dispose();
  }
});

test('a malformed required stage is recorded, not thrown, and disqualifies', async () => {
  const area = await harnessWork({ 'hosted.json': `{"bucketId": "bkt_1", ${ACCOUNT_KEY}` });
  try {
    const record = await retainProductionChain(area.root, area.output, {
      originSummary: ORIGIN,
    });
    const stage = record.stages.find((entry) => entry.id === 'cloud-ingestion');
    assert.equal(stage.present, true);
    assert.equal(stage.malformed, true);
    assert.equal(record.qualification, 'Unqualified');
    assert.deepEqual(record.missingRequiredStages, ['cloud-ingestion']);
    const retained = await readFile(join(area.output, 'hosted.json'), 'utf8');
    assert.ok(!retained.includes(ACCOUNT_KEY), 'a malformed stage must still be redacted');
  } finally {
    await area.dispose();
  }
});

test('an empty artifact is a missing stage, not a retention crash', async () => {
  const area = await harnessWork({ 'check.json': '' });
  try {
    const record = await retainProductionChain(area.root, area.output, {
      originSummary: ORIGIN,
    });
    const stage = record.stages.find((entry) => entry.id === 'exact-local-reproduction');
    assert.equal(stage.present, false);
    assert.match(stage.reason, /empty artifact/);
    assert.equal(record.qualification, 'Unqualified');
    assert.deepEqual(record.missingRequiredStages, ['exact-local-reproduction']);
  } finally {
    await area.dispose();
  }
});
