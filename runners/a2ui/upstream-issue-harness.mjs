#!/usr/bin/env node
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { chromium } from '../web/node_modules/playwright/index.mjs';
import { capture, replay, sha256 } from './adapter.mjs';
import { validateMessageDirectory } from './generate-official-fixtures.mjs';

const checkout = process.argv[2];
if (!checkout)
  throw new Error('usage: upstream-issue-harness.mjs <checkout> [react-url] [lit-url]');
const reactUrl = process.argv[3] || 'http://127.0.0.1:4311';
const litUrl = process.argv[4] || 'http://127.0.0.1:4312';
const root = new URL('.', import.meta.url);
const upstreamCommit = process.env.A2UI_EXPECTED_COMMIT;
if (!upstreamCommit) throw new Error('A2UI_EXPECTED_COMMIT is required for known-issue provenance');
const examplesDir = join(checkout, 'specification/v0_9_1/catalogs/basic/examples');
const names = (await readdir(examplesDir)).filter((name) => name.endsWith('.json')).sort();
const sacrificial = '00_simple-text.json';
const index = names.indexOf(sacrificial);
const target = join(examplesDir, sacrificial);
const original = await readFile(target, 'utf8');
const protocolDocument = JSON.parse(
  await readFile(join(checkout, 'specification/v0_9_1/json/server_to_client.json'), 'utf8'),
);
const catalogDocument = JSON.parse(
  await readFile(join(checkout, 'specification/v0_9_1/catalogs/basic/catalog.json'), 'utf8'),
);
const validationRoot = await mkdtemp(join(tmpdir(), 'a2ui-known-issues-'));

async function loadFixture(name) {
  const fixture = JSON.parse(await readFile(new URL(`fixtures/${name}`, root), 'utf8'));
  const result = replay(
    capture({
      protocolVersion: 'v0.9',
      protocolDocument,
      catalog: { id: catalogDocument.catalogId, document: catalogDocument },
      stream: fixture.messages,
      renderer: { name: 'issue-probe', version: '1', platform: 'web' },
      oracle: { kind: 'protocol-valid' },
    }),
  );
  assert.equal(result.status, 'pass', result.protocolErrors?.join('; '));
  const messagesDirectory = join(validationRoot, name.replace(/\.json$/, ''));
  await mkdir(messagesDirectory);
  await Promise.all(
    fixture.messages.map((message, index) =>
      writeFile(join(messagesDirectory, `${index}.json`), JSON.stringify(message)),
    ),
  );
  await validateMessageDirectory(checkout, messagesDirectory, catalogDocument);
  return fixture;
}

async function pageFor(browser, renderer) {
  const page = await browser.newPage();
  await page.goto(renderer === 'react' ? reactUrl : litUrl, { waitUntil: 'domcontentloaded' });
  const nav =
    renderer === 'react'
      ? page.locator('button[class*="navItem"]').nth(index)
      : page.locator('local-gallery').locator('.nav-item').nth(index);
  await nav.click();
  const surface =
    renderer === 'react'
      ? page.locator('[class*="surfaceContainer"]').first()
      : page.locator('local-gallery').locator('.surface-container');
  await surface.waitFor();
  return { page, surface };
}

async function replaceWith(fixture) {
  await writeFile(target, JSON.stringify(fixture, null, 2) + '\n');
  await new Promise((resolve) => setTimeout(resolve, 150));
}

const browser = await chromium.launch({ headless: true });
const report = {
  upstream: { repository: 'https://github.com/a2ui-project/a2ui', commit: upstreamCommit },
  knownIssues: [],
  newFindings: [],
};
try {
  const accessibility = await loadFixture('upstream-1410-accessible-action-purpose.json');
  await replaceWith(accessibility);
  const actionIdentities = accessibility.messages[1].updateComponents.components
    .filter((component) => component.component === 'Button')
    .map((component) => sha256(component.action));
  const accessibilityRuns = {};
  for (const renderer of ['react', 'lit']) {
    const { page, surface } = await pageFor(browser, renderer);
    await surface.locator('button').first().waitFor();
    const snapshots = await surface
      .locator('button')
      .all()
      .then((buttons) => Promise.all(buttons.map((button) => button.ariaSnapshot())));
    await page.close();
    accessibilityRuns[renderer] = snapshots.map((snapshot, button) => ({
      accessibleSnapshotSha256: sha256(snapshot.normalize('NFKC')),
      accessibleNamePresent: /"(?:[^"\\]|\\.)+"/.test(snapshot.split('\n')[0] || ''),
      accessibleDescriptionPresent: /(^|\n)\s*description:/.test(snapshot),
      actionIdentity: actionIdentities[button],
    }));
  }
  for (const renderer of ['react', 'lit']) {
    const runs = accessibilityRuns[renderer];
    assert.equal(runs.length, 2);
    assert.equal(runs[0].accessibleSnapshotSha256, runs[1].accessibleSnapshotSha256);
    assert.notEqual(runs[0].actionIdentity, runs[1].actionIdentity);
    assert.equal(
      runs.some((run) => run.accessibleDescriptionPresent),
      false,
    );
  }
  report.knownIssues.push({
    issue: 1410,
    status: 'reproduced',
    schemaValid: true,
    fixture: 'upstream-1410-accessible-action-purpose.json',
    renderers: accessibilityRuns,
  });

  const image = await loadFixture('upstream-1298-image-default.json');
  await replaceWith(image);
  const imageRuns = {};
  for (const renderer of ['react', 'lit']) {
    const { page, surface } = await pageFor(browser, renderer);
    const img = surface.locator('img').first();
    await img.waitFor();
    imageRuns[renderer] = await img.evaluate((element) => {
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return {
        fit: style.objectFit,
        width: rect.width,
        height: rect.height,
        cssWidth: style.width,
        cssHeight: style.height,
      };
    });
    await page.close();
  }
  const imageDiverges = JSON.stringify(imageRuns.react) !== JSON.stringify(imageRuns.lit);
  assert.equal(imageDiverges, true, 'pinned A2UI #1298 probe no longer reproduces');
  report.knownIssues.push({
    issue: 1298,
    status: 'reproduced',
    schemaValid: true,
    fixture: 'upstream-1298-image-default.json',
    renderers: imageRuns,
  });

  const textField = await loadFixture('discovered-lit-text-field-label.json');
  await replaceWith(textField);
  const textFieldRuns = {};
  for (const renderer of ['react', 'lit']) {
    const { page, surface } = await pageFor(browser, renderer);
    const input = surface.locator('input').first();
    await input.waitFor();
    const snapshot = (await input.ariaSnapshot()).normalize('NFKC');
    const association = await input.evaluate((element) => ({
      id: element.id || null,
      name: element.getAttribute('name'),
      associatedLabelCount: element.labels?.length || 0,
      associatedLabelFor: [...(element.labels || [])].map((label) => label.htmlFor || null),
    }));
    textFieldRuns[renderer] = {
      accessibleNamePresent: /"(?:[^"\\]|\\.)+"/.test(snapshot.split('\n')[0] || ''),
      accessibleSnapshotSha256: sha256(snapshot),
      ...association,
    };
    await page.close();
  }
  assert.equal(textFieldRuns.react.accessibleNamePresent, true);
  assert.equal(textFieldRuns.lit.accessibleNamePresent, false);
  assert.equal(textFieldRuns.lit.associatedLabelCount, 0);
  report.newFindings.push({
    id: 'lit-text-field-label-association',
    status: 'reproduced',
    schemaValid: true,
    fixture: 'discovered-lit-text-field-label.json',
    affectedRenderer: 'lit/v0.9',
    renderers: textFieldRuns,
  });
} finally {
  await writeFile(target, original);
  await browser.close();
}

const reportJson = JSON.stringify(report, null, 2) + '\n';
if (process.env.A2UI_REPORT) await writeFile(process.env.A2UI_REPORT, reportJson);
console.log(reportJson.trimEnd());
