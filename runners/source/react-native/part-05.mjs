  const native = nativeIds instanceof Set ? nativeIds : new Set(nativeIds || []);
  const els = [];
  let idx = 0;
  for (const rec of records || []) {
    if (!rec || !rec.hasPress) continue; // only operable nodes are gap candidates
    // Join key: the fiber node's nativeID/testID, addressed in reproit's `key:`
    // grammar so it lines up with the runtime selector (idOfEl pulls the same id).
    // A node with no id can't be joined or fixed precisely; address it by a
    // synthetic structural index so the count is still reported.
    const sel = rec.id != null ? 'key:' + rec.id : 'fiber:press#' + idx;
    // accessible={false} hides the node from AT entirely: treat as no role AND no
    // name regardless of what role/label strings were set (they're inert then).
    const hidden = rec.accessible === false;
    // An operable node whose id never appeared in the native a11y tree was not
    // exposed to AT at all -> no role.
    const inNative = rec.id != null && native.has(rec.id);
    const rolePresent =
      !hidden && rec.role != null && (rec.id == null || inNative || native.size === 0);
    const namePresent = !hidden && rec.label != null;
    els.push({
      id: sel,
      operable: true,
      gestureKind: 'tap',
      a11y: {
        rolePresent,
        namePresent,
        // focusable / inTabOrder / keyboardActivatable: not asserted on a touch
        // surface; omitted so the engine defaults them true (no spurious flag).
      },
    });
    idx++;
  }
  // Deterministic order: by selector.
  els.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return els;
}

// Run the fiber probe over the RN JS runtime and emit EXPLORE:GROUNDTRUTH for
// the current state. Best-effort: a release build (no DevTools hook) or a
// transport that can't reach JS yields an empty-elements marker (still emitted,
// so the engine records "no gaps observed" rather than nothing). `nativeIds` is
// the set of stable ids the page-source snapshot saw, for the graph-1<->graph-2
// join. Never throws.
async function emitGroundtruth(driver, sig, nativeIds, nativeCandidates) {
  let result = null;
  // Appium exposes the RN JS runtime over `mobile: executeScript` on Hermes /
  // debug builds. WebdriverIO surfaces it through two execute entry points. We
  // try both documented call shapes and accept the first that returns our
  // { ok, records } shape.
  const tryRun = async (fn) => {
    try {
      const r = await fn();
      if (r && typeof r === 'object' && Array.isArray(r.records)) return r;
    } catch (e) {
      /* transport unavailable: fall through */
    }
    return null;
  };
  // UiAutomator2 cannot execute code in the app's JS runtime. Calling these
  // optional commands there only makes WebdriverIO print a scary server ERROR
  // before our fallback catches it, so go directly to native ground truth.
  if (!isAndroid() && typeof driver.executeScript === 'function') {
    result = await tryRun(() => driver.executeScript(FIBER_PROBE_SRC, []));
  }
  if (!isAndroid() && !result && typeof driver.execute === 'function') {
    result = await tryRun(() => driver.execute(FIBER_PROBE_SRC));
  }
  // PRIMARY: a successful fiber probe (Hermes/dev build with the DevTools hook)
  // with at least one operable record wins. It is the true graph-1 oracle: it
  // sees a press handler even on a node the native a11y tree never exposed.
  if (result && result.ok) {
    const elements = groundtruthFromFiber(result.records, nativeIds);
    if (elements.length > 0) {
      log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap: false, elements }));
      return;
    }
  }

  // FALLBACK: the fiber probe was unavailable (no JS channel: uiautomator2 on a
  // real device) or yielded no operable record. Derive groundtruth from the
  // native a11y tree instead: a pointer-operable, id-bearing element that exposes
  // a generic/non-button role (android.view.ViewGroup, role `group`, no AT role)
  // is the WCAG 4.1.2 no_role (+ pointer_only) gap; one that exposes a real role
  // is clean. This keeps RN operability working live where the fiber path can't.
  const nativeEls = groundtruthFromNative(nativeCandidates);
  if (nativeEls.length > 0) {
    const reason =
      result && result.ok
        ? 'fiber-empty'
        : result && result.reason
          ? result.reason
          : 'no-js-channel';
    log(
      'JOURNEY[a] step: groundtruth from native a11y tree (' +
        reason +
        '; ' +
        nativeEls.length +
        ' operable)',
    );
    log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap: false, elements: nativeEls }));
    return;
  }

  // Neither path produced anything: emit an empty ground-truth so the engine sees
  // the state was probed (no false gaps), and log why so the operator knows.
  const reason = result && result.reason ? result.reason : 'no-js-channel';
  log('JOURNEY[a] step: groundtruth probe skipped (' + reason + '; no fiber + no native operable)');
  log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap: false, elements: [] }));
}

export { groundtruthFromFiber };

// HOST-SIDE pure reducer for the NATIVE FALLBACK: turn the pointer-operable,
// id-bearing native-tree candidates (snapshot.nativeCandidates) into the same
// EXPLORE:GROUNDTRUTH `elements` list groundtruthFromFiber produces, for the case
// where the JS fiber probe could not run (uiautomator2 has no JS channel into the
// RN runtime on a real device). Each candidate is operable by pointer; we report
// `rolePresent` from whether the native node exposed a real AT role (it rendered
// as an android.widget.Button vs a bare android.view.ViewGroup) and `namePresent`
// from its accessible name. A role-less operable element is the WCAG 4.1.2 case:
// the engine counts it as a no_role gap, and because it is operable ONLY by
// pointer with no exposed semantics we also assert keyboardActivatable=false so
// the engine additionally counts it pointer_only. A candidate that DOES expose a
// real role is reported clean (all dims true) and is not a gap. Pure +
// deterministic (sorted by selector), so it is unit-testable without a device.
function groundtruthFromNative(candidates) {
  const els = [];
  for (const c of candidates || []) {
    if (!c || c.id == null) continue;
    const rolePresent = !!c.rolePresent;
    const namePresent = !!c.namePresent;
    els.push({
      id: 'key:' + c.id,
      operable: true,
      gestureKind: 'tap',
      a11y: {
        rolePresent,
        namePresent,
        // A role-less, pointer-operable native node is reachable ONLY by finger:
        // it carries no exposed semantics for a keyboard/switch user to activate,
        // so it is pointer_only. A node that exposes a real AT role is keyboard-
        // activatable like any focusable control; report it clean.
        keyboardActivatable: rolePresent,
        inTabOrder: rolePresent,
        focusable: rolePresent,
      },
    });
  }
  els.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return els;
}

export { groundtruthFromNative };

// ── Multi-actor scenario client (the conductor protocol) ────────────────────
// Same wire protocol as the web/electron/tauri runners, the flutter explorer
// and the tui backend: the host conductor (modes/barrier.rs) owns identity
// (`GET /claim`) and ordering (`GET /next?device=` + `POST /done?device=`);
// this process plays ONE actor over its OWN Appium session/device and only
// executes actions. Each actor is a separate device (the orchestrator boots N
// sims/emulators and hands each runner its own REPROIT_APPIUM_CAPS), so no
// input isolation is needed: the conductor serializes actions globally.

// Substitute ${VAR} from the environment (same contract as the web runner):
// journeys encode `secret:` fills as ${REPROIT_SECRET_<ACCT>_<FIELD>}
// placeholders so plaintext credentials never touch disk. Unset vars expand
// to "" (a missing credential types blank, which the app rejects).
function expandEnv(s) {
  return String(s).replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (_, name) => process.env[name] || '');
}

// Locator strategies for a structural selector, in the same order tap() tries
// them: accessibility id first, then resource-id / name / content-desc. A
// `role:<role>#<idx>` selector resolves through THIS snapshot's elements list
// (the same structural index basis as the signature), then locates by its
// key/label.
function locatorsFor(sel, snap) {
  if (sel.startsWith('key:')) {
    const id = sel.slice('key:'.length);
    return [
      `~${id}`,
      `//*[@resource-id="${id}"]`,
      `//*[contains(@resource-id,"/${id}")]`,
      `//*[@name="${id}"]`,
      `//*[@content-desc="${id}"]`,
    ];
  }
  if (sel.startsWith('role:')) {
    const el = ((snap && snap.elements) || []).find((e) => e.sel === sel);
    if (!el) return [];
    const out = [];
    if (el.key) out.push(`~${el.key}`, `//*[@resource-id="${el.key}"]`, `//*[@name="${el.key}"]`);
    if (el.label)
      out.push(
        `~${el.label}`,
        `//*[@label="${el.label}"]`,
        `//*[@text="${el.label}"]`,
        `//*[@content-desc="${el.label}"]`,
      );
    return out;
  }
  return [];
}

// Resolve a structural selector to a live element, or null. Never throws.
async function findEl(driver, sel, snap) {
  for (const s of locatorsFor(sel, snap)) {
    try {
      const el = await driver.$(s);
      if (await el.isExisting()) return el;
    } catch {
      /* next strategy */
    }
  }
  return null;
}

// Fill a field located by the same key:/role: grammar as tap(). setValue clears
// existing content and types via the platform input path, so framework change
// handlers fire. A missing/unreachable target returns false so the caller
// reports a MISS rather than silently passing.
async function typeInto(driver, sel, value, snap) {
  const el = await findEl(driver, sel, snap);
  if (!el) return false;
  if (value != null && String(value).length > 0) INJECTED_VALUES.add(String(value));
  try {
    await el.setValue(value);
  } catch {
    return false;
  }
  return true;
}

// Count elements matching a journey finder, for `expect: count`. A `key:<id>`
// finder counts live matches of its first non-empty locator strategy (the same
// strategies tap resolves through); any other finder counts occurrences across
// this snapshot's visible display text (labels + texts), the same substring
// semantics the tui runner uses for its text-only surface.
async function countMatching(driver, finder, snap) {
  if (finder.startsWith('key:')) {
    for (const s of locatorsFor(finder, snap)) {
      try {
        const els = await driver.$$(s);
        if (els.length > 0) return els.length;
      } catch {
        /* next strategy */
      }
    }
    return 0;
  }
  const blob = visibleTextBlob(snap);
  return finder ? blob.split(finder).length - 1 : 0;
}

// The visible display text of a snapshot: labels + captured text nodes, joined.
// Feeds assert:text= / assert:count: with the same substring semantics as tui.
// texts entries are {text, bounds} records (the EXPLORE:STATE shape); only the
// text participates, on every platform alike.
function visibleTextBlob(snap) {
  const parts = [
    ...((snap && snap.labels) || []),
    ...((snap && snap.texts) || []).map((t) => (t && t.text != null ? t.text : '')),
  ];
  return parts.join('\n');
}

// Execute ONE scenario action, emitting the same FUZZ:ACT/MISS/ASSERT markers
// as the other runners' scenario paths. `who` is this runner's role letter,
// for log attribution. A fresh snapshot is taken per action so role:<role>#<idx>
// selectors and asserts see the CURRENT screen (a peer's action may have moved
// this device's UI, e.g. an incoming message).
async function execScenarioAction(driver, act, who, valueNodeSelectors) {
  log('FUZZ:ACT ' + who + ' ' + act);
  await advanceCausalAction(driver);
  if (act.startsWith('shoot:')) {
    // Appium devices are captured orchestrator-side (simctl/adb) from the SHOOT
    // marker; the runner only names the point (same contract as fuzz replay).
    log('SHOOT:' + act.slice('shoot:'.length));
    return;
  }
  const snap = await snapshot(driver, valueNodeSelectors).catch(() => null);
  if (act.startsWith('assert:')) {
    const body = act.slice('assert:'.length);
    if (body.startsWith('text=')) {
      const want = body.slice('text='.length);
      const ok = visibleTextBlob(snap).includes(want);
      log(
        'FUZZ:ASSERT ' + (ok ? 'pass' : 'fail') + ' text=' + JSON.stringify(want) + ' actor=' + who,
      );
    } else if (body.startsWith('count:')) {
      const rest = body.slice('count:'.length);
      const eq = rest.lastIndexOf('=');
      const finder = eq >= 0 ? rest.slice(0, eq) : rest;
      const want = eq >= 0 ? parseInt(rest.slice(eq + 1), 10) : 0;
      const got = await countMatching(driver, finder, snap);
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
    await driver.pause(300);
    return;
  }
  if (act === 'back') {
    try {
      await driver.back();
    } catch {
      /* iOS: no hardware back; harmless */
    }
    await driver.pause(500);
    return;
  }
  if (act.startsWith('auth:')) {
    // Session-restore login is not wired on the Appium runner; use a
    // `login(<account>)` actor prelude (UI flow) for multi-user auth. No-op so
    // ordering still advances, but flag it loudly.
    log('JOURNEY[a] step: auth-restore unsupported on appium runner; use login()' + ' for ' + act);
    await driver.pause(200);
    return;
  }
  if (act.startsWith('type:')) {
    const b = act.slice('type:'.length);
    const eq = b.lastIndexOf('=');
    const sel = eq >= 0 ? b.slice(0, eq) : b;
    const value = expandEnv(eq >= 0 ? b.slice(eq + 1) : '');
    const ok = await typeInto(driver, sel, value, snap);
    if (!ok) log('FUZZ:MISS ' + who + ' ' + act);
    await driver.pause(800);
    return;
  }
  if (act.startsWith('tap:')) {
    const sel = act.slice('tap:'.length);
    const ok = snap ? await tap(driver, sel, snap) : false;
    if (!ok) log('FUZZ:MISS ' + who + ' ' + act);
    await driver.pause(800);
    return;
  }
  // A key:<Name> or other cross-surface action authored for a different
  // backend: fail loudly instead of silently passing.
  log('FUZZ:MISS ' + who + ' ' + act);
}

// Multi-actor: this runner is ONE actor. It drives its own Appium session and
// pulls its next action from the host conductor (the strict step-order
// barrier), so N runners across N devices interleave exactly as the journey
// specifies. Universal wire protocol; only execScenarioAction is
// Appium-specific. Crash detection is the same oracle as fuzzing (the target
// app leaving the foreground); a crashed actor deliberately does NOT ack its
// step, so the conductor's diagnose() names this actor and action.
async function runScenarioActor(driver, valueNodeSelectors) {
  const base = process.env.REPROIT_SCENARIO_BARRIER;
  // Role identity: an explicit label wins (each runner process gets its own
  // env), else claim a distinct role from the conductor, which hands out `a`,
  // `b`, ... atomically so two actors can never collide.
  let who = process.env.REPROIT_DEVICE;
  if (!who) {
    try {
      who = (await (await fetch(base + '/claim')).text()).trim();
    } catch {
      who = '';
    }
    if (!who || who.startsWith('ERR')) who = 'a';
  }
  log('JOURNEY claimed role=' + who);
  await driver.pause(1200);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  let crashed = false;
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
    await execScenarioAction(driver, act, who, valueNodeSelectors);
    if (await appCrashed(driver)) {
      emitCrash(act);
      crashed = true;
      break;
    }
    try {
      await fetch(base + '/done?device=' + who, { method: 'POST' });
    } catch {
      /* retry via next poll */
    }
  }
  log('JOURNEY DONE');
  log(crashed ? 'Some tests failed' : 'All tests passed');
}

export { runScenarioActor, execScenarioAction, locatorsFor };

const SESSION_DELETE_TIMEOUT_MS = 10_000;
const SESSION_DELETE_GRACE_MS = 250;

// Session creation can need a long timeout while XCUITest builds WDA. Reusing
// that timeout for DELETE /session can strand an otherwise complete journey
// until the outer native gate is killed. The Appium and simulator wrappers own
// final process cleanup, so a bounded delete failure is recorded and handed
// back to those owners instead of being mistaken for an application failure.
export async function closeDriverSession(driver) {
  if (driver.options && typeof driver.options === 'object') {
    driver.options.connectionRetryTimeout = SESSION_DELETE_TIMEOUT_MS;
    driver.options.connectionRetryCount = 0;
  }

  let timeout;
  const deleteOutcome = Promise.resolve()
    .then(() => driver.deleteSession())
    .then(
      () => 'deleted',
      () => 'fallback',
    );
  const boundedOutcome = new Promise((resolve) => {
    timeout = setTimeout(
      () => resolve('fallback'),
      SESSION_DELETE_TIMEOUT_MS + SESSION_DELETE_GRACE_MS,
    );
  });
  const outcome = await Promise.race([deleteOutcome, boundedOutcome]);
  clearTimeout(timeout);
  if (outcome === 'fallback') {
    log(
      'REPROIT:CLEANUP ' +
        JSON.stringify({
          session: 'appium',
          outcome,
          reason: 'delete-session-unavailable',
          timeoutMs: SESSION_DELETE_TIMEOUT_MS,
          owner: 'appium-and-target-wrapper',
        }),
    );
  }
  return outcome;
}

async function main() {
