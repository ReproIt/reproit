import assert from 'node:assert/strict';
import {mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';
import test from 'node:test';
import {fuzzVariants, parseA2uiText, validateMessages} from './a2ui-runner.mjs';

const create = {version: 'v0.9', createSurface: {surfaceId: 'test', catalogId: 'https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json'}};
const heading = {version: 'v0.9', updateComponents: {surfaceId: 'test', components: [{id: 'root', component: 'Text', text: 'Ready', variant: 'h2'}]}};

test('parses JSON, wrapper objects, and JSONL without language assumptions', () => {
  assert.deepEqual(parseA2uiText(JSON.stringify([create])).messages, [create]);
  assert.deepEqual(parseA2uiText(JSON.stringify({messages: [create]})).messages, [create]);
  assert.deepEqual(parseA2uiText(`${JSON.stringify(create)}\n${JSON.stringify(heading)}\n`).messages, [create, heading]);
});

test('official schemas reject invalid components and accept every fuzz mutation', () => {
  assert.deepEqual(validateMessages([create, heading]), []);
  const invalid = structuredClone(heading);
  invalid.updateComponents.components[0].component = 'ImaginaryWidget';
  assert.equal(validateMessages([create, invalid])[0].kind, 'protocol-invalid');
  for (const variant of fuzzVariants([create, heading], 0, 6)) {
    assert.deepEqual(validateMessages(variant.messages), [], variant.name);
  }
});

test('protocol findings shrink without launching a renderer and keep the exact signature', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-invalid-'));
  try {
    const fixture = join(directory, 'invalid.json');
    const invalid = {version: 'v0.9', updateComponents: {surfaceId: 'test', components: [
      {id: 'broken', component: 'ImaginaryWidget'},
      {id: 'irrelevant', component: 'Text', text: 'Remove me'},
    ]}};
    await writeFile(fixture, JSON.stringify([create, invalid]));
    const scan = spawnSync(process.execPath, [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture], {encoding: 'utf8'});
    assert.equal(scan.status, 1, scan.stderr);
    const report = JSON.parse(scan.stdout);
    const finding = report.findings.find(item => item.reason.includes('ImaginaryWidget'));
    assert.ok(finding);
    assert.ok(finding.shrinkAttempts > 0);
    assert.equal(finding.minimalMessages[1].updateComponents.components.length, 1);
  } finally {
    await rm(directory, {recursive: true, force: true});
  }
});

test('standalone host finds, shrinks, and exactly replays an official renderer bug', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-test-'));
  try {
    const fixture = join(directory, 'stream.json');
    const messages = [create, {version: 'v0.9', updateComponents: {surfaceId: 'test', components: [{id: 'root', component: 'TextField', label: 'Account', value: ''}]}}];
    await writeFile(fixture, JSON.stringify(messages));
    const scan = spawnSync(process.execPath, [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture], {encoding: 'utf8'});
    assert.equal(scan.status, 1, scan.stderr);
    const report = JSON.parse(scan.stdout);
    assert.deepEqual(report.findings.map(finding => [finding.kind, finding.renderer]), [['unlabeled-input', 'lit']]);
    assert.equal(report.findings[0].minimalMessages.length, 2);

    const artifact = join(directory, 'finding.json');
    await writeFile(artifact, JSON.stringify({
      format: 'reproit-a2ui-finding',
      version: 1,
      messages: report.findings[0].minimalMessages,
      finding: report.findings[0],
    }));
    const replay = spawnSync(process.execPath, [join(import.meta.dirname, 'a2ui-runner.mjs'), 'replay', artifact], {encoding: 'utf8'});
    assert.equal(replay.status, 1, replay.stderr);
    assert.equal(JSON.parse(replay.stdout).reproduced, true);

    const persisted = JSON.parse(await readFile(artifact, 'utf8'));
    assert.equal(persisted.finding.signature, report.findings[0].signature);
  } finally {
    await rm(directory, {recursive: true, force: true});
  }
});
