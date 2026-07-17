#!/usr/bin/env node
// pw-capture: run a user's EXISTING Playwright test with tracing on, then read
// its ACTION TRACE (NOT its source) and emit a structured action list reproit can
// replay as a fuzz prefix. The pitch: "you wrote the test; reproit finds the bugs
// you didn't" -- in the user's own language, with zero new DSL.
//
// We never parse Playwright source. We run the test under a reproit-provided
// minimal config (trace:'on', fullyParallel:false, testDir/testMatch pinned at
// the given file), find the produced trace.zip, unzip it, and read the NDJSON
// `*.trace` action events (type:"action", apiName like locator.click/fill,
// params.selector/value). Playwright's STRUCTURED selector engines map cleanly to
// reproit finders; anything ambiguous (chained/css/xpath/regex) is SKIPPED with a
// logged reason, never guessed.
//
// Usage:  node pw-capture.mjs --test <abs path to spec> [--out <json file>]
// Output: a JSON object on stdout (and to --out if given):
//   { baseURL, gotoUrl, actions:[{kind,finder,value?,raw}], unsupported:[{raw,reason}],
//     notes:[...], passed }
// Exit 0 even when the test fails: a failing test can still leave a usable trace
// prefix, and reproit's job is to fuzz onward, not to grade the test.

import { spawnSync } from 'node:child_process';
import {
  mkdtempSync,
  readFileSync,
  writeFileSync,
  existsSync,
  readdirSync,
  statSync,
  rmSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname, basename, resolve } from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);

function parseArgs(argv) {
  const a = { test: null, out: null };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--test') a.test = argv[++i];
    else if (argv[i] === '--out') a.out = argv[++i];
  }
  return a;
}

// ---- selector mapping: Playwright structured engine -> reproit finder --------
// Returns { finder } on a clean map, or { skip, reason } when the selector is one
// reproit can't faithfully replay (a guess would be worse than an honest skip).
export function mapSelector(selector) {
  if (typeof selector !== 'string' || !selector.trim()) {
    return { skip: true, reason: 'empty selector' };
  }
  const sel = selector.trim();

  // Chained selectors (>> joins) drop the structure reproit replays from. A
  // single getByX is one part; a chain is a path we can't address.
  if (sel.includes('>>')) {
    return { skip: true, reason: 'chained selector (>>)' };
  }

  // internal:testid=[data-testid="x"s]  (getByTestId). Playwright appends a
  // case-sensitivity flag (`s`/`i`) before the closing bracket; tolerate it.
  let m = sel.match(
    new RegExp(
      '^internal:testid=\\[(?:data-testid|data-test-id|data-test)="([^"]*)"[si]' + '?\\]',
      'i',
    ),
  );
  if (m) return { finder: 'key:testid:' + m[1] };

  // internal:attr=[name="x"s]  /  internal:attr=[id="x"s]
  m = sel.match(/^internal:attr=\[name="([^"]*)"[si]?\]/i);
  if (m) return { finder: 'key:name:' + m[1] };
  m = sel.match(/^internal:attr=\[id="([^"]*)"[si]?\]/i);
  if (m) return { finder: 'key:id:' + m[1] };

  // internal:role=role[name="N"i] cannot be replayed faithfully without a stable
  // selector from the captured app. The name is visible text; reproit actions use
  // structural selectors only.
  m = sel.match(/^internal:role=([a-z]+)\[name="((?:[^"\\]|\\.)*)"[si]?\]/i);
  if (m) {
    return { skip: true, reason: 'role selector by visible name' };
  }
  m = sel.match(/^internal:role=([a-z]+)\b/i);
  if (m) {
    return {
      finder: 'role:' + m[1].toLowerCase() + '#0',
      weak: true,
      reason: 'getByRole without an accessible name',
    };
  }

  // internal:label="T"  /  internal:text="T"  /  internal:has-text="T"
  m = sel.match(/^internal:(?:label|text|has-text)="((?:[^"\\]|\\.)*)"[si]?/i);
  if (m) {
    return { skip: true, reason: 'visible-text selector' };
  }
  // getByPlaceholder -> internal:attr=[placeholder="T"]; placeholder text is not
  // a structural selector, so it is not replayed.
  m = sel.match(/^internal:attr=\[placeholder="((?:[^"\\]|\\.)*)"[si]?\]/i); // flag before ]
  if (m) {
    return { skip: true, reason: 'placeholder text selector' };
  }

  // Plain CSS forms reproit can address: #id, [id="x"], [name="x"], [data-testid].
  m = sel.match(/^(?:css=)?#([A-Za-z_][\w-]*)$/);
  if (m) return { finder: 'key:id:' + m[1] };
  m = sel.match(/^(?:css=)?\[id="([^"]*)"\]$/);
  if (m) return { finder: 'key:id:' + m[1] };
  m = sel.match(/^(?:css=)?\[name="([^"]*)"\]$/);
  if (m) return { finder: 'key:name:' + m[1] };
  m = sel.match(/^(?:css=)?\[data-testid="([^"]*)"\]$/);
  if (m) return { finder: 'key:testid:' + m[1] };

  // Everything else (complex css, xpath=, chained, regex name, :nth, etc.) is
  // SKIPPED with a reason. Guessing a finder for these would silently mis-replay.
  const kind = sel.startsWith('xpath=')
    ? 'xpath selector'
    : sel.startsWith('internal:')
      ? 'unsupported engine: ' + sel.slice(0, 24)
      : 'complex css selector';
  return { skip: true, reason: kind };
}

// ---- action mapping: a parsed trace action -> a reproit action string -------
// apiName is like "locator.click" / "locator.fill" / "page.goto" / "locator.check".
export function mapAction(apiName, selector, value) {
  const api = (apiName || '').toLowerCase();
  if (api === 'page.goto' || api === 'browsercontext.goto' || api === 'frame.goto') {
    return { kind: 'goto', value };
  }
  // click / check / tap / dblclick / press(on a control) -> tap:<finder>
  if (/\.(click|check|tap|dblclick|setchecked|selectoption|press)$/.test(api)) {
    const m = mapSelector(selector);
    if (m.skip) return { skip: true, reason: m.reason, raw: selector };
    // selectOption / setChecked still resolve to a tap on the control for the
    // prefix; the typed value (if any) rides along but reproit's tap is the action.
    return { kind: 'tap', finder: m.finder, weak: m.weak, reason: m.reason };
  }
  // fill / type / press-with-text -> type:<finder>=<value>
  if (/\.(fill|type)$/.test(api)) {
    const m = mapSelector(selector);
    if (m.skip) return { skip: true, reason: m.reason, raw: selector };
    return {
      kind: 'type',
      finder: m.finder,
      value: value == null ? '' : String(value),
      weak: m.weak,
      reason: m.reason,
    };
  }
  // Anything else (expect/assert/waitFor/hover/screenshot/...) is not a state
  // mutation reproit replays; skip silently-but-counted as "non-action".
  return { skip: true, reason: 'non-replayable api: ' + (apiName || '?'), nonAction: true };
}

// ---- trace.zip reading -------------------------------------------------------
// A Playwright trace.zip carries one or more `*.trace` NDJSON members. Each line
// is a JSON event; the ones we want have type:"action" (or "before" in newer
// formats) with apiName + params. We read with the `playwright`-bundled unzip if
// present, else fall back to the system `unzip`.
function unzipTraceEntries(zipPath, workDir) {
  // Use the system unzip: portable, and the trace files are small.
  const r = spawnSync('unzip', ['-o', zipPath, '-d', workDir], { encoding: 'utf8' });
  if (r.status !== 0) {
    throw new Error('unzip failed for ' + zipPath + ': ' + (r.stderr || r.stdout || ''));
  }
  return readdirSync(workDir)
    .filter((f) => f.endsWith('.trace'))
    .map((f) => join(workDir, f));
}

// Pull the ordered (apiName, selector, value) action tuples from a `.trace` file.
function actionsFromTrace(tracePath) {
  const raw = readFileSync(tracePath, 'utf8');
  const out = [];
  for (const line of raw.split('\n')) {
    const s = line.trim();
    if (!s) continue;
    let ev;
    try {
      ev = JSON.parse(s);
    } catch (_) {
      continue;
    }
    // Playwright trace formats: an action is type:"action" (older) or a
    // "before" event carrying apiName (newer). Both carry apiName + params.
    const isAction = ev.type === 'action' || ev.type === 'before';
    if (!isAction) continue;
    const apiName =
      ev.apiName ||
      (ev.method ? (ev.class ? ev.class.toLowerCase() + '.' + ev.method : ev.method) : null);
    if (!apiName) continue;
    const params = ev.params || {};
    const selector =
      params.selector != null ? params.selector : (params.strings && params.strings[0]) || null;
    const value =
      params.value != null
        ? params.value
        : params.text != null
          ? params.text
          : params.url != null
            ? params.url
            : undefined;
    out.push({ apiName, selector, value, startTime: ev.startTime || ev.wallTime || 0 });
  }
  // Order by startTime so multiple .trace files (page + api) interleave correctly.
  out.sort((a, b) => (a.startTime || 0) - (b.startTime || 0));
  return out;
}

function findTraceZips(root) {
  const found = [];
  const walk = (dir) => {
    let entries = [];
    try {
      entries = readdirSync(dir);
    } catch (_) {
      return;
    }
    for (const e of entries) {
      const p = join(dir, e);
      let st;
      try {
        st = statSync(p);
      } catch (_) {
        continue;
      }
      if (st.isDirectory()) walk(p);
      else if (e === 'trace.zip') found.push({ path: p, mtime: st.mtimeMs });
    }
  };
  walk(root);
  found.sort((a, b) => a.mtime - b.mtime);
  return found.map((f) => f.path);
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!args.test) {
    process.stderr.write('pw-capture: --test <spec> is required\n');
    process.exit(2);
  }
  const testPath = resolve(args.test);
  if (!existsSync(testPath)) {
    process.stderr.write('pw-capture: test not found: ' + testPath + '\n');
    process.exit(2);
  }

  const runnerDir = dirname(new URL(import.meta.url).pathname);
  const work = mkdtempSync(join(tmpdir(), 'reproit-pwcap-'));
  const outDir = join(work, 'results');
  // The config must live INSIDE the runner dir so its `import '@playwright/test'`
  // resolves against the runner's node_modules (ESM resolves relative to the
  // importing file, not cwd). Unique name + best-effort cleanup at the end.
  const configPath = join(runnerDir, '.reproit-pw-config-' + process.pid + '.mjs');
  // Minimal reproit-owned config: tracing on, serial, point testDir/testMatch at
  // exactly the given file. testIdAttribute left default (data-testid) so
  // getByTestId maps to key:testid. No webServer/baseURL: the test brings its own.
  const cfg = `import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: ${JSON.stringify(dirname(testPath))},
  testMatch: ${JSON.stringify(basename(testPath))},
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: 'list',
  outputDir: ${JSON.stringify(outDir)},
  use: { trace: 'on', screenshot: 'off', video: 'off' },
});
`;
  writeFileSync(configPath, cfg);
  // Guarantee the in-runner-dir config + work tree are removed on ANY exit path
  // (success, early-exit, or a thrown error), so a stray file never lingers in the
  // runner dir between runs.
  process.on('exit', () => {
    try {
      rmSync(configPath, { force: true });
    } catch (_) {}
    try {
      rmSync(work, { recursive: true, force: true });
    } catch (_) {}
  });

  // Resolve the playwright test CLI from THIS runner dir's node_modules, so the
  // capture uses the same bundled browsers reproit already installs.
  let cli;
  try {
    cli = require.resolve('playwright/cli');
  } catch (_) {
    try {
      cli = require.resolve('@playwright/test/cli');
    } catch (e) {
      process.stderr.write('pw-capture: cannot resolve playwright cli\n');
      process.exit(2);
    }
  }

  // The user's test imports `@playwright/test`, which must resolve from the test
  // file's own directory tree even though the deps live in the runner dir. Point
  // NODE_PATH at the runner's node_modules: Playwright transpiles specs to CJS,
  // and the CJS loader honors NODE_PATH for bare-import resolution.
  const runnerModules = join(runnerDir, 'node_modules');
  const nodePath = [runnerModules, process.env.NODE_PATH].filter(Boolean).join(':');
  const run = spawnSync(process.execPath, [cli, 'test', '--config', configPath], {
    cwd: runnerDir,
    encoding: 'utf8',
    env: { ...process.env, CI: '1', NODE_PATH: nodePath },
    stdio: ['ignore', 'pipe', 'pipe'],
    maxBuffer: 64 * 1024 * 1024,
  });
  const passed = run.status === 0;
  process.stderr.write(run.stdout || '');
  process.stderr.write(run.stderr || '');

  const zips = findTraceZips(outDir);
  if (zips.length === 0) {
    process.stderr.write('pw-capture: no trace.zip produced (did the test launch a browser?)\n');
    emit(
      {
        baseURL: null,
        gotoUrl: null,
        actions: [],
        unsupported: [],
        notes: ['no trace produced'],
        passed,
      },
      args.out,
    );
    process.exit(0);
  }

  // Collect every trace's actions in time order across zips.
  let raw = [];
  for (const zip of zips) {
    const ex = mkdtempSync(join(tmpdir(), 'reproit-tz-'));
    try {
      for (const tf of unzipTraceEntries(zip, ex)) raw = raw.concat(actionsFromTrace(tf));
    } catch (e) {
      process.stderr.write('pw-capture: ' + e.message + '\n');
    } finally {
      rmSync(ex, { recursive: true, force: true });
    }
  }
  raw.sort((a, b) => (a.startTime || 0) - (b.startTime || 0));

  const result = buildResult(raw);
  result.passed = passed;
  emit(result, args.out);
  process.exit(0); // the on('exit') handler removes work + config
}

// Turn ordered raw trace actions into the reproit action prefix + reports.
export function buildResult(raw) {
  const actions = [];
  const unsupported = [];
  const notes = [];
  let baseURL = null;
  let gotoUrl = null;

  for (const a of raw) {
    const mapped = mapAction(a.apiName, a.selector, a.value);
    if (mapped.kind === 'goto') {
      const url = mapped.value;
      if (!url) continue;
      if (!gotoUrl) {
        gotoUrl = url;
        try {
          baseURL = new URL(url).origin;
        } catch (_) {
          baseURL = url;
        }
        notes.push('start url from page.goto: ' + url);
      } else {
        // Later gotos are not in-fuzz nav (the runner explores from one origin);
        // record as a note so nothing is silently dropped.
        notes.push('# note: extra page.goto(' + url + ') skipped (no in-fuzz navigation)');
      }
      continue;
    }
    if (mapped.skip) {
      if (!mapped.nonAction)
        unsupported.push({ raw: mapped.raw || a.selector || '?', reason: mapped.reason });
      continue;
    }
    if (mapped.weak) {
      notes.push(
        'weak finder for ' + a.apiName + ': ' + mapped.finder + ' (' + mapped.reason + ')',
      );
    }
    if (mapped.kind === 'tap') {
      actions.push({
        kind: 'tap',
        finder: mapped.finder,
        action: 'tap:' + mapped.finder,
        raw: a.selector,
      });
    } else if (mapped.kind === 'type') {
      actions.push({
        kind: 'type',
        finder: mapped.finder,
        value: mapped.value,
        action: 'type:' + mapped.finder + '=' + mapped.value,
        raw: a.selector,
      });
    }
  }
  return { baseURL, gotoUrl, actions, unsupported, notes };
}

function emit(obj, outPath) {
  const json = JSON.stringify(obj, null, 2);
  if (outPath) writeFileSync(outPath, json);
  process.stdout.write(json + '\n');
}

// Only run main when invoked directly (not when imported by tests).
const invokedDirect =
  process.argv[1] && resolve(process.argv[1]) === resolve(new URL(import.meta.url).pathname);
if (invokedDirect) main();
