#!/usr/bin/env node

// Tauri Linux field-campaign probe.
//
// Same control contract as probe-electron.mjs: the campaign adapter runs
// launch, readiness, trigger, and observe as separate bounded phases, so
// `serve` owns the application for the whole run and each phase calls `ask`
// with one verb. Only the transport differs. Tauri renders in the system
// webview, which is driven over W3C WebDriver through tauri-driver rather than
// over CDP, so every observation goes through `browser.execute` against the
// live DOM.
//
// usage:
//   probe-tauri.mjs serve --app PATH --scenario ID --webdriverio PATH
//                         [--driver-url URL] [--variant ID] [--port N]
//   probe-tauri.mjs ask VERB [--port N]
//
// verbs: readiness, trigger, control, observe, shutdown

import { createServer } from 'node:http';

const DEFAULT_PORT = 8931;
const DEFAULT_DRIVER_URL = 'http://127.0.0.1:4444';
const READY_TIMEOUT_MS = 120_000;
const SETTLE_MS = 1_500;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function option(name, fallback = null) {
  const index = process.argv.indexOf(`--${name}`);
  if (index < 0) return fallback;
  const value = process.argv[index + 1];
  if (typeof value !== 'string' || value.startsWith('--')) {
    throw new Error(`--${name} requires a value`);
  }
  return value;
}

function requireOption(name) {
  const value = option(name);
  if (!value) throw new Error(`missing --${name}`);
  return value;
}

// ---------------------------------------------------------------- scenarios
//
// A scenario names the exact observable the campaign attributes, and nothing
// else. `identity` is returned only when the defect's signature is present AND
// the legal explanations for the same observable have been ruled out.

// Harness proof, not a defect: drives the repository's own Tauri fixture so the
// worker, driver, webview, and control API can be verified without any subject
// application. It never reports an identity.
const fixtureSmoke = {
  id: 'fixture-smoke',
  identity: 'tauri-fixture:detail-not-revealed',
  async readiness(browser) {
    const deadline = Date.now() + READY_TIMEOUT_MS;
    let last = 'not attempted';
    while (Date.now() < deadline) {
      try {
        const ready = await browser.execute(
          () => !!document.querySelector('[data-testid="toggle"]'),
        );
        if (ready) {
          return {
            ready: true,
            title: await browser.execute(() => document.title),
            url: await browser.execute(() => location.href),
          };
        }
        last = 'toggle control is not present yet';
      } catch (error) {
        last = String(error).slice(0, 200);
      }
      await sleep(1_000);
    }
    throw new Error(`the webview never became observable: ${last}`);
  },
  async trigger(browser, context, state) {
    state.before = await browser.execute(
      () => document.querySelector('[data-testid="detail"]').hidden,
    );
    await browser.execute(() => document.querySelector('[data-testid="toggle"]').click());
    await sleep(SETTLE_MS);
    return { clicked: 'toggle', hiddenBefore: state.before };
  },
  // Neighboring legal behavior: an element the trigger must not touch keeps its
  // state, so a scenario that changes everything cannot pass by accident.
  async control(browser) {
    const text = await browser.execute(
      () => document.querySelector('#overflow-message').textContent,
    );
    return { element: '#overflow-message', text, legal: text === 'Overflow proof fixture' };
  },
  async observe(browser, context, state) {
    const revealed = await browser.execute(
      () => !document.querySelector('[data-testid="detail"]').hidden,
    );
    return {
      identity: revealed ? null : this.identity,
      exceptions: [],
      hiddenBefore: state.before,
      revealed,
    };
  },
};


// ---------------------------------------------------------------- scenario 2
//
// cc-switch issue 4302 / pull request 4315. The click-outside handler was bound
// to a container ref that wrapped only the search input row, not the result
// list below it, so pressing the mouse on a search result fired the
// outside-click path and cleared the search before the click completed. The
// preset was never selected.
//
// This only reproduces with a real pointer sequence. The handler listens on
// mousedown, and a synthetic element.click() dispatches no mousedown at all, so
// it selects the preset and shows nothing. Every interaction below therefore
// goes through WebDriver pointer actions at real window coordinates.

const PRESET_QUERY = 'kimi';
const NAME_PLACEHOLDER = 'e.g., Claude Official';
const SEARCH_LABEL = 'Search provider presets';

// A genuine press and release on the element. The WebDriver Element Click
// command scrolls the element into view and dispatches a real pointer sequence,
// including the mousedown this defect turns on. Computing viewport coordinates
// by hand is not equivalent and is not safe: the rect a query returns can
// belong to an offscreen twin of the element, so the press lands on whatever
// occupies that point instead.
async function pointerPress(browser, selector) {
  // A selector can match an offscreen twin of the intended element. Press the
  // first match that is actually displayed and clickable, never simply the
  // first in document order.
  const matches = await browser.$$(selector);
  for (const element of matches) {
    if (!(await element.isDisplayed())) continue;
    if (!(await element.isClickable())) continue;
    const text = (await element.getText()).trim().slice(0, 40);
    await element.click();
    return { selector, text };
  }
  throw new Error(`no displayed clickable element for ${selector} (${matches.length} matched)`);
}

const presetPointerSelect = {
  id: 'preset-pointer-select',
  identity: 'preset-search:result-not-selected-by-pointer',
  async readiness(browser, context, state) {
    const deadline = Date.now() + READY_TIMEOUT_MS;
    let last = 'not attempted';
    while (Date.now() < deadline) {
      try {
        // The first launch shows a welcome dialog that swallows every click.
        await browser.execute(() => {
          const got = [...document.querySelectorAll('button')]
            .find((b) => /got it|\u77e5\u9053\u4e86/i.test((b.textContent || '').trim()));
          if (got) got.click();
        });
        await sleep(600);
        const opened = await browser.execute(() => {
          const plus = [...document.querySelectorAll('button')].find((b) => {
            const svg = b.querySelector('svg');
            return svg && /lucide-plus/.test(svg.getAttribute('class') || '') && b.offsetWidth;
          });
          if (!plus) return false;
          plus.click();
          return true;
        });
        await sleep(1200);
        const ready = await browser.execute((placeholder) => !!(
          [...document.querySelectorAll('input')].find((i) => i.placeholder === placeholder)
        ), NAME_PLACEHOLDER);
        if (opened && ready) {
          await browser.execute(() => {
            const search = [...document.querySelectorAll('button')].find((b) => {
              const svg = b.querySelector('svg');
              return svg && /lucide-search/.test(svg.getAttribute('class') || '') && b.offsetWidth;
            });
            if (search) search.click();
          });
          await sleep(900);
          const hasSearch = await browser.execute((label) => !!(
            [...document.querySelectorAll('input')]
              .find((i) => i.getAttribute('aria-label') === label)
          ), SEARCH_LABEL);
          if (hasSearch) {
            state.presetsBefore = await browser.execute(() =>
              document.querySelectorAll('button').length);
            return { ready: true, searchOpen: true, buttons: state.presetsBefore };
          }
          last = 'the preset search did not open';
        } else {
          last = 'the add-provider form did not open';
        }
      } catch (error) {
        last = String(error).slice(0, 200);
      }
      await sleep(1500);
    }
    throw new Error(`the add-provider preset search never became observable: ${last}`);
  },
  async trigger(browser, context, state) {
    await browser.execute((args) => {
      const input = [...document.querySelectorAll('input')]
        .find((i) => i.getAttribute('aria-label') === args.label);
      const setter = Object.getOwnPropertyDescriptor(
        window.HTMLInputElement.prototype, 'value').set;
      setter.call(input, args.query);
      input.dispatchEvent(new Event('input', { bubbles: true }));
    }, { label: SEARCH_LABEL, query: PRESET_QUERY });
    await sleep(1200);

    const pressed = await pointerPress(
      browser,
      `//button[contains(translate(., 'KIM', 'kim'), '${PRESET_QUERY}')]`,
    );
    state.presetLabel = pressed.text;
    await sleep(1500);
    return { query: PRESET_QUERY, preset: pressed.text };
  },
  // Neighboring legal behavior: the search itself still works. The trigger
  // leaves the affected build with the search closed, which is the defect's own
  // side effect, so the control reopens it before filtering. Holding on both
  // revisions separates "a result cannot be selected by pointer" from "the
  // search feature is broken".
  async control(browser) {
    await browser.execute(() => {
      const search = [...document.querySelectorAll('button')].find((b) => {
        const svg = b.querySelector('svg');
        return svg && /lucide-search/.test(svg.getAttribute('class') || '') && b.offsetWidth;
      });
      if (search) search.click();
    });
    await sleep(900);
    const counts = await browser.execute((args) => {
      const input = [...document.querySelectorAll('input')]
        .find((i) => i.getAttribute('aria-label') === args.label);
      if (!input) return null;
      const setter = Object.getOwnPropertyDescriptor(
        window.HTMLInputElement.prototype, 'value').set;
      const visible = () => [...document.querySelectorAll('button')]
        .filter((b) => b.offsetWidth).length;
      setter.call(input, '');
      input.dispatchEvent(new Event('input', { bubbles: true }));
      const all = visible();
      setter.call(input, args.query);
      input.dispatchEvent(new Event('input', { bubbles: true }));
      return { all, filtered: visible() };
    }, { label: SEARCH_LABEL, query: PRESET_QUERY });
    return {
      ...(counts ?? {}),
      reopened: counts !== null,
      legal: !!counts && counts.filtered < counts.all,
    };
  },
  async observe(browser, context, state) {
    // Selecting a preset fills the provider name field. On the affected build
    // the mousedown clears the search first, so nothing is ever selected.
    const name = await browser.execute((placeholder) => {
      const input = [...document.querySelectorAll('input')]
        .find((i) => i.placeholder === placeholder);
      return input ? input.value : null;
    }, NAME_PLACEHOLDER);
    return {
      identity: name ? null : this.identity,
      exceptions: [],
      providerName: name,
      preset: state.presetLabel ?? null,
    };
  },
};

const SCENARIOS = new Map([
  [fixtureSmoke.id, fixtureSmoke],
  [presetPointerSelect.id, presetPointerSelect],
]);

// ------------------------------------------------------------------- runtime

async function serve() {
  const executablePath = requireOption('app');
  const scenarioId = requireOption('scenario');
  const webdriverio = requireOption('webdriverio');
  const scenario = SCENARIOS.get(scenarioId);
  if (!scenario) throw new Error(`unknown scenario ${scenarioId}`);
  const driverUrl = new URL(option('driver-url', DEFAULT_DRIVER_URL));
  const variant = option('variant', 'default');
  const port = Number(option('port', String(DEFAULT_PORT)));

  const { remote } = await import(webdriverio);
  const startedAt = process.hrtime.bigint();
  const browser = await remote({
    logLevel: 'error',
    hostname: driverUrl.hostname,
    port: Number(driverUrl.port),
    path: driverUrl.pathname === '/' ? '/' : driverUrl.pathname,
    // No browserName. tauri-driver forwards it verbatim to the native driver
    // (WebKitWebDriver here), which rejects unknown values like 'wry' with
    // "Failed to match capabilities". Only tauri:options is sent, which is what
    // the repository's own Tauri runner does and what the official Tauri v2
    // WebDriver example sends.
    capabilities: { 'tauri:options': { application: executablePath } },
  });

  const context = { variant };
  const state = {};
  let reached = false;
  let triggered = false;

  const verbs = {
    async readiness() {
      const result = await scenario.readiness(browser, context, state);
      reached = true;
      return result;
    },
    async trigger() {
      if (!reached) throw new Error('readiness has not run');
      const result = await scenario.trigger(browser, context, state);
      triggered = true;
      return result;
    },
    async control() {
      if (!reached) throw new Error('readiness has not run');
      return scenario.control(browser, context, state);
    },
    async observe() {
      if (!triggered) throw new Error('trigger has not run');
      const result = await scenario.observe(browser, context, state);
      const elapsed = Number(process.hrtime.bigint() - startedAt) / 1e9;
      // WebKitGTK exposes no JS heap measurement through WebDriver, so the
      // campaign declares memory unavailable rather than inventing a number.
      return {
        ...result,
        observationReached: true,
        cleanLaunch: true,
        identity: result.identity,
        exceptions: result.exceptions ?? [],
        jsHeapMiB: null,
        elapsedSeconds: Number(elapsed.toFixed(3)),
        scenario: scenario.id,
        variant,
      };
    },
    async shutdown() {
      return { closing: true };
    },
  };

  const server = createServer((request, response) => {
    const verb = new URL(request.url, `http://127.0.0.1:${port}`).pathname.slice(1);
    const handler = verbs[verb];
    const send = (status, body) => {
      response.writeHead(status, { 'content-type': 'application/json' });
      response.end(`${JSON.stringify(body)}\n`);
    };
    if (!handler) return send(404, { error: `unknown verb ${verb}` });
    handler()
      .then((body) => {
        send(200, body);
        if (verb === 'shutdown') {
          browser.deleteSession().catch(() => {}).then(() => {
            server.close();
            process.exit(0);
          });
        }
      })
      .catch((error) => send(500, { error: String(error).slice(0, 1024) }));
  });
  server.listen(port, '127.0.0.1', () => process.stdout.write(`serving ${port}\n`));
}

async function ask() {
  const verb = process.argv[3];
  if (!verb || verb.startsWith('--')) throw new Error('ask requires a verb');
  const port = Number(option('port', String(DEFAULT_PORT)));
  const response = await fetch(`http://127.0.0.1:${port}/${verb}`);
  const body = await response.json();
  if (!response.ok) throw new Error(`${verb} failed: ${JSON.stringify(body)}`);
  process.stdout.write(`${JSON.stringify(body)}\n`);
}

const mode = process.argv[2];
if (mode === 'serve') await serve();
else if (mode === 'ask') await ask();
else throw new Error('usage: probe-tauri.mjs serve|ask');
