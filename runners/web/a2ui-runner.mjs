#!/usr/bin/env node
import {createHash} from 'node:crypto';
import {readFile, writeFile, mkdtemp, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {basename, dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {build} from 'esbuild';
import {chromium} from 'playwright';
import {A2uiMessageListSchema, BASIC_COMPONENTS} from '@a2ui/web_core/v0_9';

const CATALOG_ID = 'https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json';
const MESSAGE_KEYS = ['createSurface', 'updateComponents', 'updateDataModel', 'deleteSurface'];
const here = dirname(fileURLToPath(import.meta.url));

function canonical(value) {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`;
}

function sha256(value) {
  return createHash('sha256').update(typeof value === 'string' ? value : canonical(value)).digest('hex');
}

function parseArgs(args) {
  const config = {runs: 3, seed: 1};
  config.command = args.shift();
  config.target = args.shift();
  while (args.length) {
    const flag = args.shift();
    if (flag === '--output') config.output = args.shift();
    else if (flag === '--runs') config.runs = Number(args.shift());
    else if (flag === '--seed') config.seed = Number(args.shift());
    else if (flag === '--expect') config.expect = args.shift();
    else throw new Error(`unknown argument: ${flag}`);
  }
  if (!['scan', 'fuzz', 'replay'].includes(config.command) || !config.target) {
    throw new Error('usage: a2ui-runner.mjs scan|fuzz|replay <stream-or-finding.json> [--output report.json] [--runs N] [--seed N]');
  }
  if (!Number.isInteger(config.runs) || config.runs < 1 || config.runs > 100) throw new Error('--runs must be 1..100');
  if (!Number.isSafeInteger(config.seed) || config.seed < 0) throw new Error('--seed must be a non-negative integer');
  return config;
}

export function parseA2uiText(text) {
  const trimmed = text.trim();
  if (!trimmed) throw new Error('A2UI target is empty');
  try {
    const document = JSON.parse(trimmed);
    const messages = Array.isArray(document) ? document : document.messages;
    if (!Array.isArray(messages)) throw new Error('JSON target must be an array or contain a messages array');
    return {messages, document};
  } catch (jsonError) {
    const messages = trimmed.split(/\r?\n/).filter(Boolean).map((line, index) => {
      try { return JSON.parse(line); }
      catch (error) { throw new Error(`invalid JSONL at line ${index + 1}: ${error.message}`); }
    });
    return {messages, document: {messages}, jsonError: jsonError.message};
  }
}

export function validateMessages(messages) {
  const errors = [];
  const list = A2uiMessageListSchema.safeParse(messages);
  if (!list.success) {
    errors.push(...list.error.issues.map(issue => ({
      kind: 'protocol-invalid',
      path: issue.path.join('.'),
      reason: issue.message,
    })));
  }
  const componentSchemas = new Map(BASIC_COMPONENTS.map(api => [api.name, api.schema]));
  for (const [messageIndex, message] of messages.entries()) {
    if (!message || typeof message !== 'object' || Array.isArray(message)) continue;
    const keys = MESSAGE_KEYS.filter(key => Object.hasOwn(message, key));
    if (keys.length !== 1) errors.push({kind: 'protocol-invalid', path: String(messageIndex), reason: 'message must contain exactly one A2UI operation'});
    if (message.version !== 'v0.9') errors.push({kind: 'protocol-invalid', path: `${messageIndex}.version`, reason: 'only A2UI v0.9 is supported'});
    if (message.createSurface?.catalogId !== undefined && message.createSurface.catalogId !== CATALOG_ID) {
      errors.push({kind: 'protocol-invalid', path: `${messageIndex}.createSurface.catalogId`, reason: 'automatic scan supports the official v0.9 basic catalog'});
    }
    for (const [componentIndex, component] of (message.updateComponents?.components ?? []).entries()) {
      const schema = componentSchemas.get(component.component);
      const path = `${messageIndex}.updateComponents.components.${componentIndex}`;
      if (!schema) {
        errors.push({kind: 'protocol-invalid', path, reason: `unknown basic-catalog component ${String(component.component)}`});
        continue;
      }
      const {id: _id, component: _component, ...properties} = component;
      const parsed = schema.safeParse(properties);
      if (!parsed.success) errors.push(...parsed.error.issues.map(issue => ({
        kind: 'protocol-invalid', path: [path, ...issue.path].join('.'), reason: issue.message,
      })));
    }
  }
  return errors;
}

function validationReport(messages) {
  return validateMessages(messages).map(item => finding(item.kind, 'protocol', item.reason, {path: item.path}));
}

function minimizeInvalidMessages(original, signature) {
  let current = structuredClone(original);
  let attempts = 0;
  const reproduces = candidate => validationReport(candidate).some(item => item.signature === signature);
  let granularity = 2;
  while (current.length > 1 && attempts < 40) {
    const chunkSize = Math.ceil(current.length / granularity);
    let reduced = false;
    for (let start = 0; start < current.length && attempts < 40; start += chunkSize) {
      const candidate = current.filter((_, index) => index < start || index >= start + chunkSize);
      if (!candidate.length) continue;
      attempts++;
      if (reproduces(candidate)) {
        current = candidate;
        granularity = Math.max(2, granularity - 1);
        reduced = true;
        break;
      }
    }
    if (reduced) continue;
    if (granularity >= current.length) break;
    granularity = Math.min(current.length, granularity * 2);
  }
  for (let messageIndex = 0; messageIndex < current.length && attempts < 80; messageIndex++) {
    const components = current[messageIndex].updateComponents?.components;
    if (!Array.isArray(components) || components.length < 2) continue;
    let componentIndex = 0;
    while (componentIndex < current[messageIndex].updateComponents.components.length && attempts < 80) {
      const candidate = structuredClone(current);
      candidate[messageIndex].updateComponents.components.splice(componentIndex, 1);
      attempts++;
      if (reproduces(candidate)) current = candidate;
      else componentIndex++;
    }
  }
  return {messages: current, attempts};
}

function compact(messages) {
  const result = [];
  let pending;
  const flush = () => {
    if (!pending) return;
    result.push({
      version: pending.version,
      updateComponents: {
        ...pending.envelope,
        components: [...pending.components.values()],
      },
    });
    pending = undefined;
  };
  for (const message of messages) {
    const update = message.updateComponents;
    if (!update) {
      flush();
      result.push(structuredClone(message));
      continue;
    }
    if (!pending || pending.surfaceId !== update.surfaceId) {
      flush();
      const {components: _components, ...envelope} = structuredClone(update);
      pending = {version: message.version, surfaceId: update.surfaceId, envelope, components: new Map()};
    }
    for (const component of update.components ?? []) pending.components.set(component.id, structuredClone(component));
  }
  flush();
  return result;
}

function splitComponents(messages) {
  return messages.flatMap(message => {
    const components = message.updateComponents?.components;
    if (!Array.isArray(components) || components.length < 2) return [structuredClone(message)];
    return components.map(component => ({
      version: message.version,
      updateComponents: {...structuredClone(message.updateComponents), components: [structuredClone(component)]},
    }));
  });
}

function duplicateDataUpdates(messages) {
  return messages.flatMap(message => message.updateDataModel
    ? [structuredClone(message), structuredClone(message)]
    : [structuredClone(message)]);
}

export function fuzzVariants(messages, seed, runs) {
  const candidates = [
    {name: 'compacted', messages: compact(messages)},
    {name: 'split-components', messages: splitComponents(messages)},
    {name: 'repeated-data-updates', messages: duplicateDataUpdates(messages)},
  ];
  const variants = [];
  for (let index = 0; index < runs; index++) variants.push(structuredClone(candidates[(seed + index) % candidates.length]));
  return variants;
}

async function bundleHost(directory) {
  const output = join(directory, 'a2ui-host.js');
  await build({
    entryPoints: [join(here, 'a2ui-host.jsx')],
    outfile: output,
    bundle: true,
    format: 'iife',
    platform: 'browser',
    jsx: 'automatic',
    logLevel: 'silent',
  });
  return output;
}

function hasAccessibleName(snapshot) {
  return /\"(?:[^\"\\]|\\.)+\"/.test((snapshot.split('\n')[0] ?? '').trim());
}

async function renderOne(browser, bundle, messages, renderer) {
  const page = await browser.newPage();
  const pageErrors = [];
  page.on('pageerror', error => pageErrors.push(error.message));
  await page.setContent('<!doctype html><html><body><main id="reproit-a2ui-root"></main></body></html>');
  await page.evaluate(({messages, renderer}) => {
    window.__REPROIT_A2UI_MESSAGES__ = messages;
    window.__REPROIT_A2UI_RENDERER__ = renderer;
  }, {messages, renderer});
  await page.addScriptTag({path: bundle});
  await page.waitForFunction(() => window.__REPROIT_A2UI_READY__ === true);
  await page.waitForTimeout(25);
  const inputs = page.locator('input:not([type="hidden"]), textarea, select');
  const inputObservations = [];
  for (let index = 0; index < await inputs.count(); index++) {
    const input = inputs.nth(index);
    if (!await input.isVisible()) continue;
    const snapshot = (await input.ariaSnapshot()).normalize('NFKC');
    inputObservations.push({index, accessibleNamePresent: hasAccessibleName(snapshot), accessibilitySha256: sha256(snapshot)});
  }
  const buttons = page.getByRole('button');
  const buttonObservations = [];
  for (let index = 0; index < await buttons.count(); index++) {
    const button = buttons.nth(index);
    if (!await button.isVisible()) continue;
    const snapshot = (await button.ariaSnapshot()).normalize('NFKC');
    buttonObservations.push({index, accessibleNamePresent: hasAccessibleName(snapshot), accessibilitySha256: sha256(snapshot)});
  }
  const host = await page.evaluate(() => ({
    errors: [...(window.__REPROIT_A2UI_ERRORS__ ?? [])],
    actions: [...(window.__REPROIT_A2UI_ACTIONS__ ?? [])],
    renderedElements: document.querySelectorAll('*').length,
  }));
  await page.close();
  return {...host, errors: [...new Set([...host.errors, ...pageErrors])], inputs: inputObservations, buttons: buttonObservations};
}

function finding(kind, renderer, reason, detail = {}) {
  const signature = sha256({kind, renderer, reason, detail});
  return {kind, renderer, reason, signature, ...detail};
}

async function scanStream(browser, bundle, messages) {
  const observations = {};
  const findings = [];
  for (const renderer of ['react', 'lit']) {
    const observation = await renderOne(browser, bundle, messages, renderer);
    observations[renderer] = observation;
    for (const reason of observation.errors) findings.push(finding('renderer-error', renderer, reason));
    for (const input of observation.inputs.filter(input => !input.accessibleNamePresent)) {
      findings.push(finding('unlabeled-input', renderer, 'visible form control has no accessible name', {inputIndex: input.index}));
    }
    for (const button of observation.buttons.filter(button => !button.accessibleNamePresent)) {
      findings.push(finding('unlabeled-button', renderer, 'visible button has no accessible name', {buttonIndex: button.index}));
    }
  }
  return {observations, findings};
}

async function reproducesFinding(browser, bundle, messages, signature) {
  if (validateMessages(messages).length) return false;
  const result = await scanStream(browser, bundle, messages);
  return result.findings.some(item => item.signature === signature);
}

async function minimizeMessages(browser, bundle, original, signature) {
  let current = structuredClone(original);
  let attempts = 0;
  let granularity = 2;
  while (current.length > 1 && attempts < 40) {
    const chunkSize = Math.ceil(current.length / granularity);
    let reduced = false;
    for (let start = 0; start < current.length && attempts < 40; start += chunkSize) {
      const candidate = current.filter((_, index) => index < start || index >= start + chunkSize);
      if (!candidate.length) continue;
      attempts++;
      if (await reproducesFinding(browser, bundle, candidate, signature)) {
        current = candidate;
        granularity = Math.max(2, granularity - 1);
        reduced = true;
        break;
      }
    }
    if (reduced) continue;
    if (granularity >= current.length) break;
    granularity = Math.min(current.length, granularity * 2);
  }
  for (let messageIndex = 0; messageIndex < current.length && attempts < 80; messageIndex++) {
    const components = current[messageIndex].updateComponents?.components;
    if (!Array.isArray(components) || components.length < 2) continue;
    let componentIndex = 0;
    while (componentIndex < current[messageIndex].updateComponents.components.length && attempts < 80) {
      const candidate = structuredClone(current);
      candidate[messageIndex].updateComponents.components.splice(componentIndex, 1);
      attempts++;
      if (await reproducesFinding(browser, bundle, candidate, signature)) current = candidate;
      else componentIndex++;
    }
  }
  return {messages: current, attempts};
}

function equivalentObservation(observation) {
  return {
    errors: observation.errors,
    inputs: observation.inputs.map(input => ({accessibleNamePresent: input.accessibleNamePresent, accessibilitySha256: input.accessibilitySha256})),
    buttons: observation.buttons.map(button => ({accessibleNamePresent: button.accessibleNamePresent, accessibilitySha256: button.accessibilitySha256})),
  };
}

async function run(config) {
  const text = await readFile(config.target, 'utf8');
  let messages;
  let expected;
  if (config.command === 'replay') {
    const document = JSON.parse(text);
    if (document?.format !== 'reproit-a2ui-finding') throw new Error('replay target is not a Reproit A2UI finding');
    messages = document.messages;
    expected = document.finding;
  } else {
    messages = parseA2uiText(text).messages;
  }
  const validationFindings = validationReport(messages);
  if (validationFindings.length) {
    const findings = validationFindings.map(item => {
      const minimized = minimizeInvalidMessages(messages, item.signature);
      return {...item, minimalMessages: minimized.messages, shrinkAttempts: minimized.attempts};
    });
    if (config.command === 'replay') {
      const reproduced = findings.some(item => item.signature === (config.expect ?? expected?.signature));
      return {format: 'reproit-a2ui-replay', reproduced, expected, findings, observations: {}};
    }
    return {format: 'reproit-a2ui-run', command: config.command, target: basename(config.target), messagesSha256: sha256(messages), messages, findings, observations: {}};
  }
  const temporary = await mkdtemp(join(tmpdir(), 'reproit-a2ui-runner-'));
  let browser;
  try {
    const bundle = await bundleHost(temporary);
    browser = await chromium.launch({headless: true});
    const baseline = await scanStream(browser, bundle, messages);
    const findings = baseline.findings.map(item => ({...item, reproductionMessages: messages}));
    const variants = [];
    if (config.command === 'fuzz') {
      for (const variant of fuzzVariants(messages, config.seed, config.runs)) {
        const variantValidation = validateMessages(variant.messages);
        if (variantValidation.length) throw new Error(`internal ${variant.name} mutation is not schema-valid: ${variantValidation[0].reason}`);
        const result = await scanStream(browser, bundle, variant.messages);
        for (const renderer of ['react', 'lit']) {
          const before = equivalentObservation(baseline.observations[renderer]);
          const after = equivalentObservation(result.observations[renderer]);
          if (canonical(before) !== canonical(after)) {
            findings.push({...finding('metamorphic-divergence', renderer, `${variant.name} changed final structural behavior`, {variant: variant.name}), reproductionMessages: variant.messages});
          }
        }
        for (const item of result.findings) {
          if (!findings.some(existing => existing.signature === item.signature)) findings.push({...item, reproductionMessages: variant.messages});
        }
        variants.push({name: variant.name, messagesSha256: sha256(variant.messages), observations: result.observations});
      }
    }
    if (config.command === 'replay') {
      const reproduced = findings.some(item => item.signature === (config.expect ?? expected?.signature));
      return {format: 'reproit-a2ui-replay', reproduced, expected, findings, observations: baseline.observations};
    }
    const minimizedFindings = [];
    for (const item of findings) {
      const {reproductionMessages, ...publicFinding} = item;
      const minimized = await minimizeMessages(browser, bundle, reproductionMessages, item.signature);
      minimizedFindings.push({...publicFinding, minimalMessages: minimized.messages, shrinkAttempts: minimized.attempts});
    }
    return {format: 'reproit-a2ui-run', command: config.command, target: basename(config.target), seed: config.seed, runs: config.command === 'fuzz' ? config.runs : 0, messagesSha256: sha256(messages), messages, findings: minimizedFindings, observations: baseline.observations, variants};
  } finally {
    await browser?.close();
    await rm(temporary, {recursive: true, force: true});
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const config = parseArgs(process.argv.slice(2));
  run(config).then(async report => {
    const output = JSON.stringify(report, null, 2) + '\n';
    if (config.output) await writeFile(config.output, output);
    process.stdout.write(output);
    if (report.format === 'reproit-a2ui-replay' ? report.reproduced : report.findings.length > 0) process.exitCode = 1;
  }).catch(error => {
    console.error(`reproit-a2ui: ${error.stack ?? error.message}`);
    process.exitCode = 2;
  });
}
