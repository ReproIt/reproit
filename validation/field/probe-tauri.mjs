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

const SCENARIOS = new Map([[fixtureSmoke.id, fixtureSmoke]]);

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
