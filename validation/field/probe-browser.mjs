#!/usr/bin/env node

import { chromium } from '../../runners/web/node_modules/playwright/index.mjs';

const RUNS = 3;
const TIMEOUT_MS = 60_000;

function requireArgument(value, name) {
  if (!value)
    throw new Error(`missing ${name}`);
  return value;
}

async function probeVert(url) {
  const results = [];
  for (let run = 1; run <= RUNS; run += 1) {
    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();
    const exceptions = [];
    page.on('pageerror', error => exceptions.push(String(error)));
    const startedAt = performance.now();
    await page.goto(url, { waitUntil: 'networkidle', timeout: TIMEOUT_MS });
    await page.waitForTimeout(500);
    const body = await page.locator('body').innerText();
    results.push({
      run,
      cleanLaunch: true,
      finalUrl: page.url(),
      aboutContentPresent: body.includes('Why VERT?'),
      homeContentPresent: body.includes("The file converter you'll love."),
      exceptions,
      jsHeapMiB: await heapMiB(page),
      elapsedSeconds: elapsedSeconds(startedAt),
    });
    await browser.close();
  }
  return results;
}

async function probeSlidev(url, focus) {
  if (!['body', 'editor'].includes(focus))
    throw new Error('Slidev focus must be body or editor');
  const results = [];
  for (let run = 1; run <= RUNS; run += 1) {
    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();
    const exceptions = [];
    page.on('pageerror', error => exceptions.push(String(error)));
    const startedAt = performance.now();
    await page.goto(url, { waitUntil: 'networkidle', timeout: TIMEOUT_MS });
    const editor = page.locator('.monaco-editor').first();
    await editor.waitFor({ state: 'visible', timeout: 30_000 });
    if (focus === 'editor')
      await editor.click({ position: { x: 120, y: 40 } });
    else
      await page.locator('body').click({ position: { x: 5, y: 5 } });
    const activeElementBefore = await page.evaluate(() => ({
      tag: document.activeElement?.tagName,
      className: document.activeElement?.className,
    }));
    await page.keyboard.press('Space');
    await page.waitForTimeout(500);
    results.push({
      run,
      cleanLaunch: true,
      focus,
      activeElementBefore,
      finalUrl: page.url(),
      remainedOnSlide15: new URL(page.url()).pathname === '/15',
      exceptions: exceptions.filter(exception => !exception.includes('Wake Lock')),
      jsHeapMiB: await heapMiB(page),
      elapsedSeconds: elapsedSeconds(startedAt),
    });
    await browser.close();
  }
  return results;
}

async function heapMiB(page) {
  return page.evaluate(() =>
    Math.ceil((performance.memory?.usedJSHeapSize || 0) / 1024 / 1024));
}

function elapsedSeconds(startedAt) {
  return Number(((performance.now() - startedAt) / 1000).toFixed(3));
}

const mode = requireArgument(process.argv[2], 'mode');
const url = requireArgument(process.argv[3], 'url');
let results;
if (mode === 'vert')
  results = await probeVert(url);
else if (mode === 'slidev')
  results = await probeSlidev(url, process.argv[4] || 'editor');
else
  throw new Error(`unknown mode: ${mode}`);
process.stdout.write(`${JSON.stringify({ mode, chromium: await browserVersion(), results }, null, 2)}\n`);

async function browserVersion() {
  const browser = await chromium.launch({ headless: true });
  const version = browser.version();
  await browser.close();
  return version;
}
