import assert from 'node:assert/strict';
import {mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';
import test from 'node:test';
import {A2UI_REPAIR_CONTRACT, fuzzVariants, parseA2uiText, validateMessages} from './a2ui-runner.mjs';

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

test('malformed component collections become findings instead of verifier crashes', () => {
  const invalid = {version: 'v0.9', updateComponents: {surfaceId: 'test', components: {invalid: true}}};
  const findings = validateMessages([create, invalid]);
  assert.ok(findings.some(finding => finding.kind === 'protocol-invalid'));
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

test('protocol findings include the exact legal component schema and repair contract', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-repair-context-'));
  try {
    const fixture = join(directory, 'invalid-button.json');
    const invalid = {version: 'v0.9', updateComponents: {surfaceId: 'test', components: [
      {id: 'label', component: 'Text', text: 'Submit'},
      {id: 'submit', component: 'Button', child: 'label', action: 'submit'},
    ]}};
    await writeFile(fixture, JSON.stringify([create, invalid]));
    const scan = spawnSync(process.execPath, [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture], {encoding: 'utf8'});
    assert.equal(scan.status, 1, scan.stderr);
    const report = JSON.parse(scan.stdout);
    const finding = report.findings.find(item => item.path.endsWith('.action'));
    assert.ok(finding);
    assert.equal(report.repairContract.catalogId, A2UI_REPAIR_CONTRACT.catalogId);
    assert.equal(finding.repairContext.component.type, 'Button');
    assert.ok(finding.repairContext.component.allowedProperties.includes('action'));
    assert.deepEqual(finding.repairContext.component.requiredProperties, ['id', 'component', 'child', 'action']);
    assert.equal(finding.repairContext.component.schema.properties.action.anyOf.length, 2);
    assert.ok(report.repairContract.prohibitedProperties.includes('ariaLabel'));
  } finally {
    await rm(directory, {recursive: true, force: true});
  }
});

test('protocol findings include the exact operation schema for invalid data-model updates', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-operation-context-'));
  try {
    const fixture = join(directory, 'invalid-data-model.json');
    const invalid = {version: 'v0.9', updateDataModel: {surfaceId: 'test', data: {name: 'Ada'}}};
    await writeFile(fixture, JSON.stringify([create, invalid]));
    const scan = spawnSync(process.execPath, [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture], {encoding: 'utf8'});
    assert.equal(scan.status, 1, scan.stderr);
    const report = JSON.parse(scan.stdout);
    const finding = report.findings.find(item => item.path === '1.updateDataModel');
    assert.ok(finding);
    const {message} = finding.repairContext;
    assert.equal(message.operation, 'updateDataModel');
    assert.deepEqual(message.operationAllowedProperties.sort(), ['path', 'surfaceId', 'value']);
    assert.deepEqual(message.operationRequiredProperties, ['surfaceId']);
    assert.ok(message.schema.properties.updateDataModel.properties.value);
    assert.equal(finding.repairContext.validPatchExamples[0].path, '1.updateDataModel');
  } finally {
    await rm(directory, {recursive: true, force: true});
  }
});

test('legacy wrapped components receive an exact flat-shape migration', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-legacy-context-'));
  try {
    const fixture = join(directory, 'legacy.json');
    const legacy = {version: 'v0.9', updateComponents: {surfaceId: 'test', components: [
      {id: 'heading', component: {Text: {text: 'Welcome', variant: 'h1'}}},
    ]}};
    await writeFile(fixture, JSON.stringify([create, legacy]));
    const scan = spawnSync(process.execPath, [join(import.meta.dirname, 'a2ui-runner.mjs'), 'scan', fixture], {encoding: 'utf8'});
    assert.equal(scan.status, 1, scan.stderr);
    const report = JSON.parse(scan.stdout);
    const context = report.findings.find(item => item.path === '1.updateComponents.components.0').repairContext;
    assert.equal(context.detectedShape, 'legacy-wrapped-component');
    assert.equal(context.component.type, 'Text');
    assert.deepEqual(context.validPatchExamples[0].value, {
      id: 'heading', component: 'Text', text: 'Welcome', variant: 'h1',
    });
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
    assert.equal(report.findings[0].repairContext.component.type, 'TextField');
    assert.equal(report.findings[0].repairContext.repairability, 'renderer-change-required');
    assert.equal(report.findings[0].repairContext.owner, '@a2ui/lit');
    assert.deepEqual(report.findings[0].repairContext.validPatchExamples, []);

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
