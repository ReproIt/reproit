
// ── --record clip capture (route B: host film + box-spec) ───────────────────
// Electron's renderer is Chromium, so we ALREADY film the window with
// Playwright's recordVideo (window-only by construction: it captures the
// renderer surface, never the desktop -- the hard privacy rule). To match the
// uniform native host path (record_native_clips wants clip.mov + box-spec.json,
// then draws the box with box-overlay.mjs), we resolve the finding's element to
// a viewport-relative rect in CSS-px logical space, write box-spec.json, and
// remux the recorded .webm to clip.mov. box-overlay scales the rect by
// recordedPixels/logical (DPR-safe) and draws the same red box + caption chip
// the live web overlay draws.

// Resolve the finding's element (by the SAME key:/role: selector grammar tap()
// uses) to a viewport-relative box in CSS px, scrolling it into view and letting
// the scroll settle first (so the rect matches the frames filmed after this
// returns). Returns { x, y, w, h, videoW, videoH } or null if unresolved.
async function resolveClipBox(page, sel) {
  const handle = await page.evaluateHandle(resolveStructuralTarget, sel).catch(() => null);
  const target = handle ? handle.asElement() : null;
  if (!target) {
    if (handle) await handle.dispose().catch(() => {});
    return null;
  }
  const box = await page
    .evaluate(async (el) => {
      // Bring the element into the recorded frame INSTANTLY (not smooth): a
      // smooth animation is still moving when we measure, so the rect would
      // diverge from the settled frame the video holds -- the box lands off the
      // element. An instant scroll settles in one frame, so the measured rect
      // equals the held frame. Wait a couple of frames for any reflow.
      try {
        el.scrollIntoView({ behavior: 'instant', block: 'center', inline: 'center' });
      } catch (_) {
        try {
          el.scrollIntoView({ block: 'center', inline: 'center' });
        } catch (__) {}
      }
      let lastY = -1,
        stable = 0;
      for (let i = 0; i < 20; i++) {
        await new Promise((r) => setTimeout(r, 50));
        const y = window.scrollY;
        if (y === lastY) {
          if (++stable >= 2) break;
        } else {
          stable = 0;
          lastY = y;
        }
      }
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) return null;
      const vw = window.innerWidth || document.documentElement.clientWidth || 1;
      const vh = window.innerHeight || document.documentElement.clientHeight || 1;
      // Clamp the box inside the viewport (an inset) so a box always lands on
      // camera even when the element sits flush to an edge -- mirrors the web
      // overlay's clamp. box-overlay draws exactly this rect (scaled to pixels).
      const ins = 4;
      const left = Math.min(Math.max(r.left - 2, ins), Math.max(ins, vw - ins - 8));
      const top = Math.min(Math.max(r.top - 2, ins), Math.max(ins, vh - ins - 8));
      const w = Math.max(8, Math.min(r.width + 4, vw - left - ins));
      const h = Math.max(8, Math.min(r.height + 4, vh - top - ins));
      return { x: left, y: top, w, h, videoW: vw, videoH: vh };
    }, target)
    .catch(() => null);
  await target.dispose().catch(() => {});
  return box;
}

// Remux the Playwright-recorded .webm to a clip.mov the host box-overlay step
// reads (record_native_clips looks for `clip.mov` by name). box-overlay
// re-encodes to h264 mp4 anyway, so a straight transcode to h264/mov is enough;
// returns true on success.
function remuxToMov(webm, mov) {
  if (!webm || !existsSync(webm)) return false;
  const r = spawnSync(
    'ffmpeg',
    [
      '-hide_banner',
      '-loglevel',
      'error',
      '-y',
      '-i',
      webm,
      '-c:v',
      'libx264',
      '-pix_fmt',
      'yuv420p',
      '-an',
      mov,
    ],
    { stdio: ['ignore', 'inherit', 'inherit'] },
  );
  return r.status === 0 && existsSync(mov);
}

// ── Multi-actor scenario client (the conductor protocol) ────────────────────
// Same wire protocol as the web runner / flutter explorer / tui backend: the
// host conductor owns identity (`GET /claim`) and ordering (`GET /next` +
// `POST /done`); this process plays ONE actor and only executes actions.

// Count VISIBLE elements matching a journey finder, for `expect: count`. Runs
// in the renderer (passed to page.evaluate). Same key grammar as tap(); any
// other finder is treated as a raw CSS selector. Byte-identical to the web
// runner's countMatching so `expect:` means the same thing on both surfaces.
function countMatching(finder) {
  const esc = (v) => (window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\]/g, '\\$&'));
  let sel = finder;
  if (finder.startsWith('key:')) {
    const body = finder.slice(4);
    const ci = body.indexOf(':');
    const kind = ci >= 0 ? body.slice(0, ci) : '';
    const val = ci >= 0 ? body.slice(ci + 1) : body;
    if (kind === 'testid')
      sel = '[data-testid="' + esc(val) + '"],[data-test-id="' + esc(val) + '"]';
    else if (kind === 'id') sel = '#' + esc(val);
    else if (kind === 'name') sel = '[name="' + esc(val) + '"]';
  }
  let els;
  try {
    els = document.querySelectorAll(sel);
  } catch (_) {
    return -1;
  }
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  let n = 0;
  for (const el of els) if (visible(el)) n++;
  return n;
}

// Fill a field located by the same key:/role: grammar as tap(), typing via the
// real keyboard so framework input handlers fire (port of the web runner's
// typeInto; Electron's renderer is a Playwright Page, so the same API drives
// it). A missing/unreachable/non-text target returns false so the caller
// reports a MISS rather than silently passing.
async function typeInto(page, sel, value) {
  if (value != null && String(value).length > 0) INJECTED_VALUES.add(String(value));
  // The SHARED resolver, the same one tap() uses. This function used to carry a
  // THIRD role vocabulary of its own -- no tappable filter at all, and a roleOf
  // that fell through to `ariaRole || tag` -- so `role:textfield#N` counted a
  // different set again depending on whether the walk chose to tap or to type.
  const handle = await page.evaluateHandle(resolveStructuralTarget, sel).catch(() => null);
  const el0 = handle ? handle.asElement() : null;
  if (!el0) {
    if (handle) await handle.dispose().catch(() => {});
    return false;
  }
  const found = await page
    .evaluate((el) => {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) return false;
      const st = getComputedStyle(el);
      if (st.visibility === 'hidden' || st.display === 'none') return false;
      // Only type into things that hold text; a non-text target is a miss so the
      // caller treats it like a failed action rather than silently no-op'ing.
      const tag = el.tagName.toLowerCase();
      const isText =
        tag === 'textarea' ||
        (el.getAttribute &&
          (el.getAttribute('role') || '').toLowerCase().match(/textbox|searchbox|combobox/)) ||
        el.isContentEditable ||
        (tag === 'input' &&
          !['checkbox', 'radio', 'range', 'button', 'submit', 'reset', 'image'].includes(
            (el.getAttribute('type') || 'text').toLowerCase(),
          ));
      if (!isText) return false;
      try {
        el.focus();
      } catch (e) {}
      el.setAttribute('data-reproit-typed', '1');
      return true;
    }, el0)
    .catch(() => false);
  await el0.dispose().catch(() => {});
  if (!found) return false;
  // Type via the real keyboard so framework input handlers fire, then commit
  // with Enter. Clear any existing content first for determinism.
  try {
    await page.evaluate(() => {
      const el = document.querySelector('[data-reproit-typed="1"]');
      if (!el) return;
      el.removeAttribute('data-reproit-typed');
      if ('value' in el) el.value = '';
      else if (el.isContentEditable) el.textContent = '';
    });
    if (value.length) await page.keyboard.insertText(value);
    // Fire input/change so frameworks that bind on them update their model.
    await page.evaluate((v) => {
      const ae = document.activeElement;
      if (!ae) return;
      if ('value' in ae && ae.value !== v && v.length) ae.value = v;
      ae.dispatchEvent(new Event('input', { bubbles: true }));
      ae.dispatchEvent(new Event('change', { bubbles: true }));
    }, value);
    await page.keyboard.press('Enter');
  } catch (e) {
    return false;
  }
  return true;
}

// Execute ONE scenario action, emitting the same FUZZ:ACT/MISS/ASSERT markers
// as the web runner's scenario path. `who` is this runner's role letter, for
// log attribution. `type:` values are env-expanded literals (secrets arrive
// resolved from the host); the web runner's adversarial-class tokens do not
// apply to authored scenario fills.
async function execScenarioAction(page, act, who) {
  log('FUZZ:ACT ' + who + ' ' + act);
  if (act.startsWith('shoot:')) {
    await shoot(page, act.slice('shoot:'.length));
    return;
  }
  if (act.startsWith('assert:')) {
    const body = act.slice('assert:'.length);
    if (body.startsWith('text=')) {
      const want = body.slice('text='.length);
      const ok = await page
        .evaluate((t) => !!(document.body && document.body.innerText.includes(t)), want)
        .catch(() => false);
      log(
        'FUZZ:ASSERT ' + (ok ? 'pass' : 'fail') + ' text=' + JSON.stringify(want) + ' actor=' + who,
      );
    } else if (body.startsWith('count:')) {
      const rest = body.slice('count:'.length);
      const eq = rest.lastIndexOf('=');
      const finder = eq >= 0 ? rest.slice(0, eq) : rest;
      const want = eq >= 0 ? parseInt(rest.slice(eq + 1), 10) : 0;
      const got = await page.evaluate(countMatching, finder).catch(() => -1);
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
    await page.waitForTimeout(300);
    return;
  }
  if (act === 'back') {
    await page.goBack({ timeout: 3000 }).catch(() => {});
    await page.waitForTimeout(400);
    return;
  }
  if (act.startsWith('auth:')) {
    // Session-restore login is not wired on the Electron runner; use a
    // `login(<account>)` actor prelude (UI flow) for multi-user auth. No-op so
    // ordering still advances, but flag it loudly.
    log(
      'JOURNEY[a] step: auth-restore unsupported on electron runner; use ' + 'login() for ' + act,
    );
    await page.waitForTimeout(200);
    return;
  }
  if (act.startsWith('type:')) {
    const b = act.slice('type:'.length);
    const eq = b.lastIndexOf('=');
    const sel = eq >= 0 ? b.slice(0, eq) : b;
    const value = expandEnv(eq >= 0 ? b.slice(eq + 1) : '');
    const ok = await typeInto(page, sel, value);
    if (!ok) log('FUZZ:MISS ' + who + ' ' + act);
    await page.waitForTimeout(900);
    return;
  }
  const sel = act.slice('tap:'.length);
  const ok = await tap(page, sel);
  if (!ok) log('FUZZ:MISS ' + who + ' ' + act);
  await page.waitForTimeout(900);
}

// Multi-actor: this runner is ONE actor. It drives the already-launched app
// window and pulls its next action from the host conductor (the strict
// step-order barrier), so N runners across N processes interleave exactly as
// the journey specifies. Universal wire protocol; only execScenarioAction is
// Electron-specific.
async function runScenarioActor(page) {
  const base = process.env.REPROIT_SCENARIO_BARRIER;
  // Role identity: an explicit label wins (each process gets its own env),
  // else claim a distinct role from the conductor, which hands out `a`, `b`,
  // ... atomically so two actors can never collide.
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
  await page.waitForTimeout(1200);
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
    await execScenarioAction(page, act, who);
    try {
      await fetch(base + '/done?device=' + who, { method: 'POST' });
    } catch (_) {}
  }
  await page.waitForTimeout(500); // flush a trailing pageerror before teardown
  log('JOURNEY DONE');
  log('All tests passed');
}

async function main() {
