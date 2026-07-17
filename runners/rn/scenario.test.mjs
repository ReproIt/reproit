// Contract test for the Appium runner's multi-actor conductor client: two
// scenario actors (each with a MOCKED webdriverio session, no device/emulator
// needed) pull an interleaved script from a stub HTTP conductor speaking the
// exact modes/barrier.rs wire protocol. Pins, from the runner side:
//   * /claim hands each actor a distinct role (a, b);
//   * the strict global step order (serve + ack order observed on the wire);
//   * per-actor isolation (an actor only executes its own actions);
//   * the FUZZ:ACT/MISS/ASSERT marker contract the Rust classifier reads.
// The live end of this backend (real UiAutomator2 session, real taps) is
// covered by .github/scripts/appium-android-smoke.sh; this test covers the
// conductor protocol, which the smoke's single-actor walk never exercises.
// The actors run in a CHILD node process (the runner logs on process.stdout,
// which the parent's test reporter also owns), so the parent asserts on the
// child's captured stdout. Run: `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import http from 'node:http';
import { execFile } from 'node:child_process';

const RUNNER_URL = new URL('./runner.mjs', import.meta.url).href;

// The child harness: mocks the webdriverio session surface the scenario client
// touches (getPageSource for snapshot/asserts, $ / $$ for tap/type/count,
// back/pause, no queryAppState so the crash oracle stays silent), then runs
// N actors concurrently against the conductor named by REPROIT_SCENARIO_BARRIER.
// Mock interactions are printed as MOCKCALLS lines for the parent to assert on.
const HARNESS = `
const { runScenarioActor } = await import(${JSON.stringify(RUNNER_URL)});
const PAGE_XML = \`<?xml version="1.0" encoding="UTF-8"?>
<hierarchy rotation="0">
  <android.widget.FrameLayout bounds="[0,0][1080,1920]" displayed="true">
    <android.widget.Button resource-id="com.demo:id/send" text="Send"
      clickable="true" displayed="true" bounds="[0,0][200,80]"/>
    <android.widget.EditText resource-id="com.demo:id/msg" text=""
      clickable="true" displayed="true" bounds="[0,100][1080,180]"/>
    <android.widget.TextView text="hello from alice" displayed="true" bounds="[0,200][1080,260]"/>
  </android.widget.FrameLayout>
</hierarchy>\`;
function mockDriver(tag) {
  const calls = [];
  const el = (selector) => ({
    isExisting: async () => true,
    click: async () => { calls.push('click ' + selector); },
    setValue: async (v) => { calls.push('setValue ' + selector + '=' + v); },
  });
  return {
    tag, calls,
    getPageSource: async () => PAGE_XML,
    pause: async () => {},
    back: async () => { calls.push('back'); },
    $: async (selector) => el(selector),
    $$: async (selector) => [el(selector)],
  };
}
const n = Number(process.env.HARNESS_ACTORS || '2');
const drivers = [];
for (let i = 0; i < n; i++) drivers.push(mockDriver(String.fromCharCode(97 + i)));
await Promise.all(drivers.map((d) => runScenarioActor(d, [])));
for (const d of drivers) console.log('MOCKCALLS ' + d.tag + ' ' + JSON.stringify(d.calls));
`;

function runActors(env) {
  return new Promise((resolve, reject) => {
    execFile(
      process.execPath,
      ['--input-type=module', '-e', HARNESS],
      { env: { ...process.env, ...env }, timeout: 30000 },
      (err, stdout, stderr) =>
        err ? reject(new Error(String(err) + '\n' + stdout + stderr)) : resolve(stdout),
    );
  });
}

// A stub conductor speaking the modes/barrier.rs wire protocol over a real
// HTTP listener, recording serve/ack order so the test can assert the strict
// global interleaving promise from the wire.
function startConductor(script, n) {
  const state = { cursor: 0, served: false, joined: Array(n).fill(false), claimed: 0 };
  const observed = { served: [], acked: [] };
  const letter = (i) => String.fromCharCode(97 + i);
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    const dev = url.searchParams.get('device');
    const idx = dev ? dev.charCodeAt(0) - 97 : -1;
    let body = 'ERR bad-request';
    if (url.pathname === '/claim') {
      body = state.claimed < n ? letter(state.claimed) : 'ERR full';
      if (state.claimed < n) state.joined[state.claimed] = true;
      state.claimed++;
    } else if (idx >= 0 && url.pathname === '/next') {
      if (idx < n) state.joined[idx] = true;
      if (state.cursor >= script.length) body = 'DONE';
      else if (!state.joined.every(Boolean) || script[state.cursor][0] !== idx) body = 'WAIT';
      else {
        if (!state.served) {
          state.served = true;
          observed.served.push(dev + ':' + script[state.cursor][1]);
        }
        body = 'ACT\t' + script[state.cursor][1];
      }
    } else if (idx >= 0 && req.method === 'POST' && url.pathname === '/done') {
      if (state.cursor < script.length && script[state.cursor][0] === idx && state.served) {
        observed.acked.push(dev + ':' + script[state.cursor][1]);
        state.cursor++;
        state.served = false;
      }
      body = 'OK';
    }
    res.setHeader('content-length', Buffer.byteLength(body));
    res.end(body);
  });
  return new Promise((resolvePort) => {
    server.listen(0, '127.0.0.1', () => {
      resolvePort({ port: server.address().port, observed, close: () => server.close() });
    });
  });
}

// The mock-call list one actor's session recorded, from the harness output.
function callsOf(out, tag) {
  const m = out.match(new RegExp('MOCKCALLS ' + tag + ' (\\[.*)'));
  return m ? JSON.parse(m[1]) : null;
}

test('two appium actors interleave in the scripted order', async () => {
  // alice taps, bob types + asserts, alice asserts: the conductor enforces
  // this global order; each actor only ever sees its own steps.
  const script = [
    [0, 'tap:key:send'],
    [1, 'type:key:msg=hi bob'],
    [1, 'assert:text=hello from alice'],
    [0, 'assert:count:key:send=1'],
  ];
  const { port, observed, close } = await startConductor(script, 2);
  let out = '';
  try {
    out = await runActors({
      REPROIT_SCENARIO_BARRIER: `http://127.0.0.1:${port}`,
      REPROIT_DEVICE: '', // both actors exercise the /claim path
      HARNESS_ACTORS: '2',
    });
  } finally {
    close();
  }

  // Distinct roles claimed; every actor finished with the shared markers.
  assert.match(out, /JOURNEY claimed role=a/);
  assert.match(out, /JOURNEY claimed role=b/);
  assert.strictEqual(out.match(/JOURNEY DONE/g).length, 2, out);
  assert.strictEqual(out.match(/All tests passed/g).length, 2, out);

  // Each actor executed exactly its own actions, attributed to its role.
  assert.match(out, /FUZZ:ACT a tap:key:send/);
  assert.match(out, /FUZZ:ACT b type:key:msg=hi bob/);
  assert.match(out, /FUZZ:ASSERT pass text="hello from alice" actor=b/);
  assert.match(out, /FUZZ:ASSERT pass count key:send want=1 got=1 actor=a/);
  assert.doesNotMatch(out, /FUZZ:MISS/);

  // One session performed the tap and never the fill; the OTHER performed the
  // fill and never the tap: actions land on exactly one actor's device. Which
  // mock claimed `a` is a launch race, so the check is permutation-agnostic.
  const first = callsOf(out, 'a');
  const second = callsOf(out, 'b');
  const tapper = first.some((c) => c.startsWith('click ~send')) ? first : second;
  const typer = tapper === first ? second : first;
  assert.ok(
    tapper.some((c) => c.startsWith('click ~send')),
    tapper.join(),
  );
  assert.ok(!tapper.some((c) => c.startsWith('setValue')), tapper.join());
  assert.ok(
    typer.some((c) => c === 'setValue ~msg=hi bob'),
    typer.join(),
  );
  assert.ok(!typer.some((c) => c.startsWith('click')), typer.join());

  // The conductor saw every step served AND acked in the global order: the
  // strict-interleaving promise, observed from the wire.
  const want = script.map(([d, a]) => String.fromCharCode(97 + d) + ':' + a);
  assert.deepStrictEqual(observed.served, want);
  assert.deepStrictEqual(observed.acked, want);
});

test('a labeled actor keeps its env role and misses loudly on a stale action', async () => {
  const script = [
    [0, 'tap:key:nonexistent'], // still resolves (mock says everything exists)
    [0, 'key:Down'], // a TUI-surface action: cross-surface MISS
  ];
  const { port, observed, close } = await startConductor(script, 1);
  let out = '';
  try {
    out = await runActors({
      REPROIT_SCENARIO_BARRIER: `http://127.0.0.1:${port}`,
      REPROIT_DEVICE: 'a', // env label wins; /claim is never needed
      HARNESS_ACTORS: '1',
    });
  } finally {
    close();
  }
  assert.match(out, /JOURNEY claimed role=a/);
  assert.match(out, /FUZZ:MISS a key:Down/);
  assert.match(out, /JOURNEY DONE/);
  // A MISS still acks (the conductor advances; staleness is visible, not fatal).
  assert.strictEqual(observed.acked.length, 2, observed.acked.join());
});
