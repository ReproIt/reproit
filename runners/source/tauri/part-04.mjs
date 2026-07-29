  return String(s).replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (_, name) => process.env[name] || '');
}

// Fill a field located by the same key:/role: grammar as TAP_JS, entirely
// in-page: WebDriver has no way to hand our custom locator an element handle
// without an extra tagging round-trip, and the native-setter + input/change
// Provenance ledger for the broken-asset oracle: every value the fuzzer TYPES is
// recorded so brokenAssetScan can exclude an asset (or tofu) that exists only
// because a fuzzer-injected value was reflected into the DOM, not the app's own
// rendered content. Session-wide.
const INJECTED_VALUES = new Set();

// dispatch below is the standard way to update framework-bound fields (React
// tracks the native value descriptor). Returns true when a visible text-holding
// target was filled; false is a MISS.
const TYPE_JS = `
  const s = arguments[0];
  const value = arguments[1];
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const cssEscape = (v) => (
    window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\\\]/g, '\\\\$&')
  );
  let el = null;
  if (s.startsWith('key:')) {
    const body = s.slice(4);
    const ci = body.indexOf(':');
    if (ci < 0) return false;
    const kind = body.slice(0, ci);
    const val = body.slice(ci + 1);
    if (kind === 'testid') {
      el = document.querySelector('[data-testid="' + cssEscape(val) + '"]')
        || document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
    } else if (kind === 'id') {
      el = document.getElementById(val);
    } else if (kind === 'name') {
      el = document.querySelector('[name="' + cssEscape(val) + '"]');
    }
  } else if (s.startsWith('role:')) {
    const hash = s.indexOf('#');
    if (hash < 0) return false;
    const role = s.slice('role:'.length, hash);
    const idx = parseInt(s.slice(hash + 1), 10);
    if (!(idx >= 0)) return false;
    const roleOf = (el) => {
      const tag = el.tagName.toLowerCase();
      const ariaRole = (el.getAttribute('role') || '').toLowerCase();
      if (
        ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox'
      ) return 'textfield';
      if (tag === 'input') {
        const t = (el.getAttribute('type') || 'text').toLowerCase();
        if (
          ['checkbox', 'radio', 'range', 'button', 'submit', 'reset', 'image'].includes(t)
        ) return t;
        return 'textfield';
      }
      if (tag === 'textarea' || tag === 'select') return 'textfield';
      return ariaRole || tag;
    };
    let seen = -1, target = null;
    const walk = (el) => {
      if (target) return;
      if (!visible(el)) { for (const c of el.children) walk(c); return; }
      if (roleOf(el) === role) { seen++; if (seen === idx) { target = el; return; } }
      for (const c of el.children) walk(c);
    };
    const root = document.body || document.documentElement;
    if (root) walk(root);
    el = target;
  }
  if (!el || !visible(el)) return false;
  const tag = el.tagName.toLowerCase();
  const isText = tag === 'textarea'
    || (el.getAttribute &&
      (el.getAttribute('role') || '').toLowerCase().match(/textbox|searchbox|combobox/))
    || el.isContentEditable
    || (tag === 'input' && !['checkbox', 'radio', 'range', 'button', 'submit', 'reset', 'image']
      .includes((el.getAttribute('type') || 'text').toLowerCase()));
  if (!isText) return false;
  try { el.focus(); } catch (e) {}
  if (el.isContentEditable && !('value' in el)) {
    el.textContent = value;
  } else {
    const proto = tag === 'textarea' ? window.HTMLTextAreaElement : window.HTMLInputElement;
    const desc = proto && Object.getOwnPropertyDescriptor(proto.prototype, 'value');
    if (desc && desc.set) desc.set.call(el, value); else el.value = value;
  }
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return true;
`;

// Count VISIBLE elements matching a journey finder, for `expect: count`. Same
// key grammar as TAP_JS; any other finder is a raw CSS selector. Semantics are
// byte-identical to the web runner's countMatching.
const COUNT_JS = `
  const finder = arguments[0];
  const esc = (v) => (window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\\\]/g, '\\\\$&'));
  let sel = finder;
  if (finder.startsWith('key:')) {
    const body = finder.slice(4);
    const ci = body.indexOf(':');
    const kind = ci >= 0 ? body.slice(0, ci) : '';
    const val = ci >= 0 ? body.slice(ci + 1) : body;
    if (kind === 'testid') {
      sel = '[data-testid="' + esc(val) + '"],[data-test-id="' + esc(val) + '"]';
    }
    else if (kind === 'id') sel = '#' + esc(val);
    else if (kind === 'name') sel = '[name="' + esc(val) + '"]';
  }
  let els;
  try { els = document.querySelectorAll(sel); } catch (_) { return -1; }
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  let n = 0;
  for (const el of els) if (visible(el)) n++;
  return n;
`;

// Execute ONE scenario action, emitting the same FUZZ:ACT/MISS/ASSERT markers
// as the other runners' scenario paths. `type:` values are env-expanded
// literals (secrets arrive resolved from the host).
async function execScenarioAction(browser, act, who) {
  log('FUZZ:ACT ' + who + ' ' + act);
  if (act.startsWith('shoot:')) {
    await shoot(browser, act.slice('shoot:'.length));
    return;
  }
  if (act.startsWith('assert:')) {
    const body = act.slice('assert:'.length);
    if (body.startsWith('text=')) {
      const want = body.slice('text='.length);
      let ok = false;
      try {
        ok = await browser.execute(
          'return !!(document.body && document.body.innerText.' + 'includes(arguments[0]))',
          want,
        );
      } catch (_) {}
      log(
        'FUZZ:ASSERT ' + (ok ? 'pass' : 'fail') + ' text=' + JSON.stringify(want) + ' actor=' + who,
      );
    } else if (body.startsWith('count:')) {
      const rest = body.slice('count:'.length);
      const eq = rest.lastIndexOf('=');
      const finder = eq >= 0 ? rest.slice(0, eq) : rest;
      const want = eq >= 0 ? parseInt(rest.slice(eq + 1), 10) : 0;
      let got = -1;
      try {
        got = await browser.execute(COUNT_JS, finder);
      } catch (_) {}
      log(
        'FUZZ:ASSERT ' +
          (got === want ? 'pass' : 'fail') +
          ' count ' +
          finder +
          ' want=' +
          want +
          ' got=' +
          got +
          ' actor=' +
          who,
      );
    } else {
      log('FUZZ:ASSERT fail unsupported ' + body + ' actor=' + who);
    }
    await browser.pause(300);
    return;
  }
  if (act === 'back') {
    await browser.back().catch(() => {});
    await browser.pause(400);
    return;
  }
  if (act.startsWith('auth:')) {
    // Session-restore login is not wired on the Tauri runner; use a
    // `login(<account>)` actor prelude (UI flow) for multi-user auth. No-op so
    // ordering still advances, but flag it loudly.
    log('JOURNEY[a] step: auth-restore unsupported on tauri runner; use login() ' + 'for ' + act);
    await browser.pause(200);
    return;
  }
  if (act.startsWith('type:')) {
    const b = act.slice('type:'.length);
    const eq = b.lastIndexOf('=');
    const sel = eq >= 0 ? b.slice(0, eq) : b;
    const value = expandEnv(eq >= 0 ? b.slice(eq + 1) : '');
    if (value != null && String(value).length > 0) INJECTED_VALUES.add(String(value));
    let ok = false;
    try {
      ok = await browser.execute(TYPE_JS, sel, value);
    } catch (_) {}
    if (!ok) log('FUZZ:MISS ' + who + ' ' + act);
    await browser.pause(900);
    return;
  }
  const sel = act.slice('tap:'.length);
  const ok = await tap(browser, sel);
  if (!ok) log('FUZZ:MISS ' + who + ' ' + act);
  await browser.pause(900);
}

// Multi-actor: this runner is ONE actor. It drives the launched webview and
// pulls its next action from the host conductor (the strict step-order
// barrier), so N runners across N processes interleave exactly as the journey
// specifies. Universal wire protocol; only execScenarioAction is Tauri-specific.
async function runScenarioActor(browser) {
  const base = process.env.REPROIT_SCENARIO_BARRIER;
  // Role identity: an explicit label wins (each process gets its own env),
  // else claim a distinct role from the conductor.
  let who = process.env.REPROIT_DEVICE;
  if (!who) {
    try {
      who = (await (await fetch(base + '/claim')).text()).trim();
    } catch (_) {
      who = '';
    }
    if (!who || who.startsWith('ERR')) who = 'a';
  }
  log('JOURNEY claimed role=' + who);
  await browser.pause(1500);
  // Exception hooks so a renderer throw during any step surfaces as the same
  // EXCEPTION block the fuzz walk emits; drained after every action.
  await installHooks(browser);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  for (let guard = 0; guard < 100000; guard++) {
    let body = 'WAIT';
    try {
      body = (await (await fetch(base + '/next?device=' + who)).text()).trim();
    } catch {
      await sleep(100);
      continue;
    }
    if (body === 'DONE') break;
    if (body === 'WAIT') {
      await sleep(40);
      continue;
    }
    const act = body.startsWith('ACT\t') ? body.slice(4) : body;
    await execScenarioAction(browser, act, who);
    await installHooks(browser); // a navigation replaces the window; idempotent
    await drainErrors(browser);
    try {
      await fetch(base + '/done?device=' + who, { method: 'POST' });
    } catch (_) {}
  }
  await drainErrors(browser); // catch an error settled after the last step
  log('JOURNEY DONE');
  log('All tests passed');
}

async function main() {
