#!/usr/bin/env node
import assert from 'node:assert/strict';
import { readdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { chromium, firefox, webkit } from '../web/node_modules/playwright/index.mjs';
import { canonicalJson, sha256 } from './adapter.mjs';
import { compactDocument } from './generate-official-fixtures.mjs';

const checkout = process.argv[2];
if (!checkout)
  throw new Error(
    'usage: official-fixture-renderer-harness.mjs <pinned-a2ui-checkout> ' +
      '[react-url] [lit-url]',
  );
const reactUrl = process.argv[3] || 'http://127.0.0.1:4311';
const litUrl = process.argv[4] || 'http://127.0.0.1:4312';
const examplesDir = join(checkout, 'specification/v0_9_1/catalogs/basic/examples');
const examples = (await readdir(examplesDir)).filter((name) => name.endsWith('.json')).sort();
assert(examples.length > 0, 'official example fixture set is empty');
const upstreamCommit = execFileSync('git', ['-C', checkout, 'rev-parse', 'HEAD'], {
  encoding: 'utf8',
}).trim();
const documents = new Map(
  await Promise.all(
    examples.map(async (name) => [
      name,
      JSON.parse(await readFile(join(examplesDir, name), 'utf8')),
    ]),
  ),
);

function buttonActionIdentities(document) {
  const components = new Map();
  for (const message of document.messages || document) {
    for (const component of message.updateComponents?.components || [])
      components.set(component.id, component);
  }
  const actions = [];
  const seen = new Set();
  const visit = (id) => {
    if (seen.has(id)) return;
    seen.add(id);
    const component = components.get(id);
    if (!component) return;
    if (component.component === 'Button') actions.push(sha256(component.action || null));
    if (typeof component.child === 'string') visit(component.child);
    for (const child of Array.isArray(component.children) ? component.children : [])
      if (typeof child === 'string') visit(child);
  };
  visit('root');
  return actions;
}

function scanSemanticStructure(root) {
  const result = [];
  let scanId = 0;
  const mark = (element) => {
    const id = String(scanId++);
    element.setAttribute('data-reproit-a11y-scan', id);
    return { scanId: id };
  };
  const walk = (node) => {
    for (const element of node.children || []) {
      const style = getComputedStyle(element);
      if (style.display === 'none' || style.visibility === 'hidden') continue;
      const tag = element.tagName.toLowerCase();
      if (/^h[1-6]$/.test(tag))
        result.push({ kind: 'heading', level: Number(tag[1]), ...mark(element) });
      else if (tag === 'input')
        result.push({
          kind: 'input',
          type: (element.type || 'text').toLowerCase(),
          disabled: element.disabled,
          required: element.required,
          ...mark(element),
        });
      else if (tag === 'button')
        result.push({ kind: 'button', disabled: element.disabled, ...mark(element) });
      else if (tag === 'select' || tag === 'textarea')
        result.push({
          kind: tag,
          disabled: element.disabled,
          required: element.required,
          ...mark(element),
        });
      else if (tag === 'img')
        result.push({ kind: 'img', fit: style.objectFit || 'fill', ...mark(element) });
      else if (tag === 'audio' || tag === 'video')
        result.push({ kind: tag, controls: element.controls });
      if (element.shadowRoot) walk(element.shadowRoot);
      walk(element);
    }
  };
  walk(root);
  return result;
}

function sealedStructure(raw, actions) {
  const buttons = raw.filter((item) => item.kind === 'button');
  const bindActions = buttons.length === actions.length;
  let buttonIndex = 0;
  return raw
    .map((item) => {
      const snapshot = String(item.ariaSnapshot || '')
        .normalize('NFKC')
        .trim()
        .replace(/[ \t]+/g, ' ');
      const firstLine = snapshot.split('\n')[0] || '';
      const sealed = {
        ...item,
        accessibleNamePresent: /"(?:[^"\\]|\\.)+"/.test(firstLine),
        accessibleDescriptionPresent: /(^|\n)\s*description:/.test(snapshot),
        accessibleSnapshotSha256: sha256(snapshot),
      };
      delete sealed.ariaSnapshot;
      delete sealed.scanId;
      if (item.kind === 'button') {
        if (bindActions) sealed.actionIdentity = actions[buttonIndex];
        buttonIndex++;
      }
      return sealed;
    })
    .sort((a, b) => canonicalJson(a).localeCompare(canonicalJson(b)));
}

async function openExplorer(page, renderer) {
  const url = renderer === 'react' ? reactUrl : litUrl;
  for (let attempt = 0; attempt < 5; attempt++) {
    try {
      await page.goto(url, { waitUntil: 'domcontentloaded' });
      break;
    } catch (error) {
      if (
        !/interrupted by another navigation|ERR_ABORTED/.test(String(error.message)) ||
        attempt === 4
      )
        throw error;
      await page.waitForTimeout(100);
    }
  }
  if (renderer === 'react') await page.locator('button[class*="navItem"]').first().waitFor();
  else await page.locator('local-gallery').locator('.nav-item').first().waitFor();
}

async function observe(page, renderer, index, actions) {
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
  await page.waitForTimeout(50);
  const raw = await surface.evaluate(scanSemanticStructure);
  for (const item of raw) {
    if (item.scanId !== undefined)
      item.ariaSnapshot = await surface
        .locator(`[data-reproit-a11y-scan="${item.scanId}"]`)
        .ariaSnapshot();
  }
  return sealedStructure(raw, actions);
}

const engines = { chromium, firefox, webkit };
const runGenerated = process.env.A2UI_SKIP_GENERATED !== '1';
const observations = {};
const compactedObservations = {};
const sacrificialName = '00_simple-text.json';
const sacrificialIndex = examples.indexOf(sacrificialName);
const sacrificialPath = join(examplesDir, sacrificialName);
const sacrificialText = await readFile(sacrificialPath, 'utf8');
for (const [engine, browserType] of Object.entries(engines)) {
  const browser = await browserType.launch({ headless: true });
  try {
    let reactPage = await browser.newPage();
    let litPage = await browser.newPage();
    await openExplorer(reactPage, 'react');
    await openExplorer(litPage, 'lit');
    observations[engine] = {};
    for (const [index, name] of examples.entries()) {
      const actions = buttonActionIdentities(documents.get(name));
      const react = await observe(reactPage, 'react', index, actions);
      const lit = await observe(litPage, 'lit', index, actions);
      observations[engine][name] = { react, lit };
    }
    compactedObservations[engine] = {};
    if (!runGenerated) continue;
    try {
      for (const name of examples) {
        const document = compactDocument(documents.get(name));
        const actions = buttonActionIdentities(document);
        await writeFile(sacrificialPath, JSON.stringify(document, null, 2) + '\n');
        await reactPage.close();
        await litPage.close();
        reactPage = await browser.newPage();
        litPage = await browser.newPage();
        await openExplorer(reactPage, 'react');
        await openExplorer(litPage, 'lit');
        const react = await observe(reactPage, 'react', sacrificialIndex, actions);
        const lit = await observe(litPage, 'lit', sacrificialIndex, actions);
        compactedObservations[engine][name] = { react, lit };
      }
    } finally {
      await writeFile(sacrificialPath, sacrificialText);
    }
  } finally {
    await browser.close();
  }
}

const findings = [];
for (const name of examples) {
  const variants = [];
  for (const engine of Object.keys(engines)) {
    for (const renderer of ['react', 'lit'])
      variants.push({ engine, renderer, structure: observations[engine][name][renderer] });
  }
  const groups = new Map();
  for (const variant of variants) {
    const key = canonicalJson(variant.structure);
    const group = groups.get(key) || { structure: variant.structure, implementations: [] };
    group.implementations.push(`${variant.renderer}/${variant.engine}`);
    groups.set(key, group);
  }
  if (groups.size > 1) findings.push({ example: name, groups: [...groups.values()] });
}

const metamorphicFindings = [];
for (const name of runGenerated ? examples : []) {
  for (const engine of Object.keys(engines)) {
    for (const renderer of ['react', 'lit']) {
      const official = observations[engine][name][renderer];
      const compacted = compactedObservations[engine][name][renderer];
      if (canonicalJson(official) !== canonicalJson(compacted))
        metamorphicFindings.push({ example: name, engine, renderer, official, compacted });
    }
  }
}

const report = {
  upstream: { repository: 'https://github.com/a2ui-project/a2ui', commit: upstreamCommit },
  fixtures: {
    sourceExamples: examples.length,
    generatedCompactedStreams: runGenerated ? examples.length : 0,
    streams: examples.length * (runGenerated ? 2 : 1),
    rendererExecutions: examples.length * 2 * (runGenerated ? 2 : 1) * Object.keys(engines).length,
  },
  evidenceSha256: sha256({ observations, compactedObservations }),
  findings,
  metamorphicFindings,
};
const reportJson = JSON.stringify(report, null, 2) + '\n';
if (process.env.A2UI_REPORT) await writeFile(process.env.A2UI_REPORT, reportJson);
console.log(reportJson.trimEnd());
if ((findings.length || metamorphicFindings.length) && process.env.A2UI_FIXTURE_SURVEY !== '1')
  process.exitCode = 1;
