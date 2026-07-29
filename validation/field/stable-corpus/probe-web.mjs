#!/usr/bin/env node

import { chromium, firefox, webkit } from '/opt/web/node_modules/playwright/index.mjs';

const TIMEOUT_MS = 60_000;
const browserTypes = { chromium, firefox, webkit };
const engine = process.argv[2];
const vertRoot = process.argv[3];
const slidevRoot = process.argv[4];
const browserType = browserTypes[engine];

if (!browserType || !vertRoot || !slidevRoot)
  throw new Error('usage: probe-web.mjs ENGINE VERT_ROOT SLIDEV_ROOT');

async function observe(url, action) {
  const browser = await browserType.launch({ headless: true });
  const page = await browser.newPage();
  const exceptions = [];
  page.on('pageerror', error => exceptions.push(String(error)));
  try {
    await page.goto(url, { waitUntil: 'networkidle', timeout: TIMEOUT_MS });
    const detail = await action(page);
    return {
      observationReached: true,
      cleanLaunch: exceptions.length === 0,
      exceptions,
      engine,
      browserVersion: browser.version(),
      finalUrl: page.url(),
      ...detail,
    };
  } finally {
    await browser.close();
  }
}

function verdict(observation, legalBehaviorObserved, legalBehavior) {
  return {
    ...observation,
    legalBehaviorObserved,
    legalBehavior,
    identity: legalBehaviorObserved ? null : 'known-good-behavior-misclassified',
  };
}

const vertAboutObservation = await observe(`${vertRoot}/about`, async page => {
  const body = await page.locator('body').innerText();
  return {
    aboutContentPresent: body.includes('Why VERT?'),
    homeContentPresent: body.includes("The file converter you'll love."),
  };
});
const vertAbout = verdict(
  vertAboutObservation,
  vertAboutObservation.aboutContentPresent
    && !vertAboutObservation.homeContentPresent,
  'the fixed direct About route rendered the exact About content',
);

const vertRootObservation = await observe(`${vertRoot}/`, async page => {
  const body = await page.locator('body').innerText();
  return {
    aboutContentPresent: body.includes('Why VERT?'),
    homeContentPresent: body.includes("The file converter you'll love."),
  };
});
const vertRootRoute = verdict(
  vertRootObservation,
  vertRootObservation.homeContentPresent
    && !vertRootObservation.aboutContentPresent,
  'the neighboring root route rendered its legal home content',
);

const slidevObservation = engine === 'chromium'
  ? await observe(`${slidevRoot}/15`, async page => {
      const editor = page.locator('.monaco-editor').first();
      await editor.waitFor({ state: 'visible', timeout: 30_000 });
      await page.locator('body').click({ position: { x: 5, y: 5 } });
      const activeElement = await page.evaluate(() => document.activeElement?.tagName);
      await page.keyboard.press('Space');
      await page.waitForTimeout(500);
      return {
        activeElement,
        legalBodyShortcutAdvanced:
          new URL(page.url()).pathname === '/16',
      };
    })
  : await observe(`${slidevRoot}/#/2`, async page => {
      const body = await page.locator('body').innerText();
      return {
        reachedSlide2: new URL(page.url()).hash === '#/2',
        slide2ContentPresent: body.includes('What is Slidev?'),
      };
    });
const slidevLegal = engine === 'chromium'
  ? slidevObservation.legalBodyShortcutAdvanced === true
  : slidevObservation.reachedSlide2 === true
    && slidevObservation.slide2ContentPresent === true;
const slidevAction = verdict(
  slidevObservation,
  slidevLegal,
  engine === 'chromium'
    ? 'Space with BODY focused advanced from slide 15 to slide 16'
    : 'directly opening slide 2 rendered the second slide',
);

process.stdout.write(`${JSON.stringify({
  engine,
  cases: { vertAbout, vertRootRoute, slidevAction },
}, null, 2)}\n`);
