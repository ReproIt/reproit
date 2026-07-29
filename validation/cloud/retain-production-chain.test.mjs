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
const REQUIRED_STAGES = [
  'reset',
  'production-signal',
  'cloud-ingestion',
  'local-materialization',
  'exact-local-reproduction',
  'direct-replay',
  'retention-and-deletion',
];

function contract(originKind = 'fixture') {
  return {
    targetId: 'web-chromium',
    originKind,
    revisions: {
      cli: `git:${'a'.repeat(40)}`,
      sdk: {
        name: 'reproit-web-protocol-v1',
        revision: `git:${'b'.repeat(40)}`,
      },
      application: `sha256:${'c'.repeat(64)}`,
    },
    local: {
      provider: 'reproit-occurrence-v1',
      trusted: true,
    },
    execution: {
      adapter: {
        kind: 'playwright',
        engine: 'chromium',
      },
      commands: REQUIRED_STAGES.map((stage) => ({
        stage,
        command: `run ${stage}`,
        assertions: [`${stage} passed`],
      })),
      reset: {
        command: 'create a clean workspace and disposable Cloud project',
        evidence: ['reset'],
      },
      cleanup: {
        command: 'delete the disposable Cloud project and workspace',
        evidence: ['retention-and-deletion'],
      },
    },
  };
}

function options(overrides = {}) {
  return {
    originSummary: ORIGIN,
    contract: contract(),
    ...overrides,
  };
}

async function harnessWork(overrides = {}) {
  const root = await mkdtemp(resolve(tmpdir(), 'reproit-prod-chain-'));
  const files = {
    'reset.json': JSON.stringify({ cleanWorkspace: true, disposableProject: true }),
    'project.json': JSON.stringify({
      appId: 'app_1234',
      apiKey: ACCOUNT_KEY,
      publishableKey: PUBLISHABLE_KEY,
    }),
    'hosted.json': JSON.stringify({
      base: 'https://cloud.reproit.example',
      projectId: 'app_1234',
      occurrenceId: 'run_release_gate_1234',
      bucketId: 'bkt_9999',
      ingestedFindings: 500,
    }),
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
    const record = await retainProductionChain(area.root, area.output, options());
    assert.equal(record.qualification, 'FixtureQualified');
    assert.deepEqual(record.missingRequiredStages, []);
    assert.deepEqual(record.qualificationBlockers, []);
    assert.match(record.chainSha256, /^sha256:[a-f0-9]{64}$/);
    for (const stage of record.stages) {
      if (!stage.present) continue;
      assert.match(stage.rawSha256, /^sha256:[a-f0-9]{64}$/);
      assert.match(stage.sanitizedSha256, /^sha256:[a-f0-9]{64}$/);
    }
    const stageIds = record.stages.map((stage) => stage.id);
    assert.deepEqual(stageIds, [
      'reset',
      'production-signal',
      'cloud-ingestion',
      'local-materialization',
      'exact-local-reproduction',
      'direct-replay',
      'reproduction-stderr',
      'retention-and-deletion',
    ]);
  } finally {
    await area.dispose();
  }
});

test('no live credential survives into the retained bytes or the record', async () => {
  const area = await harnessWork();
  try {
    await retainProductionChain(area.root, area.output, options());
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
      ...options(),
      qualification: 'IndependentQualified',
      contract: contract('independent-application'),
    });
    assert.equal(record.qualification, 'Unqualified');
    assert.deepEqual(record.missingRequiredStages, ['exact-local-reproduction']);
    const stage = record.stages.find((entry) => entry.id === 'exact-local-reproduction');
    assert.equal(stage.present, false);
  } finally {
    await area.dispose();
  }
});

test('cleanup evidence is required for qualification', async () => {
  const area = await harnessWork({ 'delete.json': null });
  try {
    const record = await retainProductionChain(area.root, area.output, options());
    assert.equal(record.qualification, 'Unqualified');
    assert.deepEqual(record.missingRequiredStages, ['retention-and-deletion']);
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
    const record = await retainProductionChain(area.root, area.output, options());
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
    const record = await retainProductionChain(area.root, area.output, options());
    const stage = record.stages.find((entry) => entry.id === 'exact-local-reproduction');
    assert.equal(stage.present, false);
    assert.match(stage.reason, /empty artifact/);
    assert.equal(record.qualification, 'Unqualified');
    assert.deepEqual(record.missingRequiredStages, ['exact-local-reproduction']);
  } finally {
    await area.dispose();
  }
});

test('a complete chain without exact revision bindings remains unqualified', async () => {
  const area = await harnessWork();
  try {
    const record = await retainProductionChain(area.root, area.output, {
      originSummary: ORIGIN,
    });
    assert.equal(record.qualification, 'Unqualified');
    assert.ok(record.qualificationBlockers.includes('contract'));
  } finally {
    await area.dispose();
  }
});

test('fixture evidence cannot claim independent qualification', async () => {
  const area = await harnessWork();
  try {
    const record = await retainProductionChain(area.root, area.output, options({
      qualification: 'IndependentQualified',
    }));
    assert.equal(record.qualification, 'Unqualified');
    assert.ok(record.qualificationBlockers.includes('originKind-for-qualification'));
  } finally {
    await area.dispose();
  }
});

test('a web target cannot claim a different Playwright engine', async () => {
  const area = await harnessWork();
  const mismatched = contract();
  mismatched.execution.adapter.engine = 'firefox';
  try {
    const record = await retainProductionChain(area.root, area.output, {
      ...options(),
      contract: mismatched,
    });
    assert.equal(record.qualification, 'Unqualified');
    assert.ok(record.qualificationBlockers.includes('execution.adapter.engine'));
  } finally {
    await area.dispose();
  }
});
