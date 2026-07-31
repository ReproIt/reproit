  const snap = await browser.execute(snapshotJs(valueNodeSelectors || []));
  // Hash the canonical Node tree host-side, exactly like the Rust oracle and the
  // golden vectors. Text never contributes.
  snap.sig = signatureOf(snap.anchor, snap.tree);
  // Structural-only signature (no V: section): the per-node key the Layer-1 cap
  // tracks. Computed by hashing the descriptor with the value-class suffix
  // stripped, so it is the exact pre-value-state signature of this structure.
  const full = descriptorOf(snap.anchor, snap.tree);
  const vAt = full.indexOf('\nV:');
  snap.vsection = vAt >= 0 ? full.slice(vAt + 3) : '';
  snap.structuralSig = vAt >= 0 ? fnv1a(full.slice(0, vAt)) : snap.sig;
  // Layer-1 content fingerprint (runner-local, ephemeral): structural sig plus
  // the sorted (stable-key, trimmed raw text) list. An action is EFFECTIVE iff
  // the structural sig OR this fingerprint changed. Never folded into the key.
  snap.content = snap.sig + '|' + snap.textNodes.map((p) => p[0] + '=' + p[1]).join(';');
  return snap;
}

// DOM QUIESCENCE settle before a STRUCTURAL-SIGNATURE capture (mirrors the web
// runner's settleForSignature). Waits for the webview to STOP changing so two
// renders of the same route converge: no DOM mutation for a stable window, then
// running animations settled, then two clean frames. WebDriver has no
// network-idle wait, so that leg is omitted (the mutation-quiet window covers
// late DOM writes). Every wait is hard-capped; best-effort.
async function settleForSignature(browser) {
  try {
    await browser.executeAsync((done) => {
      const twoFrames = () =>
        new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
      (async () => {
        await new Promise((resolve) => {
          let obs = null;
          let quiet = null;
          const finish = () => {
            if (quiet) clearTimeout(quiet);
            if (hard) clearTimeout(hard);
            if (obs) {
              try {
                obs.disconnect();
              } catch (_) {}
            }
            resolve();
          };
          const arm = () => {
            if (quiet) clearTimeout(quiet);
            quiet = setTimeout(finish, 400);
          };
          const hard = setTimeout(finish, 1800);
          try {
            obs = new MutationObserver(arm);
            obs.observe(document.documentElement, {
              subtree: true,
              childList: true,
              attributes: true,
              characterData: true,
            });
          } catch (_) {}
          arm();
        });
        try {
          const running = (document.getAnimations ? document.getAnimations() : []).filter(
            (a) => a.playState === 'running',
          );
          await Promise.race([
            Promise.allSettled(running.map((a) => a.finished)),
            new Promise((r) => setTimeout(r, 800)),
          ]);
        } catch (_) {}
        await twoFrames();
        done();
      })();
    });
  } catch (_) {}
}

// BOT-WALL guard (defensive; a local Tauri app is rarely behind a WAF, but kept
// consistent with the web runner). Detects a challenge interstitial served into
// the webview so the run is reported UNSCANNABLE with zero findings.
async function detectBotWall(browser) {
  try {
    return await browser.execute(() => {
      const title = (document.title || '').toLowerCase();
      const bodyText = (document.body ? document.body.innerText || '' : '').toLowerCase();
      const has = (re) => re.test(title) || re.test(bodyText);
      if (
        document.querySelector(
          '#challenge-running, #cf-challenge-running, #challenge-form, .' +
            'cf-turnstile, [id^="cf-chl"], script[src*="challenge-platform"], ' +
            'iframe[src*="challenges.cloudflare.com"]',
        )
      )
        return { vendor: 'Cloudflare', marker: 'challenge-platform' };
      if (
        has(/just a moment/) ||
        has(/checking your browser before/) ||
        has(/performing (a )?security verification/) ||
        has(/enable javascript and cookies to continue/)
      ) {
        return { vendor: 'Cloudflare', marker: 'interstitial' };
      }
      if (has(/attention required/) && has(/cloudflare/))
        return { vendor: 'Cloudflare', marker: 'attention-required' };
      if (document.querySelector('#px-captcha, .px-block, [class*="perimeterx"]'))
        return { vendor: 'PerimeterX', marker: 'px-captcha' };
      if (/ray id:/.test(bodyText) && bodyText.length < 1200)
        return { vendor: 'Cloudflare', marker: 'ray-id-block' };
      return null;
    });
  } catch (_) {
    return null;
  }
}

// PARITY: keep in sync with runners/web/runner.mjs (operability + flicker oracle)
// ====================================================================
//  OPERABILITY / ACCESSIBILITY GROUND TRUTH (the EXPLORE:GROUNDTRUTH marker)
//  Mirrors runners/web/runner.mjs, but Tauri's webview has NO CDP, so GRAPH 1
//  (operableByPointer) uses native + cursor:pointer + delegation-marker signals
//  only (plus an inline onclick / a document.onclick handler we can read from
//  JS), never a captured event-listener list. GRAPH 2 (a11y dims) runs entirely
//  in-page: inTabOrder is the standard sequential-focus rule (focusable AND
//  tabIndex >= 0 -> a negative tabindex is reachable by script/pointer but NOT
//  by Tab), and keyboardActivatable is derived structurally (native semantics +
//  inline key handlers), never by synthesizing a keypress, which would fire the
//  app's handlers as a side effect. A missing dimension defaults to true (= no gap) in the
//  engine, so we only report what we measured. The whole probe is one execute().
//  Keyed by the SAME selector the EXPLORE:STATE elements use.
// ====================================================================
const GROUNDTRUTH_JS = `
  const ROLES = {
    screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
    icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
    slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
  };
  const roleOf = (el) => {
    const tag = el.tagName.toLowerCase();
    const ariaRole = (el.getAttribute('role') || '').toLowerCase();
    if (ariaRole) {
      if (
        ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox'
      ) return 'textfield';
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
  const interactive = (el, role) => {
    const tag = el.tagName.toLowerCase();
    if (['a', 'button', 'select'].includes(tag)) return true;
    if (tag === 'input' || tag === 'textarea') return true;
    if (role === 'textfield') return true;
    if (
      ['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role)
    ) return true;
    if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
    return false;
  };
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const keyOf = (el) => {
    const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
    if (testid && testid.trim()) return 'testid:' + testid.trim();
    const id = el.getAttribute('id');
    if (id && id.trim()) return 'id:' + id.trim();
    const name = el.getAttribute('name');
    if (name && name.trim()) return 'name:' + name.trim();
    return null;
  };
  const nativeInteractive = (el) => {
    const tag = el.tagName.toLowerCase();
    if (['a', 'button', 'select', 'textarea', 'summary'].includes(tag)) return true;
    if (tag === 'input') {
      const t = (el.getAttribute('type') || 'text').toLowerCase();
      return t !== 'hidden';
    }
    if (el.isContentEditable) return true;
    return false;
  };
  // Roles that name a region or a piece of document structure, NOT an operable
  // widget. A landmark (search/navigation/banner/...) or a structural/live role
  // is something a pointer user reads, not something they "operate", so it must
  // not count as a delegation marker, else it is promoted to operable by a
  // page-wide document click handler and surfaces as a phantom gap.
  const NON_INTERACTIVE_ROLES = new Set([
    'banner', 'complementary', 'contentinfo', 'form', 'main', 'navigation',
    'region', 'search',
    'article', 'definition', 'directory', 'document', 'feed', 'figure', 'group',
    'heading', 'img', 'list', 'listitem', 'math', 'none', 'note', 'presentation',
    'separator', 'table', 'term', 'toolbar', 'tooltip', 'caption', 'rowgroup',
    'row', 'cell', 'columnheader', 'rowheader',
    'dialog', 'alertdialog', 'alert', 'log', 'marquee', 'status', 'timer',
    'application',
  ]);
  const hasDelegationMarker = (el) => {
    const role = (el.getAttribute('role') || '').trim().toLowerCase();
    if (role && !NON_INTERACTIVE_ROLES.has(role)) return true;
    if (el.hasAttribute('tabindex')) return true;
    return false;
  };
  // aria-activedescendant: an item operated via a focusable composite widget (a
  // listbox/menu/tree/grid/combobox whose CONTAINER holds focus and moves a
  // roving "active" item). Such items are keyboard-reachable AND activatable even
  // with tabindex=-1, because the container handles the keys.
  const adManaged = (el) => {
    const isFocusable = (c) => {
      const ti = c.getAttribute('tabindex');
      return (ti !== null && parseInt(ti, 10) >= 0) || nativeInteractive(c);
    };
    if (el.hasAttribute('aria-activedescendant') && isFocusable(el)) return true;
    const c = el.closest('[aria-activedescendant]');
    if (c && c !== el && isFocusable(c)) return true;
    const id = el.getAttribute('id');
    if (id) {
      const q = window.CSS && CSS.escape ? CSS.escape(id) : id;
      const ref = document.querySelector('[aria-activedescendant="' + q + '"]');
      if (ref && isFocusable(ref)) return true;
    }
    return false;
  };
  // reachable: on-screen AND hit-testable, so a real pointer user can operate it.
  // The operable gate below uses this so an off-screen/occluded control is not a
  // phantom pointer-only/keyboard gap.
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
  const rolePresent = (el) => {
    const tag = el.tagName.toLowerCase();
    if (['a', 'button', 'select', 'textarea', 'input', 'summary'].includes(tag)) return true;
    if (/^h[1-6]$/.test(tag)) return true;
    const ar = (el.getAttribute('role') || '').trim().toLowerCase();
    if (!ar) return false;
    return !['none', 'presentation', 'generic'].includes(ar);
  };
  const namePresent = (el) => {
    const aria = el.getAttribute('aria-label'); if (aria && aria.trim()) return true;
    const lb = el.getAttribute('aria-labelledby'); if (lb && lb.trim()) return true;
    const title = el.getAttribute('title'); if (title && title.trim()) return true;
    const alt = el.getAttribute('alt'); if (alt && alt.trim()) return true;
    const ph = el.getAttribute('placeholder'); if (ph && ph.trim()) return true;
    const text = (el.innerText || el.textContent || '').trim();
    return text.length > 0;
  };
  const gestureKindOf = (el, role, native, deleg) => {
    if (role === 'textfield') return 'field';
    if (native) return 'button';
    if (deleg) return 'delegated';
    return 'tap';
  };
  // No CDP: approximate the document-level delegated-click pattern by reading
  // an inline document.onclick / body.onclick handler (the only listener kind
  // visible to script). Real addEventListener handlers are invisible here, so
  // Tauri's delegated detection is best-effort and conservative.
  const docDelegates = !!(document.onclick || (document.body && document.body.onclick));

  const out = [];
  const perRole = {};
  const root = document.body || document.documentElement;
  const walk = (el, isRoot) => {
    if (!isRoot && !visible(el)) { for (const c of el.children) walk(c, false); return; }
    if (!isRoot) {
      const role = roleOf(el);
      const inWalk = interactive(el, role);
      const native = nativeInteractive(el);
      const parentCursor = el.parentElement ? getComputedStyle(el.parentElement).cursor : '';
      const cursor = getComputedStyle(el).cursor === 'pointer' && parentCursor !== 'pointer';
      const deleg = hasDelegationMarker(el);
      const ownInline = !!el.onclick || el.hasAttribute('onclick');
      const candidate = inWalk || native || cursor || deleg || ownInline;
      let sel;
      if (inWalk) {
        const idx = perRole[role] || 0; perRole[role] = idx + 1;
        const key = keyOf(el); sel = key ? 'key:' + key : 'role:' + role + '#' + idx;
      } else if (candidate) {
        const key = keyOf(el); sel = key ? 'key:' + key : 'role:' + role + '#gt' + out.length;
      }
      if (candidate) {
        // operable is graph 1: an element a pointer can ACTUALLY operate now. An
        // off-screen/occluded control is not pointer-operable, so it cannot be a
        // pointer-only/keyboard gap either; gate on reachability to align the two
        // graphs (matches the web runner).
        const operable = reachable(el) && (
          native || cursor || ownInline || (docDelegates && deleg)
        );
        // inTabOrder: sequential-focus reachability. An element is in the Tab
        // sequence iff it is focusable AND its tabIndex is >= 0. A tabindex=-1
        // element is script/pointer focusable but NOT reachable by Tab (the
        // motivating <div role=option tabindex=-1> case). An aria-activedescendant
        // item is reachable + activatable via its focusable composite container.
        const adm = adManaged(el);
        const focusable = native || el.tabIndex >= 0 ||
          (el.hasAttribute('tabindex') && el.tabIndex >= 0) || adm;
        const inTabOrder = (el.tabIndex >= 0 && focusable) || adm;
        const a11y = {
          rolePresent: rolePresent(el),
          namePresent: namePresent(el),
          inTabOrder: inTabOrder,
          focusable: focusable,
        };
        if (operable) {
          if (!inTabOrder && !native) {
            a11y.keyboardActivatable = false;
          } else {
            // keyboardActivatable, derived WITHOUT firing the control. We must
            // NOT synthesize Enter/Space (even via dispatchEvent): a bubbling
            // keydown fires the app's real handler (a navigation, or a crash) as
            // a side effect, polluting the crash oracle. A Tauri webview has no
            // CDP, so we cannot enumerate addEventListener key handlers; the most
            // we can read cheaply is the native semantics and inline on* handlers.
            // A native control, or one with an inline key handler, is keyboard-
            // activatable. Otherwise, since the element is focusable and in the
            // Tab order, we assume activatable rather than flag a gap we cannot
            // prove (matches the web runner's no-CDP fallback; it means Tauri
            // under-reports the click-only-div case the CDP path catches).
            const inlineKey = !!(el.onkeydown || el.onkeypress || el.onkeyup);
            a11y.keyboardActivatable = native || inlineKey || focusable;
          }
        }
        out.push({
          id: sel,
          operable: operable,
          gestureKind: gestureKindOf(el, role, native, deleg),
          a11y,
        });
      }
    }
    for (const c of el.children) walk(c, false);
  };
  if (root) walk(root, true);
  // Focus trap detection needs a real Tab traversal, which the webview can't do
  // from script; report false (a missing/false focusTrap is the safe default).
  return { elements: out, focusTrap: false };
`;

// Emit the EXPLORE:GROUNDTRUTH record for the current state (Tauri). `sig` is the
// SAME signature the EXPLORE:STATE for this state carried. Best-effort: a failed
// probe simply emits nothing.
async function emitGroundtruth(browser, sig) {
  let res;
  try {
    res = await browser.execute(GROUNDTRUTH_JS);
  } catch (e) {
    return;
  }
  if (!res) return;
  log(
    'EXPLORE:GROUNDTRUTH ' +
      JSON.stringify({ sig, focusTrap: !!res.focusTrap, elements: res.elements || [] }),
  );
}

// The Tier-1 flicker oracle (persistent-anchor churn) is NOT evaluated on this
// tier. Its two execute() source strings lived here, complete and never called,
// so a reader saw an oracle the runner never ran and a Tauri report showed no
// flicker finding for the same reason a clean app does. They were removed on
// 2026-07-31; the gap is declared in validation/oracles/coverage.json, and
// wiring it needs a before-action/after-settle mark pair on the WebDriver path
// plus a Tauri host to prove it fires.

// PARITY: keep in sync with runners/web/runner.mjs (overflow oracle).
//
// CONTENT-BUG oracle (deterministic, DOM/label-based). The literal artifacts a
// stringify/template bug leaks to the screen: [object Object], whole-word
// undefined/null/NaN, an unrendered {{...}}/${...} placeholder. Scans only the
// OWN text of keyed, visible elements so the finding is addressed by a stable,
// locale-invariant key (never the text). Pure substring/structure test, no pixel
// or timing read, so the same DOM yields the same finding on every run/replay.
// Identical to the web runner; runs in-webview via browser.execute.
const DETECT_CONTENTBUG_JS = `
  // Fuzzer provenance (mirrors the web tier): a reflected fuzzer probe is not the
  // app's own broken content. arguments[0] is the injected-values array passed by
  // browser.execute(DETECT_CONTENTBUG_JS, [...INJECTED_VALUES]).
  const injected = (Array.isArray(arguments[0]) ? arguments[0] : [])
    .map((v) => String(v == null ? '' : v).toLowerCase())
    .filter((v) => v.length > 0);
  const fromFuzzInjection = (text) => {
    const n = String(text || '').toLowerCase();
    if (!n) return false;
    if (injected.some(
      (v) => n.indexOf(v) !== -1 || (v.length >= 3 && v.indexOf(n) !== -1),
    )) return true;
    // Fragmented reflection: the browser parsed markup out of the probe, so the
    // visible text is a fragment; check the specific artifact tokens for provenance.
    const arts = [];
    const tm = n.match(/\\{\\{[^}]*\\}\\}/g); if (tm) arts.push(...tm);
    const dm = n.match(/\\$\\{[^}]*\\}/g); if (dm) arts.push(...dm);
    if (n.indexOf('[object object]') !== -1) arts.push('[object object]');
    return arts.some((a) => injected.some((v) => v.indexOf(a) !== -1));
  };
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const CODE_TAGS = new Set(['code', 'pre', 'script', 'style', 'textarea']);
  const inCodeContext = (el) => {
    if (el.isContentEditable) return true;
    for (let n = el; n && n !== document.body; n = n.parentElement) {
      if (CODE_TAGS.has(n.tagName.toLowerCase())) return true;
    }
    return false;
  };
  const keyOf = (el) => {
    const tid = (el.getAttribute('data-testid') || el.getAttribute('data-test-id') || '').trim();
    if (tid) return 'testid:' + tid;
    const id = (el.getAttribute('id') || '').trim();
    if (id) return 'id:' + id;
    const name = (el.getAttribute('name') || '').trim();
    if (name) return 'name:' + name;
    return null;
  };
  const ownText = (el) => {
    let t = '';
    for (const c of el.childNodes) if (c.nodeType === 3) t += c.textContent;
    return t.replace(/\\s+/g, ' ').trim();
  };
  // Prose guard for BOTH artifact kinds: fire only when the artifact IS the label,
  // never when docs prose merely mentions "[object Object]" or the "{{ }}" syntax.
  const dominates = (s) => s.length <= 24 && !/[.!?]/.test(s);
  const reasonOf = (text) => {
    if (!text) return null;
    if (text.includes('[object Object]')) {
      const s = text.replace(/\\[object Object\\]/g, ' ').replace(/\\s+/g, ' ').trim();
      if (dominates(s)) return 'object-object';
    }
    if (/\\{\\{[^}]*\\}\\}/.test(text) || /\\$\\{[^}]*\\}/.test(text)) {
      const s = text
        .replace(/\\{\\{[^}]*\\}\\}/g, ' ')
        .replace(/\\$\\{[^}]*\\}/g, ' ')
        .replace(/\\s+/g, ' ')
        .trim();
      if (dominates(s)) return 'unrendered-template';
    }
    return null;
  };
  const out = [];
  const seen = new Set();
  const all = document.body ? document.body.querySelectorAll('*') : [];
  for (const el of all) {
    if (!visible(el)) continue;
    if (inCodeContext(el)) continue;
    const key = keyOf(el);
    if (!key) continue;
    const text = ownText(el);
    const reason = reasonOf(text);
    if (!reason) continue;
    if (fromFuzzInjection(text)) continue;
    const dedup = key + '|' + reason;
    if (seen.has(dedup)) continue;
    seen.add(dedup);
    out.push({ key, reason, text: text.slice(0, 80) });
  }
  out.sort((a, b) => (
    a.key < b.key ? -1 : a.key > b.key ? 1 :
      (a.reason < b.reason ? -1 : a.reason > b.reason ? 1 : 0)
  ));
  return out;
`;

// PARITY: keep in sync with runners/web/runner.mjs (jank/hang watchdog).
//
// JANK / HANG watchdog (deterministic, recorded-trace based). Two paths, both
// installed inside the webview via execute() (idempotent) and re-installed each
// observe() since a navigation replaces the window:
//
//   1. Long Tasks (CHROMIUM / WebView2 only). We key off the webview's own Long
//      Tasks trace, never a wall-clock duration sample: a `longtask`
//      PerformanceObserver entry is emitted for any task that blocks the main
//      thread > 50ms, buffered and delivered after the blocking task finishes.
//      We classify by the MAX blocked duration into coarse, well-separated
//      floors (>=2000ms hang, >=200ms jank) so timing jitter can never flip the
//      verdict. The Long Tasks API exists ONLY in Chromium/WebView2; on Tauri's
//      WebKit webview (WKWebView on macOS, WebKitGTK on Linux) it is ABSENT, so
//      this path records nothing there.
//
//   2. requestAnimationFrame frame-drop detector (CROSS-ENGINE). rAF fires once
//      per would-be paint in EVERY engine, so the interval between two callbacks
//      is how long the main thread blocked between two frames. This is the path
//      that closes the silence on Tauri's WebKit webview, where Long Tasks is
//      unavailable. The classifier (classifyFrameIntervals) and its floors are
//      COPIED VERBATIM from runners/web/runner.mjs, where they are FP-validated
//      on real firefox/webkit (clean + animated sites stay silent). Emits the
//      SAME EXPLORE:JANK / EXPLORE:HANG markers with the SAME reused
//      JANK_FLOOR_MS / HANG_FLOOR_MS buckets, so the marker is byte-identical
//      across paths and to the web runner.
//
// drainJankForEngine() runs the Long Tasks path when it produced entries (the
// precise Chromium/WebView2 signal) and otherwise falls back to the rAF path,
// so a WebView2 verdict is unchanged while WebKit gets the cross-engine signal.
// A webview where NEITHER path sees a stall stays SILENT, NEVER a false positive
// (same honesty as the web runner's firefox/webkit fallback).
const JANK_FLOOR_MS = 200;
const HANG_FLOOR_MS = 2000;
const INSTALL_LONGTASK_JS = `
  try {
    if (!window.__reproitLongTaskHooked) {
      window.__reproitLongTaskHooked = true;
      window.__reproitLongTasks = [];
      const obs = new PerformanceObserver((list) => {
        for (const e of list.getEntries()) window.__reproitLongTasks.push(Math.round(e.duration));
      });
      obs.observe({ entryTypes: ['longtask'] });
    }
  } catch (_) { /* no Long Tasks API: jank/hang silent on this webview */ }
  return true;
`;
const RESET_LONGTASK_JS = `try { window.__reproitLongTasks = []; } catch (_) {} return true;`;
const DRAIN_LONGTASK_JS = `
  const t = window.__reproitLongTasks || [];
  window.__reproitLongTasks = [];
  return t;
`;
async function installLongTaskObserver(browser) {
