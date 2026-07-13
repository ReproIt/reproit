#!/usr/bin/env node
import assert from 'node:assert/strict';
import {readFile, writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {chromium, firefox, webkit} from '../web/node_modules/playwright/index.mjs';
import {canonicalJson, capture, rendererMatrix, replay, sha256} from './adapter.mjs';

const checkout = process.argv[2];
if (!checkout) throw new Error('usage: live-renderer-harness.mjs <pinned-a2ui-checkout> [react-url] [lit-url]');
const reactUrl = process.argv[3] || 'http://127.0.0.1:4311';
const litUrl = process.argv[4] || 'http://127.0.0.1:4312';
const exampleName = '00_simple-login-form.json';
const examplesDir = join(checkout, 'specification/v0_9_1/catalogs/basic/examples');
const examplePath = join(examplesDir, exampleName);
const protocolPath = join(checkout, 'specification/v0_9_1/json/server_to_client.json');
const catalogPath = join(checkout, 'specification/v0_9_1/catalogs/basic/catalog.json');

function messagesFrom(document) {
  return Array.isArray(document) ? document : document.messages;
}

function componentMap(stream) {
  const components = new Map();
  for (const message of stream) {
    for (const component of message.updateComponents?.components || []) components.set(component.id, component);
  }
  return components;
}

// Convert the A2UI component graph into the same small semantic vocabulary used
// for live DOM. Text content, labels, CSS classes, element IDs, and pixels are
// deliberately absent: renderer comparison is structural and locale-independent.
function expectedStructure(stream) {
  const components = componentMap(stream);
  const out = [];
  const seen = new Set();
  const visit = id => {
    if (seen.has(id)) return;
    seen.add(id);
    const component = components.get(id);
    if (!component) return;
    if (component.component === 'Text' && /^h[1-6]$/.test(component.variant || '')) {
      out.push({kind: 'heading', level: Number(component.variant.slice(1))});
    } else if (component.component === 'TextField') {
      const variant = component.variant || 'shortText';
      if (variant === 'longText') out.push({kind: 'textarea', disabled: false});
      else out.push({kind: 'input', type: variant === 'obscured' ? 'password' : 'text', disabled: false, required: false});
    } else if (component.component === 'Button') {
      out.push({kind: 'button', disabled: false});
    } else if (component.component === 'CheckBox') {
      out.push({kind: 'input', type: 'checkbox', disabled: false, required: false});
    } else if (component.component === 'Image') {
      out.push({kind: 'img', disabled: false});
    }
    if (typeof component.child === 'string') visit(component.child);
    for (const child of component.children || []) if (typeof child === 'string') visit(child);
  };
  visit('root');
  return out;
}

function scanStructuralDom(root) {
  const out = [];
  const walk = node => {
    for (const element of node.children || []) {
      const tag = element.tagName.toLowerCase();
      if (/^h[1-6]$/.test(tag)) out.push({kind: 'heading', level: Number(tag[1])});
      else if (tag === 'input') out.push({kind: 'input', type: (element.type || 'text').toLowerCase(), disabled: element.disabled, required: element.required});
      else if (tag === 'button') out.push({kind: 'button', disabled: element.disabled});
      else if (tag === 'select' || tag === 'textarea' || tag === 'img') out.push({kind: tag, disabled: Boolean(element.disabled)});
      if (element.shadowRoot) walk(element.shadowRoot);
      walk(element);
    }
  };
  walk(root);
  return out;
}

async function exampleIndex() {
  const {readdir} = await import('node:fs/promises');
  const names = (await readdir(examplesDir)).filter(name => name.endsWith('.json')).sort();
  const index = names.indexOf(exampleName);
  assert.notEqual(index, -1, `official example ${exampleName} is missing`);
  return index;
}

async function observe(page, rendererName, index, seedRendererFault = false) {
  if (rendererName === '@a2ui/react') {
    await page.goto(reactUrl, {waitUntil: 'networkidle'});
    await page.locator('button[class*="navItem"]').nth(index).click();
  } else {
    await page.goto(litUrl, {waitUntil: 'networkidle'});
    await page.locator('local-gallery').locator('.nav-item').nth(index).click();
  }
  const surface = rendererName === '@a2ui/react'
    ? page.locator('[class*="surfaceContainer"]').first()
    : page.locator('local-gallery').locator('.surface-container');
  await surface.locator('input').first().waitFor();
  if (seedRendererFault) await surface.locator('input').first().evaluate(element => element.remove());
  return surface.evaluate(scanStructuralDom);
}

async function observeUntil(page, rendererName, index, expected) {
  let actual;
  for (let attempt = 0; attempt < 20; attempt++) {
    actual = await observe(page, rendererName, index);
    if (canonicalJson(actual) === canonicalJson(expected)) return actual;
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  return actual;
}

function capsuleFor({stream, renderer, actual, protocolDocument, catalogDocument, oracleExpected = 'obscured'}) {
  const args = {
    protocolVersion: 'v0.9', protocolDocument,
    catalog: {id: catalogDocument.catalogId, document: catalogDocument},
    stream, renderer,
    oracle: {kind: 'component-property', surfaceId: 'gallery-simple-login-form', componentId: 'password_field', path: '/variant', expected: oracleExpected},
  };
  const exactOracle = replay(capture(args));
  assert.notEqual(exactOracle.status, 'invalid', `official stream failed structural replay: ${exactOracle.protocolErrors?.join('; ')}`);
  // Live DOM supplies only framework-neutral structural evidence. Pass/fail is
  // authoritative from replay's exact oracle; comparing DOM to an independently
  // inferred expected tree here could silently substitute a different oracle.
  return capture({...args, observation: {status: exactOracle.status, oracleIdentity: exactOracle.oracleIdentity, structuralSha256: sha256(actual)}});
}

function assertOnly(matrix, field) {
  assert.equal(matrix.status, field ? 'fail' : 'pass');
  for (const name of ['protocolInvalidity', 'agentNondeterminism', 'rendererDivergence', 'appUiFailure']) {
    assert.equal(matrix[name].length, name === field ? 1 : 0, `${name} attribution was unexpected`);
  }
}

const originalText = await readFile(examplePath, 'utf8');
const baselineDocument = JSON.parse(originalText);
const baselineStream = messagesFrom(baselineDocument);
const expected = expectedStructure(baselineStream);
const protocolDocument = JSON.parse(await readFile(protocolPath, 'utf8'));
const catalogDocument = JSON.parse(await readFile(catalogPath, 'utf8'));
const index = await exampleIndex();
const browserTypes = {chromium, firefox, webkit};
const results = {};

async function withBrowser(browserType, operation) {
  const browser = await browserType.launch({headless: true});
  try {
    return await operation(browser);
  } finally {
    await browser.close();
  }
}

try {
  for (const [engine, browserType] of Object.entries(browserTypes)) {
    results[engine] = await withBrowser(browserType, async browser => {
      const reactPage = await browser.newPage();
      const litPage = await browser.newPage();
      const reactActual = await observe(reactPage, '@a2ui/react', index);
      const litActual = await observe(litPage, '@a2ui/lit', index);
      const platform = `web/${engine}`;
      const healthy = rendererMatrix([
        capsuleFor({stream: baselineStream, renderer: {name: '@a2ui/react', version: '0.10.1', platform}, actual: reactActual, protocolDocument, catalogDocument}),
        capsuleFor({stream: baselineStream, renderer: {name: '@a2ui/lit', version: '0.10.1', platform}, actual: litActual, protocolDocument, catalogDocument}),
      ]);
      assertOnly(healthy, null);

      const litFaultActual = await observe(litPage, '@a2ui/lit', index, true);
      const rendererFault = rendererMatrix([
        capsuleFor({stream: baselineStream, renderer: {name: '@a2ui/react', version: '0.10.1', platform}, actual: reactActual, protocolDocument, catalogDocument}),
        capsuleFor({stream: baselineStream, renderer: {name: '@a2ui/lit', version: '0.10.1', platform}, actual: litFaultActual, protocolDocument, catalogDocument}),
      ]);
      assertOnly(rendererFault, 'rendererDivergence');

      return {
        healthy: {react: reactActual, lit: litActual, oracleStatuses: healthy.runs.map(run => run.observation.status), matrix: healthy.status},
        seededRendererFault: {react: reactActual, lit: litFaultActual, oracleStatuses: rendererFault.runs.map(run => run.observation.status), attribution: 'rendererDivergence'},
      };
    });
  }

  // Seed a valid application-stream fault: the password field is declared as
  // ordinary short text. Both official renderers must agree on the wrong
  // structure, while the exact component-property oracle still expects
  // `obscured`. Vite reloads this source JSON into both explorers.
  const sharedDocument = structuredClone(baselineDocument);
  const password = componentMap(messagesFrom(sharedDocument)).get('password_field');
  password.variant = 'shortText';
  await writeFile(examplePath, JSON.stringify(sharedDocument, null, 2) + '\n');
  const sharedStream = messagesFrom(sharedDocument);
  const sharedExpected = expectedStructure(sharedStream);
  for (const [engine, browserType] of Object.entries(browserTypes)) {
    results[engine].seededSharedStreamFault = await withBrowser(browserType, async browser => {
      const reactPage = await browser.newPage();
      const litPage = await browser.newPage();
      const reactShared = await observeUntil(reactPage, '@a2ui/react', index, sharedExpected);
      const litShared = await observeUntil(litPage, '@a2ui/lit', index, sharedExpected);
      const platform = `web/${engine}`;
      const sharedFault = rendererMatrix([
        capsuleFor({stream: sharedStream, renderer: {name: '@a2ui/react', version: '0.10.1', platform}, actual: reactShared, protocolDocument, catalogDocument}),
        capsuleFor({stream: sharedStream, renderer: {name: '@a2ui/lit', version: '0.10.1', platform}, actual: litShared, protocolDocument, catalogDocument}),
      ]);
      assertOnly(sharedFault, 'appUiFailure');
      return {react: reactShared, lit: litShared, oracleStatuses: sharedFault.runs.map(run => run.observation.status), attribution: 'appUiFailure'};
    });
  }

  console.log(JSON.stringify({
    upstream: {commit: '96abfdc60de0657c6322028d10c1cc7bc25c237c', example: exampleName},
    expected,
    engines: results,
  }, null, 2));
} finally {
  await writeFile(examplePath, originalText);
}
