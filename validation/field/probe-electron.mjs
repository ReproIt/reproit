#!/usr/bin/env node

// Electron Linux field-campaign probe.
//
// The campaign adapter runs launch, readiness, trigger, and observe as separate
// bounded phases, but an Electron application only exists for the lifetime of
// one process. `serve` therefore owns the application and exposes a loopback
// control API; each phase calls `ask` with one verb. Nothing here inspects the
// application source: every verdict comes from the running application's own
// observable state.
//
// usage:
//   probe-electron.mjs serve --app PATH --scenario ID --playwright PATH
//                            [--fixture URL] [--variant ID] [--port N] [--cwd DIR]
//   probe-electron.mjs ask VERB [--port N]
//
// verbs: readiness, trigger, control, observe, shutdown

import { createServer } from 'node:http';
import { dirname } from 'node:path';

const DEFAULT_PORT = 8930;
const READY_TIMEOUT_MS = 120_000;
const SETTLE_MS = 2_000;
const RELOAD_SETTLE_MS = 8_000;

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

// ---------------------------------------------------------------- scenario 1
//
// responsively-app issue 1441. The application's webview preload claimed the
// `f` key unconditionally, so the character never reached a focused text field
// in the previewed page. Every interaction goes through the <webview> element
// API, which lives in the renderer, so the key travels the real Chromium input
// path into the guest.

const INPUT_ID = 'note';
const TRIGGER_KEY = 'f';
const CONTROL_KEY = 'g';

async function guestEval(page, expression) {
  return page.evaluate(async (source) => {
    const view = document.querySelector('webview');
    if (!view) throw new Error('no webview is attached');
    return view.executeJavaScript(source);
  }, expression);
}

async function pressInGuestInput(page, key) {
  return page.evaluate(async ({ key: pressed, inputId }) => {
    const view = document.querySelector('webview');
    await view.executeJavaScript(`document.getElementById(${JSON.stringify(inputId)}).focus()`);
    const active = await view.executeJavaScript(
      'document.activeElement && document.activeElement.id',
    );
    if (active !== inputId) throw new Error(`focus did not land on #${inputId}, got ${active}`);
    view.focus();
    view.sendInputEvent({ type: 'keyDown', keyCode: pressed });
    view.sendInputEvent({ type: 'char', keyCode: pressed });
    view.sendInputEvent({ type: 'keyUp', keyCode: pressed });
    return active;
  }, { key, inputId: INPUT_ID });
}

async function readGuest(page) {
  const raw = await guestEval(
    page,
    `JSON.stringify({
       value: document.getElementById(${JSON.stringify(INPUT_ID)}).value,
       fullscreen: !!document.fullscreenElement,
       pageSuppressed: !!(window.__reproit && window.__reproit.pageSuppressed),
       errors: (window.__reproit && window.__reproit.errors) || [],
     })`,
  );
  return JSON.parse(raw);
}

const webviewTextInput = {
  id: 'webview-text-input',
  identity: 'webview-keydown:f-suppressed-in-focused-text-input',
  needsFixture: true,
  launchArgs: (context) => [context.fixture],
  async readiness(page, context, state) {
    const deadline = Date.now() + READY_TIMEOUT_MS;
    let last = 'not attempted';
    while (Date.now() < deadline) {
      try {
        const href = await guestEval(page, 'location.href');
        const ready = await guestEval(
          page,
          `!!document.getElementById(${JSON.stringify(INPUT_ID)})`,
        );
        if (typeof href === 'string' && href.startsWith(context.fixture) && ready) {
          state.guestUrl = href;
          const webviews = await page.evaluate(() => document.querySelectorAll('webview').length);
          return { ready: true, guestUrl: href, webviews };
        }
        last = `guest at ${href}, fixture element ready=${ready}`;
      } catch (error) {
        last = String(error).slice(0, 200);
      }
      await sleep(1_000);
    }
    throw new Error(`guest never reached the fixture: ${last}`);
  },
  async trigger(page, context, state) {
    state.focused = await pressInGuestInput(page, TRIGGER_KEY);
    await sleep(SETTLE_MS);
    return { delivered: TRIGGER_KEY, focused: state.focused };
  },
  // Neighboring legal behavior: a different printable key in the same focused
  // input must still reach the guest on both revisions.
  async control(page) {
    await guestEval(page, `document.getElementById(${JSON.stringify(INPUT_ID)}).value = ''`);
    await pressInGuestInput(page, CONTROL_KEY);
    await sleep(SETTLE_MS);
    const guest = await readGuest(page);
    return { delivered: CONTROL_KEY, value: guest.value, legal: guest.value === CONTROL_KEY };
  },
  async observe(page, context, state) {
    const guest = await readGuest(page);
    // The defect is exactly this: the keystroke never reaches the focused text
    // field because the application's webview preload claims it. Two legal
    // explanations for the same missing character are ruled out before the
    // identity is attributed: the guest page may claim the key itself, and the
    // input may not be accepting characters at all. The second is settled by
    // delivering the control key into the same input.
    let attributable = guest.value !== TRIGGER_KEY && !guest.pageSuppressed;
    let controlValue = null;
    if (attributable) {
      await guestEval(page, `document.getElementById(${JSON.stringify(INPUT_ID)}).value = ''`);
      await pressInGuestInput(page, CONTROL_KEY);
      await sleep(SETTLE_MS);
      controlValue = (await readGuest(page)).value;
      attributable = controlValue === CONTROL_KEY;
    }
    return {
      identity: attributable ? this.identity : null,
      exceptions: guest.errors,
      guestValue: guest.value,
      guestFullscreen: guest.fullscreen,
      guestPageSuppressed: guest.pageSuppressed,
      guestControlValue: controlValue,
      guestUrl: state.guestUrl,
    };
  },
};

// ---------------------------------------------------------------- scenario 2
//
// upscayl issue 1225 / pull request 1257. The settings tab wrote the chosen
// export format to localStorage as a bare string on top of the jotai
// atomWithStorage entry for the same key. atomWithStorage stores JSON, so the
// bare value failed to parse on the next load and the setting silently reverted
// to the default. The fix deletes the extra write.

const FORMAT_KEY = 'saveImageAs';
const FORMAT_DEFAULT = 'png';
const FORMAT_PICK = 'jpg';
const THEME_KEY = 'theme';
const THEME_PICK = 'dark';

async function dismissDialogs(page) {
  for (let attempt = 0; attempt < 6; attempt += 1) {
    const open = await page.$('[role="dialog"][data-state="open"]');
    if (!open) return true;
    await page.keyboard.press('Escape');
    await sleep(800);
  }
  return !(await page.$('[role="dialog"][data-state="open"]'));
}

async function openSettings(page) {
  if (!(await dismissDialogs(page))) throw new Error('a modal dialog would not close');
  await page.getByText('Settings', { exact: true }).first().click({ timeout: 20_000 });
  await sleep(SETTLE_MS);
}

// The format control is a row of buttons; the active one carries btn-primary.
async function selectedFormat(page) {
  return page.evaluate(({ formats }) => {
    const buttons = [...document.querySelectorAll('button')];
    const match = buttons.find((button) => {
      const label = (button.textContent || '').trim().toLowerCase();
      return formats.includes(label) && button.className.includes('btn-primary');
    });
    return match ? (match.textContent || '').trim().toLowerCase() : null;
  }, { formats: ['png', 'jpg', 'webp'] });
}

async function clickFormat(page, format) {
  const clicked = await page.evaluate((wanted) => {
    const button = [...document.querySelectorAll('button')]
      .find((candidate) => (candidate.textContent || '').trim().toLowerCase() === wanted);
    if (!button) return false;
    button.click();
    return true;
  }, format);
  if (!clicked) throw new Error(`no ${format} format button is present`);
  await sleep(SETTLE_MS);
}

const settingsPersistence = {
  id: 'settings-persistence',
  identity: 'settings:save-image-as-not-restored-after-reload',
  needsFixture: false,
  launchArgs: () => [],
  async readiness(page, context, state) {
    const deadline = Date.now() + READY_TIMEOUT_MS;
    let last = 'not attempted';
    while (Date.now() < deadline) {
      try {
        await openSettings(page);
        // Adversarial corpus variant: a different setting is already stored in
        // the broken bare form, so something visibly reverts on reload. The
        // oracle must still refuse to report this defect's identity.
        if (context.variant === 'unrelated-bare-key') {
          await page.evaluate((key) => localStorage.setItem(key, 'dark'), THEME_KEY);
        }
        const selected = await selectedFormat(page);
        if (selected) {
          state.initialFormat = selected;
          return { ready: true, selectedFormat: selected };
        }
        last = 'settings tab has no active format button';
      } catch (error) {
        last = String(error).slice(0, 200);
      }
      await sleep(2_000);
    }
    throw new Error(`settings never became observable: ${last}`);
  },
  async trigger(page, context, state) {
    if (state.initialFormat === FORMAT_PICK) {
      throw new Error(`the default format is already ${FORMAT_PICK}`);
    }
    await clickFormat(page, FORMAT_PICK);
    state.picked = await selectedFormat(page);
    state.storedAfterPick = await page.evaluate(
      (key) => localStorage.getItem(key),
      FORMAT_KEY,
    );
    // Reloading the renderer re-runs exactly the restore path the defect
    // breaks. Nothing else about the application is touched.
    await page.reload({ waitUntil: 'domcontentloaded' });
    await sleep(RELOAD_SETTLE_MS);
    await openSettings(page);
    return { picked: state.picked, storedAfterPick: state.storedAfterPick };
  },
  // Neighboring legal behavior: another setting persisted through the same
  // storage mechanism, without the extra bare write, still survives a reload.
  async control(page) {
    await page.evaluate(
      ({ key, value }) => localStorage.setItem(key, JSON.stringify(value)),
      { key: THEME_KEY, value: THEME_PICK },
    );
    await page.reload({ waitUntil: 'domcontentloaded' });
    await sleep(RELOAD_SETTLE_MS);
    const restored = await page.evaluate((key) => localStorage.getItem(key), THEME_KEY);
    return {
      key: THEME_KEY,
      restored,
      legal: restored === JSON.stringify(THEME_PICK),
    };
  },
  async observe(page, context, state) {
    const selected = await selectedFormat(page);
    const stored = await page.evaluate((key) => localStorage.getItem(key), FORMAT_KEY);
    const reverted = selected === FORMAT_DEFAULT && state.picked === FORMAT_PICK;
    return {
      identity: reverted ? this.identity : null,
      exceptions: [],
      selectedFormat: selected,
      storedAfterPick: state.storedAfterPick,
      storedAfterReload: stored,
    };
  },
};

const SCENARIOS = new Map([
  [webviewTextInput.id, webviewTextInput],
  [settingsPersistence.id, settingsPersistence],
]);

// ------------------------------------------------------------------- runtime

async function serve() {
  const executablePath = requireOption('app');
  const scenarioId = requireOption('scenario');
  const playwright = requireOption('playwright');
  const scenario = SCENARIOS.get(scenarioId);
  if (!scenario) throw new Error(`unknown scenario ${scenarioId}`);
  const fixture = option('fixture');
  const variant = option('variant', 'default');
  if (scenario.needsFixture && !fixture) throw new Error(`${scenarioId} requires --fixture`);
  const port = Number(option('port', String(DEFAULT_PORT)));
  const cwd = option('cwd', dirname(executablePath));
  const { _electron: electron } = await import(playwright);

  const context = { fixture, variant };
  const startedAt = process.hrtime.bigint();
  const app = await electron.launch({
    executablePath,
    args: [...scenario.launchArgs(context), '--no-sandbox', '--disable-gpu'],
    cwd,
  });
  const page = await app.firstWindow();
  const exceptions = [];
  page.on('pageerror', (error) => exceptions.push(String(error).slice(0, 200)));
  await page.waitForLoadState('domcontentloaded');

  // Applications reach out on startup -- update checks, extension downloads,
  // analytics. The campaign runs them with no network on purpose, so those
  // failures are caused by the containment, not by the build under test. They
  // are retained separately rather than dropped, and anything that is NOT
  // offline-attributable still fails the run.
  const OFFLINE = /Failed to fetch|NetworkError|ERR_INTERNET_DISCONNECTED|ERR_NAME_NOT_RESOLVED|ERR_CONNECTION|net::|getaddrinfo|ENOTFOUND|EAI_AGAIN/;

  const state = {};
  let reached = false;
  let triggered = false;

  const verbs = {
    async readiness() {
      const result = await scenario.readiness(page, context, state);
      reached = true;
      return result;
    },
    async trigger() {
      if (!reached) throw new Error('readiness has not run');
      const result = await scenario.trigger(page, context, state);
      triggered = true;
      return result;
    },
    async control() {
      if (!reached) throw new Error('readiness has not run');
      return scenario.control(page, context, state);
    },
    async observe() {
      if (!triggered) throw new Error('trigger has not run');
      const result = await scenario.observe(page, context, state);
      const heap = await page.evaluate(
        () => Math.ceil((performance.memory?.usedJSHeapSize || 0) / 1024 / 1024),
      );
      const elapsed = Number(process.hrtime.bigint() - startedAt) / 1e9;
      const observed = [...exceptions, ...(result.exceptions ?? [])];
      // The scenario's detail fields come first so the contract fields below
      // are always the ones the adapter reads.
      return {
        ...result,
        observationReached: true,
        cleanLaunch: true,
        identity: result.identity,
        exceptions: observed.filter((entry) => !OFFLINE.test(entry)),
        environmentExceptions: observed.filter((entry) => OFFLINE.test(entry)),
        jsHeapMiB: heap > 0 ? heap : null,
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
          app.close().catch(() => {}).then(() => {
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
else throw new Error('usage: probe-electron.mjs serve|ask');
