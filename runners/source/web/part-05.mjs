  const ok = await page
    .evaluate(
      ({ s, mark, box, boxColor }) => {
        const visible = (el) => {
          const r = el.getBoundingClientRect();
          if (r.width === 0 || r.height === 0) return false;
          const st = getComputedStyle(el);
          return st.visibility !== 'hidden' && st.display !== 'none';
        };
        // Same reachability floor as snapshot(): center on-screen AND hit-test there
        // resolves to the element or a descendant. Kept in lockstep so role+index
        // resolution counts exactly the candidates snapshot() offered, an offstage
        // control consumes no index and can't be reached by any selector.
        const reachable = (el) => {
          if (!visible(el)) return false;
          const r = el.getBoundingClientRect();
          const cx = r.left + r.width / 2;
          const cy = r.top + r.height / 2;
          const vw = window.innerWidth || document.documentElement.clientWidth;
          const vh = window.innerHeight || document.documentElement.clientHeight;
          if (cx < 0 || cy < 0 || cx >= vw || cy >= vh) return false;
          const hit = document.elementFromPoint(cx, cy);
          if (!hit) return false;
          return hit === el || el.contains(hit);
        };
        // Production sessions can be captured with a taller/shorter viewport than
        // the developer's runner. A style-visible target that is merely offscreen is
        // still the same structural control: scroll it into view before declaring a
        // stale replay. Hidden or occluded controls remain misses.
        const bringIntoReach = (el) => {
          if (reachable(el)) return true;
          if (!visible(el)) return false;
          const r = el.getBoundingClientRect();
          const vw = window.innerWidth || document.documentElement.clientWidth;
          const vh = window.innerHeight || document.documentElement.clientHeight;
          const offscreen =
            r.right <= 0 ||
            r.bottom <= 0 ||
            r.left >= vw ||
            r.top >= vh ||
            r.left < 0 ||
            r.top < 0 ||
            r.right > vw ||
            r.bottom > vh;
          if (!offscreen) return false;
          try {
            el.scrollIntoView({ behavior: 'auto', block: 'center', inline: 'nearest' });
          } catch (_) {}
          return reachable(el);
        };
        const cssEscape = (v) =>
          window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\]/g, '\\$&');
        // On a recorded replay, tag the clicked element so a crash/jank/hang box can
        // point at exactly the control the user actuated (only the LAST one carries
        // the tag). Gated on `mark` so a normal fuzz walk never touches the DOM.
        const doClick = (el) => {
          if (mark) {
            try {
              for (const e of document.querySelectorAll('[data-reproit-trigger]'))
                e.removeAttribute('data-reproit-trigger');
              el.setAttribute('data-reproit-trigger', '1');
            } catch (_) {}
          }
          // PREVIEW (`box`): instead of clicking, highlight the element reproit is
          // ABOUT to tap, with a human-readable caption, drawn while the page is still
          // live. So a tap that then navigates / freezes / crashes still shows the
          // right element and the right name (a frozen page can't be annotated after).
          if (box) {
            // Minimal motion: scroll to the element ONLY if it is not already fully in
            // view, and centre it just enough to keep it on screen -- a clip should not
            // re-scroll a control the viewer can already see.
            try {
              const rr = el.getBoundingClientRect();
              const vh = window.innerHeight || document.documentElement.clientHeight;
              const vw = window.innerWidth || document.documentElement.clientWidth;
              const inView = rr.top >= 0 && rr.left >= 0 && rr.bottom <= vh && rr.right <= vw;
              if (!inView)
                el.scrollIntoView({ behavior: 'smooth', block: 'center', inline: 'nearest' });
            } catch (_) {}
            const old = document.getElementById('__reproit_tapbox');
            if (old) old.remove();
            const r = el.getBoundingClientRect();
            const layer = document.createElement('div');
            layer.id = '__reproit_tapbox';
            layer.style.cssText =
              'position:absolute;top:0;left:0;width:0;height:0;z-index:2147483646;' +
              'pointer-events:none';
            const b = document.createElement('div');
            const col = boxColor || '#2f6bff';
            b.style.cssText = [
              'position:absolute',
              'top:' + (r.top + window.scrollY - 2) + 'px',
              'left:' + (r.left + window.scrollX - 2) + 'px',
              'width:' + (r.width + 4) + 'px',
              'height:' + (r.height + 4) + 'px',
              'border:3px solid ' + col,
              'background:' + col + '20',
              'border-radius:4px',
              'box-shadow:0 0 0 1px rgba(255,255,255,.5),0 4px 18px rgba(0,0,0,.35)',
            ].join(';');
            const tag = document.createElement('div');
            tag.textContent = box;
            tag.style.cssText = [
              'position:absolute',
              'top:-22px',
              'left:-3px',
              'background:' + col,
              'color:#fff',
              'font:600 12px/1 ui-monospace,SFMono-Regular,Menlo,monospace',
              'padding:4px 7px',
              'border-radius:5px',
              'white-space:nowrap',
              'box-shadow:0 2px 8px rgba(0,0,0,.4)',
            ].join(';');
            b.appendChild(tag);
            layer.appendChild(b);
            (document.body || document.documentElement).appendChild(layer);
            return { preview: true };
          }
          // Stash the clicked element for the post-tap oracle probes (the
          // duplicate-submit eligibility check and the focus-loss guards read it
          // in-page). A window ref only, never a DOM mutation, so the signature/
          // content/mutation oracles are untouched.
          try {
            window.__reproitLastTap = el;
            // Record whether the browser's pointer activation focused the target
            // before application click handlers can replace it.
            if (window.__reproitFocusProbe) {
              window.__reproitTapFocused = false;
              el.addEventListener(
                'click',
                () => {
                  window.__reproitTapFocused = document.activeElement === el;
                },
                { capture: true, once: true },
              );
            }
          } catch (_) {}
          const rect = el.getBoundingClientRect();
          return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
        };

        if (s.startsWith('key:')) {
          const body = s.slice(4);
          const ci = body.indexOf(':');
          if (ci < 0) return false;
          const kind = body.slice(0, ci);
          const val = body.slice(ci + 1);
          let el = null;
          if (kind === 'testid') {
            el =
              document.querySelector('[data-testid="' + cssEscape(val) + '"]') ||
              document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
          } else if (kind === 'id') {
            el = document.getElementById(val);
          } else if (kind === 'name') {
            el = document.querySelector('[name="' + cssEscape(val) + '"]');
          }
          if (!el) return false;
          // A keyed control may be below the fold on this runner even though it was
          // reachable in production. Scroll only that case; auth-gated, hidden, and
          // occluded controls still fail as stale.
          if (!bringIntoReach(el)) return false;
          return doClick(el);
        }

        if (s.startsWith('role:')) {
          const hash = s.indexOf('#');
          if (hash < 0) return false;
          const role = s.slice('role:'.length, hash);
          const idx = parseInt(s.slice(hash + 1), 10);
          if (!(idx >= 0)) return false;
          // Re-derive document-order tappables of this role from the live tree using
          // the SAME canonical role logic as snapshot(), and click the idx-th. No text.
          const ROLES = {
            screen: 1,
            header: 1,
            text: 1,
            button: 1,
            link: 1,
            textfield: 1,
            image: 1,
            icon: 1,
            list: 1,
            listitem: 1,
            tab: 1,
            switch: 1,
            checkbox: 1,
            radio: 1,
            slider: 1,
            menu: 1,
            menuitem: 1,
            dialog: 1,
            group: 1,
            node: 1,
          };
          const roleOf = (el) => {
            const tag = el.tagName.toLowerCase();
            const ariaRole = (el.getAttribute('role') || '').toLowerCase();
            if (ariaRole) {
              if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox')
                return 'textfield';
              if (ariaRole === 'heading') return 'header';
              if (ariaRole === 'img') return 'image';
              if (ariaRole === 'switch') return 'switch';
              if (ariaRole === 'link') return 'link';
              if (ariaRole === 'button') return 'button';
              if (ROLES[ariaRole]) return ariaRole;
            }
            if (tag === 'input') {
              const t = (el.getAttribute('type') || 'text').toLowerCase();
              if (t === 'checkbox') return 'checkbox';
              if (t === 'radio') return 'radio';
              if (t === 'range') return 'slider';
              if (['button', 'submit', 'reset', 'image'].includes(t)) return 'button';
              return 'textfield';
            }
            if (tag === 'textarea' || tag === 'select') return 'textfield';
            if (tag === 'a') return 'link';
            if (tag === 'button') return 'button';
            if (tag === 'img' || tag === 'svg') return 'image';
            if (/^h[1-6]$/.test(tag) || tag === 'header') return 'header';
            if (tag === 'ul' || tag === 'ol') return 'list';
            if (tag === 'li') return 'listitem';
            if (tag === 'dialog') return 'dialog';
            if (tag === 'nav' || tag === 'menu') return 'menu';
            return 'node';
          };
          const interactive = (el, r) => {
            const tag = el.tagName.toLowerCase();
            if (['a', 'button', 'select'].includes(tag)) return true;
            // Keep this in lockstep with snapshot()'s interactive() so role+index
            // ordering is identical: text fields are actionable (driven by "type").
            if (tag === 'input' || tag === 'textarea') return true;
            if (r === 'textfield') return true;
            if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(r))
              return true;
            if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
            return false;
          };
          let seen = -1,
            target = null;
          const walk = (el) => {
            if (target) return;
            if (!visible(el)) {
              for (const c of el.children) walk(c);
              return;
            }
            const r = roleOf(el);
            // Count every style-visible interactive, matching the production SDK and
            // snapshot()'s viewport-independent positional index. Reachability is
            // checked only after the structural target is selected.
            if (interactive(el, r) && r === role) {
              seen++;
              if (seen === idx) {
                target = el;
                return;
              }
            }
            for (const c of el.children) walk(c);
          };
          const root = document.body || document.documentElement;
          if (root) walk(root);
          if (!target) return false;
          if (!bringIntoReach(target)) return false;
          return doClick(target);
        }

        return false;
      },
      {
        s: sel,
        mark: !!(opts && opts.mark),
        box: (opts && opts.box) || null,
        boxColor: (opts && opts.boxColor) || null,
      },
    )
    .catch(() => null);
  if (!ok) return false;
  if (ok.preview) return true;
  try {
    await page.mouse.click(ok.x, ok.y, { delay: 10 });
    return true;
  } catch (_) {
    return false;
  }
}

// STRUCTURAL type: resolve the SAME locale-invariant selector as tap() and type
// `value` into the field, then press Enter (many apps, e.g. TodoMVC's new-todo,
// commit on Enter). Focuses the element, sets its value, and dispatches the
// input/change events frameworks listen for. Returns true on success. The
// selector resolution mirrors tap() exactly so role+index addressing lines up.
// Provenance ledger for the broken-asset oracle: every value the fuzzer TYPES is
// recorded here so brokenAssetScan can exclude an asset (or tofu) that exists only
// because a fuzzer-injected value was reflected into the DOM (the XSS-probe
// `<img src=x>` case), not the app's own rendered content. Session-wide.
const INJECTED_VALUES = new Set();
async function typeInto(page, sel, value, opts) {
  if (value != null && String(value).length > 0) INJECTED_VALUES.add(String(value));
  const found = await page
    .evaluate(
      ({ s, mark }) => {
        const visible = (el) => {
          const r = el.getBoundingClientRect();
          if (r.width === 0 || r.height === 0) return false;
          const st = getComputedStyle(el);
          return st.visibility !== 'hidden' && st.display !== 'none';
        };
        // Same reachability floor as snapshot()/tap(): center on-screen AND hit-test
        // resolves to the element or a descendant. Kept in lockstep so role+index
        // resolution counts exactly the fields snapshot() offered.
        const reachable = (el) => {
          if (!visible(el)) return false;
          const r = el.getBoundingClientRect();
          const cx = r.left + r.width / 2;
          const cy = r.top + r.height / 2;
          const vw = window.innerWidth || document.documentElement.clientWidth;
          const vh = window.innerHeight || document.documentElement.clientHeight;
          if (cx < 0 || cy < 0 || cx >= vw || cy >= vh) return false;
          const hit = document.elementFromPoint(cx, cy);
          if (!hit) return false;
          return hit === el || el.contains(hit);
        };
        const bringIntoReach = (el) => {
          if (reachable(el)) return true;
          if (!visible(el)) return false;
          const r = el.getBoundingClientRect();
          const vw = window.innerWidth || document.documentElement.clientWidth;
          const vh = window.innerHeight || document.documentElement.clientHeight;
          const offscreen =
            r.right <= 0 ||
            r.bottom <= 0 ||
            r.left >= vw ||
            r.top >= vh ||
            r.left < 0 ||
            r.top < 0 ||
            r.right > vw ||
            r.bottom > vh;
          if (!offscreen) return false;
          try {
            el.scrollIntoView({ behavior: 'auto', block: 'center', inline: 'nearest' });
          } catch (_) {}
          return reachable(el);
        };
        const cssEscape = (v) =>
          window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\]/g, '\\$&');

        let el = null;
        if (s.startsWith('key:')) {
          const body = s.slice(4);
          const ci = body.indexOf(':');
          if (ci < 0) return false;
          const kind = body.slice(0, ci);
          const val = body.slice(ci + 1);
          if (kind === 'testid') {
            el =
              document.querySelector('[data-testid="' + cssEscape(val) + '"]') ||
              document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
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
          const ROLES = {
            screen: 1,
            header: 1,
            text: 1,
            button: 1,
            link: 1,
            textfield: 1,
            image: 1,
            icon: 1,
            list: 1,
            listitem: 1,
            tab: 1,
            switch: 1,
            checkbox: 1,
            radio: 1,
            slider: 1,
            menu: 1,
            menuitem: 1,
            dialog: 1,
            group: 1,
            node: 1,
          };
          const roleOf = (el) => {
            const tag = el.tagName.toLowerCase();
            const ariaRole = (el.getAttribute('role') || '').toLowerCase();
            if (ariaRole) {
              if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox')
                return 'textfield';
              if (ariaRole === 'heading') return 'header';
              if (ariaRole === 'img') return 'image';
              if (ariaRole === 'switch') return 'switch';
              if (ariaRole === 'link') return 'link';
              if (ariaRole === 'button') return 'button';
              if (ROLES[ariaRole]) return ariaRole;
            }
            if (tag === 'input') {
              const t = (el.getAttribute('type') || 'text').toLowerCase();
              if (t === 'checkbox') return 'checkbox';
              if (t === 'radio') return 'radio';
              if (t === 'range') return 'slider';
              if (['button', 'submit', 'reset', 'image'].includes(t)) return 'button';
              return 'textfield';
            }
            if (tag === 'textarea' || tag === 'select') return 'textfield';
            if (tag === 'a') return 'link';
            if (tag === 'button') return 'button';
            if (tag === 'img' || tag === 'svg') return 'image';
            if (/^h[1-6]$/.test(tag) || tag === 'header') return 'header';
            if (tag === 'ul' || tag === 'ol') return 'list';
            if (tag === 'li') return 'listitem';
            if (tag === 'dialog') return 'dialog';
            if (tag === 'nav' || tag === 'menu') return 'menu';
            return 'node';
          };
          const interactive = (el, r) => {
            const tag = el.tagName.toLowerCase();
            if (['a', 'button', 'select'].includes(tag)) return true;
            if (tag === 'input' || tag === 'textarea') return true;
            if (r === 'textfield') return true;
            if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(r))
              return true;
            if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
            return false;
          };
          let seen = -1,
            target = null;
          const walk = (el) => {
            if (target) return;
            if (!visible(el)) {
              for (const c of el.children) walk(c);
              return;
            }
            const r = roleOf(el);
            // Count the viewport-independent, style-visible structural space.
            if (interactive(el, r) && r === role) {
              seen++;
              if (seen === idx) {
                target = el;
                return;
              }
            }
            for (const c of el.children) walk(c);
          };
          const root = document.body || document.documentElement;
          if (root) walk(root);
          el = target;
        }
        if (!el) return false;
        // A field below the fold can be made reachable without changing its
        // structural identity. Hidden or occluded fields remain a miss.
        if (!bringIntoReach(el)) return false;
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
        // Recorded replay: tag this field as the trigger so a crash/jank box (e.g. a
        // form that throws on submit) can point at it. Only the latest action's tag.
        if (mark) {
          try {
            for (const e of document.querySelectorAll('[data-reproit-trigger]'))
              e.removeAttribute('data-reproit-trigger');
            el.setAttribute('data-reproit-trigger', '1');
          } catch (_) {}
        }
        return true;
      },
      { s: sel, mark: !!(opts && opts.mark) },
    )
    .catch(() => false);
  if (!found) return false;
  // Type via the real keyboard so framework input handlers fire, then commit
  // with Enter. We located + focused the element above; type into the focused
  // field. Clear any existing content first for determinism.
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

// Execute ONE scenario action on a page, emitting the same FUZZ:ACT/MISS/ASSERT
// markers as the single-actor path. `who` is this runner's device label, for
// log attribution. Shared by the multi-actor pull-loop below.
async function execScenarioAction(page, act, who, inputs) {
  log('FUZZ:ACT ' + who + ' ' + act);
  if (act.startsWith('shoot:')) {
    // Screenshot point: capture the current screen and emit the SHOOT marker.
    // No state move, so no observe/stuck change (parity with assert:).
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
    await page.goBack().catch(() => {});
    await page.waitForTimeout(400);
    return;
  }
  if (act.startsWith('type:')) {
    const b = act.slice('type:'.length);
    const eq = b.lastIndexOf('=');
    const sel = eq >= 0 ? b.slice(0, eq) : b;
    const valId = eq >= 0 ? b.slice(eq + 1) : 'normal';
    // PRECEDENCE: a property-matched fixture input for this field wins over the
    // adversarial-class token (same rule as the fuzz-replay path); else the
    // class token / env-expanded literal, unchanged.
    const fixtureVal = inputValueFor(sel, inputs);
    const value =
      fixtureVal != null
        ? fixtureVal
        : ADVERSARIAL_BY_ID[valId] !== undefined
          ? ADVERSARIAL_BY_ID[valId]
          : expandEnv(valId);
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

// Multi-actor: this runner is ONE actor. It opens a single context against the
// shared backend and pulls its next action from the host conductor (the strict
// step-order barrier), so N runners across N processes interleave exactly as the
// journey specifies. Universal: every backend speaks this same two-verb HTTP
// protocol; only execScenarioAction is web-specific.
async function runScenarioActor(browser) {
  const base = process.env.REPROIT_SCENARIO_BARRIER;
  // Property-matched fixture inputs from the fuzz config (empty unless present);
  // a matching `type:` action types the provided value (see inputValueFor).
  const inputs = loadInputs(loadFuzz());
  // Role identity: an explicit label wins (each process gets its own env), else
  // claim a distinct role from the conductor. Claiming is the universal path and
  // the only safe one for shared-build runners, where every device boots the
  // same binary and can't carry a baked-in label; the conductor hands out `a`,
  // `b`, ... atomically so two actors can never collide.
  let who = process.env.REPROIT_DEVICE;
  if (!who) {
    try {
      who = (await (await fetch(base + '/claim')).text()).trim();
    } catch (_) {
      who = '';
    }
    if (!who || who.startsWith('ERR')) who = 'a';
  }
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  page.on('pageerror', (err) => {
    const msg = String(err && err.message ? err.message : err);
    if (
      exceptionIsBenign(msg) ||
      exceptionThrownInTracker(err && err.stack) ||
      exceptionIsNonDeterministic(msg, err && err.stack) ||
      !exceptionIsFirstParty(err && err.stack, APP_ORIGIN)
    )
      return;
    log('EXCEPTION CAUGHT BY WEB PAGE');
    log('actor ' + who + ': ' + msg);
    const stack = err && err.stack ? String(err.stack) : '';
    for (const line of stack.split('\n').slice(0, 8)) log(line);
    log('════════');
  });
  // Renderer/GPU/OOM crash (Playwright `crash`, not `pageerror`): emit the same
  // app-crash block so a process death isn't misattributed to the runner.
  page.on('crash', () => {
    log('EXCEPTION CAUGHT BY WEB PAGE');
    log(
      'actor ' +
        who +
        (': the page crashed (renderer process gone -- GPU / out-of-memory / ' + 'sad-tab)'),
    );
    log('════════');
  });
  await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
  log('JOURNEY claimed role=' + who);
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
    await execScenarioAction(page, act, who, inputs);
    try {
      await fetch(base + '/done?device=' + who, { method: 'POST' });
    } catch (_) {}
  }
  await page.waitForTimeout(500); // flush a trailing pageerror before teardown
  log('JOURNEY DONE');
  log('All tests passed');
  await ctx.close().catch(() => {});
}

// Humanize a raw action string for the review HUD, matching the cloud
// "path to the bug" vocabulary: `tap:<sel>` -> "tap <sel>", `type:<sel>=<val>`
// -> 'type "<val>" -> <sel>', `back` -> "back", initial -> "load".
function humanizeAction(act) {
  if (!act || act === 'load') return 'load';
  if (act === 'back') return '← back';
  if (act.startsWith('tap:')) return 'tap  ' + act.slice(4);
  if (act.startsWith('type:')) {
    const body = act.slice(5);
    const i = body.indexOf('=');
    return i < 0 ? 'type  ' + body : 'type "' + body.slice(i + 1) + '"  →  ' + body.slice(0, i);
  }
  return act;
}

// Draw/update an on-page caption bar naming the action about to be performed,
// with a step counter; the LAST replayed step (the trigger) goes red with an
// x, mirroring the cloud path graph's failure node. Injected per action because
// a navigation drops the previous document's overlay. Best-effort, never throws.
async function showActionHud(page, act, step, total) {
  const text = `step ${step + 1}/${total}   ${humanizeAction(act)}`;
  const isFail = step >= total - 1;
  await page
    .evaluate(
      ({ text, isFail }) => {
        let el = document.getElementById('__reproit_hud');
        if (!el) {
          el = document.createElement('div');
          el.id = '__reproit_hud';
          el.style.cssText = [
            'position:fixed',
            'top:14px',
            'left:50%',
            'transform:translateX(-50%)',
            'z-index:2147483647',
            'font:600 14px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace',
            'padding:10px 16px',
            'border-radius:10px',
            'pointer-events:none',
            'box-shadow:0 6px 24px rgba(0,0,0,.45)',
            'max-width:92vw',
            'white-space:nowrap',
            'overflow:hidden',
            'text-overflow:ellipsis',
          ].join(';');
          (document.body || document.documentElement).appendChild(el);
        }
        el.style.background = isFail ? 'rgba(190,32,32,.96)' : 'rgba(18,20,26,.94)';
        el.style.color = '#fff';
        el.style.border = '1px solid ' + (isFail ? '#ff7a7a' : 'rgba(255,255,255,.14)');
        el.textContent = (isFail ? '✗  ' : '▸  ') + text;
      },
      { text, isFail },
    )
    .catch(() => {});
}

// Draw red bounding box(es) around the element(s) that broke on the CURRENT
// (final) state of a recorded replay, so the clip visibly POINTS at the bug: the
// HUD says what action was taken, the box says what broke. Covers every oracle
// that HAS a place on screen:
//   - content-bug            : re-detected here from the settled DOM (mirrors the
//     oracle predicates, not a divergent detector).
//   - crash / jank / hang    : the element the triggering action targeted, tagged
//     `[data-reproit-trigger]` at click/focus time (`hints.triggerLabel` names it).
//   - flicker                : the persistent-chrome anchors that were rebuilt
//     (`hints.flickerKeys`, resolved back to live nodes by the same key grammar).
// (leak is process-
// level: neither has a box.) Boxes are PAGE-coordinate (scroll-invariant) and
// capped/prioritized so a busy page stays legible; the top offender is scrolled
// into view so it lands in the recorded frame. Replay+record only; best-effort,
// never throws, no effect on the marker stream.
async function drawFindingBoxes(page, hints = {}) {
