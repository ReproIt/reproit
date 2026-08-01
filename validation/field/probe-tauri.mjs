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
const PRESET_BUTTON = '//button[normalize-space(.)="KimiKimi"]';
// Each preset button carries its family label as well as its display name, so
// the Kimi button reads "KimiKimi" and this one reads "KimiKimi For Coding".
const CONTROL_BUTTON = '//button[contains(normalize-space(.),"Kimi For Coding")]';
const NAME_PLACEHOLDER = 'e.g., Claude Official';
const SEARCH_LABEL = 'Search provider presets';
const WINDOW_WIDTH = 1600;
const WINDOW_HEIGHT = 1100;
const DIALOG_SETTLE_MS = 60_000;

// The application shows two informational dialogs, a first-launch welcome and
// an "About Common Config" note on the add-provider form. Each renders a
// full-window overlay. Neither is part of the defect, and both swallow the
// pointer press: the press lands on the overlay, the search closes, and the
// preset is never selected -- on BOTH revisions. That is a run whose observable
// is produced by the harness rather than by the subject, so the scenario
// dismisses every open dialog and refuses to proceed until none is open.
async function dismissDialogs(browser) {
  return browser.execute(() => {
    const open = [];
    for (const dialog of document.querySelectorAll('[role="dialog"],[role="alertdialog"]')) {
      if (!dialog.offsetWidth) continue;
      const accept = [...dialog.querySelectorAll('button')]
        .find((b) => /got it|知道了|ok|确定/i.test((b.textContent || '').trim()));
      if (accept) accept.click();
      open.push((dialog.textContent || '').trim().slice(0, 24));
    }
    return open;
  });
}

// Dialogs appear asynchronously, so one dismissal proves nothing. Require two
// consecutive clear polls before the run may continue.
async function settleDialogs(browser, label) {
  const deadline = Date.now() + DIALOG_SETTLE_MS;
  let clear = 0;
  while (Date.now() < deadline) {
    const open = await dismissDialogs(browser);
    clear = open.length === 0 ? clear + 1 : 0;
    if (clear >= 2) return true;
    await sleep(700);
  }
  throw new Error(`${label}: a modal overlay never cleared`);
}

// A genuine press and release on the element. The WebDriver Element Click
// command scrolls the element into view and dispatches a real pointer sequence,
// including the mousedown this defect turns on. Computing viewport coordinates
// by hand is not equivalent and is not safe: the rect a query returns can
// belong to an offscreen twin of the element, so the press lands on whatever
// occupies that point instead.
//
// The hit test is part of the contract: the press is only attributed when the
// preset itself is the topmost element at its own centre. Without it, an
// overlay absorbs the press and the run reports the defect's observable on
// every revision.
async function pointerPress(browser, xpath) {
  const hit = await browser.execute((selector) => {
    const found = document.evaluate(
      selector, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null,
    ).singleNodeValue;
    if (!found) return { found: false };
    const rect = found.getBoundingClientRect();
    const x = Math.round(rect.x + rect.width / 2);
    const y = Math.round(rect.y + rect.height / 2);
    const top = document.elementFromPoint(x, y);
    return {
      found: true,
      centre: [x, y],
      topmost: !!top && found.contains(top),
      text: (found.textContent || '').trim().slice(0, 40),
    };
  }, xpath);
  if (!hit.found) throw new Error(`no element matched ${xpath}`);
  if (!hit.topmost) throw new Error(`${xpath} is not the topmost element at its own centre`);
  const element = await browser.$(xpath);
  if (!(await element.isDisplayed())) throw new Error(`${xpath} is not displayed`);
  if (!(await element.isClickable())) throw new Error(`${xpath} is not clickable`);
  await element.click();
  return { selector: xpath, text: hit.text, centre: hit.centre };
}

const presetPointerSelect = {
  id: 'preset-pointer-select',
  identity: 'preset-search:result-not-selected-by-pointer',
  async readiness(browser, context, state) {
    const deadline = Date.now() + READY_TIMEOUT_MS;
    let last = 'not attempted';
    // The application window is 1000x650 by configuration, which puts most of
    // the preset grid outside the viewport. Fix the geometry first so every run
    // presses the same preset at the same place.
    await browser.setWindowRect(0, 0, WINDOW_WIDTH, WINDOW_HEIGHT);
    await sleep(SETTLE_MS);
    while (Date.now() < deadline) {
      try {
        await settleDialogs(browser, 'launch');
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
        // The add-provider form raises its own informational dialog.
        await settleDialogs(browser, 'add-provider');
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

    const pressed = await pointerPress(browser, PRESET_BUTTON);
    state.presetLabel = pressed.text;
    await sleep(1500);
    return { query: PRESET_QUERY, preset: pressed.text, centre: pressed.centre };
  },
  // Neighboring legal behavior, run on the affected build after the trigger:
  // the same pointer press on a preset with the search closed still selects it.
  // Holding here separates "a preset reached through the search cannot be
  // selected by pointer" from "this harness cannot select a preset at all".
  async control(browser) {
    await settleDialogs(browser, 'control');
    // The trigger closes the search on the affected build; close it explicitly
    // if the run being controlled left it open.
    const searchOpen = await browser.execute((label) => !!(
      [...document.querySelectorAll('input')].find((i) => i.getAttribute('aria-label') === label)
    ), SEARCH_LABEL);
    if (searchOpen) {
      await browser.execute(() => {
        const search = [...document.querySelectorAll('button')].find((b) => {
          const svg = b.querySelector('svg');
          return svg && /lucide-search/.test(svg.getAttribute('class') || '') && b.offsetWidth;
        });
        if (search) search.click();
      });
      await sleep(900);
    }
    const pressed = await pointerPress(browser, CONTROL_BUTTON);
    await sleep(1500);
    const name = await browser.execute((placeholder) => {
      const input = [...document.querySelectorAll('input')]
        .find((i) => i.placeholder === placeholder);
      return input ? input.value : null;
    }, NAME_PLACEHOLDER);
    return {
      preset: pressed.text,
      searchWasOpen: searchOpen,
      providerName: name,
      legal: name === 'Kimi For Coding',
    };
  },
  async observe(browser, context, state) {
    // Selecting a preset fills the provider name field. On the affected build
    // the mousedown clears the search first, so nothing is ever selected.
    const observed = await browser.execute((args) => {
      const name = [...document.querySelectorAll('input')]
        .find((i) => i.placeholder === args.placeholder);
      const search = [...document.querySelectorAll('input')]
        .find((i) => i.getAttribute('aria-label') === args.label);
      return {
        providerName: name ? name.value : null,
        searchStillOpen: !!search,
        searchValue: search ? search.value : null,
      };
    }, { placeholder: NAME_PLACEHOLDER, label: SEARCH_LABEL });
    return {
      identity: observed.providerName ? null : this.identity,
      exceptions: [],
      providerName: observed.providerName,
      searchStillOpen: observed.searchStillOpen,
      searchValue: observed.searchValue,
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
