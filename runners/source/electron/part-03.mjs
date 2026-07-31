  await page
    .addInitScript(() => {
      try {
        window.__reproitLongTasks = [];
        const obs = new PerformanceObserver((list) => {
          for (const e of list.getEntries()) window.__reproitLongTasks.push(Math.round(e.duration));
        });
        obs.observe({ entryTypes: ['longtask'] });
      } catch (_) {
        /* no Long Tasks API: jank/hang silent on this engine */
      }
    })
    .catch(() => {});
}
async function drainJank(page) {
  const tasks = await page
    .evaluate(() => {
      const t = window.__reproitLongTasks || [];
      window.__reproitLongTasks = [];
      return t;
    })
    .catch(() => []);
  if (!tasks || !tasks.length) return null;
  const max = Math.max(...tasks);
  if (max >= HANG_FLOOR_MS) {
    return { kind: 'hang', bucket: HANG_FLOOR_MS, count: tasks.length };
  }
  if (max >= JANK_FLOOR_MS) {
    return { kind: 'jank', bucket: JANK_FLOOR_MS, count: tasks.length };
  }
  return null;
}

// PARITY: keep in sync with runners/web/runner.mjs (leak heap sampler).
//
// LEAK sampler (deterministic, v8 heap). The CDP `Runtime.getHeapUsage` reports
// the REAL, unrounded v8 used-heap size (performance.memory is quantized and
// useless for a multi-MB leak), so we use that when a CDP session is available
// and force a GC first (`HeapProfiler.collectGarbage`) so the reading is the
// RETAINED (live) heap. We emit a MEMORY:SAMPLE marker per cycle; the soak side
// reconstructs the series. Electron's renderer is Chromium with full CDP, so
// this is the precise (non-fallback) path, byte-identical to the web runner.
async function sampleHeap(page, cdp, tMs) {
  let used = null;
  if (cdp) {
    try {
      await cdp.send('HeapProfiler.collectGarbage').catch(() => {});
      const r = await cdp.send('Runtime.getHeapUsage');
      if (r && typeof r.usedSize === 'number') used = Math.round(r.usedSize);
    } catch (_) {
      used = null;
    }
  }
  if (used == null) {
    try {
      used = await page.evaluate(() => {
        if (performance.memory && typeof performance.memory.usedJSHeapSize === 'number') {
          return performance.memory.usedJSHeapSize;
        }
        return null;
      });
    } catch (_) {
      used = null;
    }
  }
  if (used == null) return;
  log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: used }));
}

// PARITY: keep in sync with runners/web/runner.mjs (Tier-2 pixel-flicker oracle).
//
// Tier-2 flicker oracle (gated, Chromium/CDP only). Records the frames the
// renderer presented during a transition (CDP Page.startScreencast) and scores
// the sequence for a transient divergence: a middle frame that diverges from the
// settled FINAL frame far more than the endpoints (flicker-oracle.mjs
// transientDivergence). OFF by default; only emits when REPROIT_FLICKER_PIXELS=1,
// same gate as the web runner. The pngjs decoder + the host-pure probe/flicker
// helpers are imported lazily inside main() so this module stays import-safe for
// the parity test; if any of them is unavailable the oracle stays silent.
const FLICKER_PIXELS = process.env.REPROIT_FLICKER_PIXELS === '1';
// Probe mode (REPROIT_PROBE=1): the web tier's destructive probe pass. This
// runner has no probe of its own, but the flag still gates the viewport-
// swapping zoom-reflow check below, matching the web runner's guard.
const PROBE = process.env.REPROIT_PROBE === '1';
// Filled in by main() via dynamic import when FLICKER_PIXELS is on. Null until
// then (and on any import failure), which keeps startScreencastCapture a no-op.
let PIXEL = null;
function pngToRgba(buf) {
  const png = PIXEL.PNG.sync.read(buf);
  return { data: png.data, width: png.width, height: png.height };
}
async function startScreencastCapture(cdp) {
  if (!FLICKER_PIXELS || !PIXEL || !cdp) return null;
  const frames = [];
  const onFrame = (ev) => {
    frames.push(Buffer.from(ev.data, 'base64'));
    cdp.send('Page.screencastFrameAck', { sessionId: ev.sessionId }).catch(() => {});
  };
  try {
    await cdp.send('Page.enable');
    cdp.on('Page.screencastFrame', onFrame);
    await cdp.send('Page.startScreencast', {
      format: 'png',
      everyNthFrame: 1,
      maxWidth: 320,
      maxHeight: 240,
    });
  } catch (_) {
    try {
      cdp.off('Page.screencastFrame', onFrame);
    } catch (_) {}
    return null;
  }
  return {
    async stop() {
      try {
        await cdp.send('Page.stopScreencast');
      } catch (_) {}
      try {
        cdp.off('Page.screencastFrame', onFrame);
      } catch (_) {}
      return frames;
    },
  };
}
async function finishScreencastCapture(cap, from, action) {
  if (!cap) return;
  let frames;
  try {
    frames = await cap.stop();
  } catch (_) {
    return;
  }
  if (!frames || frames.length < 3) return;
  let rgbas;
  try {
    rgbas = frames.map(pngToRgba);
  } catch (_) {
    return;
  }
  const final = rgbas[rgbas.length - 1];
  const diffs = [];
  for (const f of rgbas) {
    if (
      f.width !== final.width ||
      f.height !== final.height ||
      f.data.length !== final.data.length
    ) {
      continue;
    }
    diffs.push(PIXEL.changedFraction(f.data, final.data));
  }
  const fl = PIXEL.transientDivergence(diffs);
  if (fl) {
    log('EXPLORE:FLICKER ' + JSON.stringify({ from, action, peak: fl.peak, frames: fl.frames }));
  }
}

// ====================================================================
//  OPERABILITY / ACCESSIBILITY GROUND TRUTH (the EXPLORE:GROUNDTRUTH marker)
//  Two graphs over the SAME tappable walk snapshot() produced:
//    GRAPH 1 (operableByPointer): is this element actually operable by a
//      pointer? native interactive OR cursor:pointer OR a real click/pointer
//      event listener (CDP) OR a DELEGATED target (document/body has a click/
//      pointerdown listener AND the element carries a role/[data-*]/tabindex
//      marker -> e.g. <div role=option tabindex=-1> driven by a doc listener).
//    GRAPH 2 (a11y/keyboard dims): real Tab traversal records which elements
//      land in document.activeElement (inTabOrder); operable elements are
//      probed for keyboardActivatable (focus + Enter/Space changes content);
//      rolePresent = a non-generic ARIA/native role; namePresent = an
//      accessible name. A focus trap is when Tab cycles within a subset that
//      never returns to body.
//  The diff (operable yet not keyboard-reachable / pointer-only / no-role) is
//  what the Rust oracle flags as a gap. We emit only dimensions we actually
//  determined; a MISSING a11y field defaults to true (= no gap) in the engine,
//  so we never assert a healthy dimension we didn't measure.
//  Keyed by the SAME selector (`sel`) the EXPLORE:STATE elements use, so the
//  oracle joins ground truth to the state's elements with no translation.
// ====================================================================

// Walk the live DOM with the exact roleOf/interactive/visible logic snapshot()
// uses, in the SAME document order, and tag every tappable with a stable index
// attribute (data-reproit-gt="<i>"). Returns per-element static facts: its
// selector (identical to snapshot()'s), whether it is natively interactive,
// whether it has cursor:pointer, whether it carries a delegation marker (role /
// data-* / tabindex), and the rolePresent / namePresent a11y dims. The
// listener-based operability (own click listener, delegated via document) is
// filled in host-side from CDP, keyed by the tag index.
async function gtCollect(page) {
  return page.evaluate(() => {
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
    const interactive = (el, role) => {
      const tag = el.tagName.toLowerCase();
      if (['a', 'button', 'select'].includes(tag)) return true;
      if (tag === 'input' || tag === 'textarea') return true;
      if (role === 'textfield') return true;
      if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role))
        return true;
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
    // Native interactive: an element a pointer can drive WITHOUT a listener or
    // cursor hint, by the platform's own semantics.
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
    // Delegation marker: an element that is not natively interactive but carries
    // an authoring signal it is MEANT to be operated, namely an ARIA role or a
    // tabindex. Combined host-side with a document/body click listener, this is
    // the <div role=option tabindex=-1> delegated-click pattern. We deliberately
    // do NOT treat a bare data-* attribute as a marker: data-* is used widely for
    // non-interactive bookkeeping, so it floods the graph with false delegated
    // targets; role/tabindex are the precise "this is interactive" signals.
    // Roles that name a region or a piece of document structure, NOT an operable
    // widget. A landmark (search/navigation/banner/...) or a structural/live role
    // is something a pointer user reads, not something they "operate", so it must
    // not count as a delegation marker. Without this, any element bearing such a
    // role gets promoted to operable by the page-wide document click listener
    // (docDelegates) and surfaces as a phantom pointer-only/keyboard gap.
    const NON_INTERACTIVE_ROLES = new Set([
      // landmarks
      'banner',
      'complementary',
      'contentinfo',
      'form',
      'main',
      'navigation',
      'region',
      'search',
      // document structure
      'article',
      'definition',
      'directory',
      'document',
      'feed',
      'figure',
      'group',
      'heading',
      'img',
      'list',
      'listitem',
      'math',
      'none',
      'note',
      'presentation',
      'separator',
      'table',
      'term',
      'toolbar',
      'tooltip',
      'caption',
      'rowgroup',
      'row',
      'cell',
      'columnheader',
      'rowheader',
      // containers + live regions / status
      'dialog',
      'alertdialog',
      'alert',
      'log',
      'marquee',
      'status',
      'timer',
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
    // roving "active" item with arrow keys). Such items are keyboard-reachable
    // AND activatable even with tabindex=-1, because the container handles the
    // keys. This is the standard roving/activedescendant ARIA pattern; a naive
    // per-element tabindex check misreads its options as keyboard-unreachable.
    const adManaged = (el) => {
      const isFocusable = (c) => {
        const ti = c.getAttribute('tabindex');
        return (ti !== null && parseInt(ti, 10) >= 0) || nativeInteractive(c);
      };
      // The composite widget itself: a focusable element that OWNS
      // aria-activedescendant (listbox/combobox/grid/tree/menu) processes
      // arrow/Enter keys per the ARIA contract, so it is keyboard-operable even
      // when the key handler lives on an ancestor or document rather than on the
      // element's own node. A precise spec signal, not a guess at delegation.
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
    // rolePresent: a non-generic role. A native interactive tag (a/button/input/
    // select/textarea) inherently has a role; otherwise an explicit ARIA role
    // that is not the generic "none"/"presentation"/"generic".
    const rolePresent = (el) => {
      const tag = el.tagName.toLowerCase();
      if (['a', 'button', 'select', 'textarea', 'input', 'summary'].includes(tag)) return true;
      if (/^h[1-6]$/.test(tag)) return true;
      const ar = (el.getAttribute('role') || '').trim().toLowerCase();
      if (!ar) return false;
      return !['none', 'presentation', 'generic'].includes(ar);
    };
    const namePresent = (el) => {
      const aria = el.getAttribute('aria-label');
      if (aria && aria.trim()) return true;
      const labelledby = el.getAttribute('aria-labelledby');
      if (labelledby && labelledby.trim()) return true;
      const title = el.getAttribute('title');
      if (title && title.trim()) return true;
      const alt = el.getAttribute('alt');
      if (alt && alt.trim()) return true;
      const ph = el.getAttribute('placeholder');
      if (ph && ph.trim()) return true;
      const text = (el.innerText || el.textContent || '').trim();
      return text.length > 0;
    };
    const gestureKindOf = (el, role, native, deleg) => {
      const tag = el.tagName.toLowerCase();
      if (role === 'textfield') return 'field';
      if (native) return 'button';
      if (deleg) return 'delegated';
      return 'tap';
    };
    // reachable: on-screen AND hit-testable, so a real pointer user can operate
    // it. The operable gate below uses this so an off-screen/occluded control is
    // not a phantom pointer-only/keyboard gap.
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

    // Clear any stale tags from a prior state, then re-tag in document order.
    for (const e of document.querySelectorAll('[data-reproit-gt]'))
      e.removeAttribute('data-reproit-gt');
    const out = [];
    // perRole counts ONLY tappable-walk elements, so role:<role>#<idx> selectors
    // match snapshot()/EXPLORE:STATE byte-for-byte. The ground truth also covers
    // a BROADER set: elements that are operable by pointer yet the tappable
    // grammar drops them (the <div role=option tabindex=-1> delegated case is
    // the motivating one). Such broader-only elements are keyed by their stable
    // id when they have one (key:id:...), so they still join to a real element.
    const perRole = {};
    const root = document.body || document.documentElement;
    const walk = (el, isRoot) => {
      if (!isRoot && !visible(el)) {
        for (const c of el.children) walk(c, false);
        return;
      }
      if (!isRoot) {
        const role = roleOf(el);
        const inTappableWalk = interactive(el, role);
        const native = nativeInteractive(el);
        // cursor:pointer is INHERITED, so a clickable parent paints every
        // descendant with it. Only count it as an OWN operability signal when
        // this element introduces it (its parent is not already pointer), which
        // avoids flagging the dozens of nested wrappers under one clickable card.
        const parentCursor = el.parentElement ? getComputedStyle(el.parentElement).cursor : '';
        const cursor = getComputedStyle(el).cursor === 'pointer' && parentCursor !== 'pointer';
        const deleg = hasDelegationMarker(el);
        // A ground-truth candidate is anything the tappable walk takes OR any
        // element that is plausibly operable by pointer (native / cursor hint /
        // a delegation marker), so pointer-only controls outside the keyboard-
        // reachable grammar are still measured.
        const candidate = inTappableWalk || native || cursor || deleg;
        // Keep the per-role index in lockstep with snapshot() by only advancing
        // it for tappable-walk elements.
        let sel;
        if (inTappableWalk) {
          const idx = perRole[role] || 0;
          perRole[role] = idx + 1;
          const key = keyOf(el);
          sel = key ? 'key:' + key : 'role:' + role + '#' + idx;
        } else if (candidate) {
          const key = keyOf(el);
          // No tappable-walk index to borrow; prefer a stable key. Lacking one,
          // fall back to a role+document-position key that is at least unique.
          sel = key ? 'key:' + key : 'role:' + role + '#gt' + out.length;
        }
        if (candidate) {
          const i = out.length;
          el.setAttribute('data-reproit-gt', String(i));
          out.push({
            sel,
            role,
            native,
            cursor,
            deleg,
            reachable: reachable(el),
            rolePresent: rolePresent(el),
            namePresent: namePresent(el),
            adManaged: adManaged(el),
            gestureKind: gestureKindOf(el, role, native, deleg),
          });
        }
      }
      for (const c of el.children) walk(c, false);
    };
    if (root) walk(root, true);
    return out;
  });
}

// Are there click/pointerdown listeners on the document or body? Those make any
// element with a delegation marker operable by pointer (the delegated pattern).
// CDP-only (web + Electron). Returns true if such a listener exists.
async function gtDocDelegates(cdp) {
  const targets = ['document', 'document.body'];
  for (const expr of targets) {
    try {
      const { result } = await cdp.send('Runtime.evaluate', { expression: expr });
      if (!result || !result.objectId) continue;
      const { listeners } = await cdp.send('DOMDebugger.getEventListeners', {
        objectId: result.objectId,
      });
      if (
        (listeners || []).some(
          (l) => l.type === 'click' || l.type === 'pointerdown' || l.type === 'mousedown',
        )
      )
        return true;
    } catch (e) {
      /* CDP best-effort */
    }
  }
  return false;
}

// Does this tagged element have its OWN real click/pointer listener? CDP-only.
// `pointer` = a real click/pointer handler (graph-1 operability); `key` = a real
// keydown/keypress/keyup handler. The key signal catches "focusable but
// keyboard-dead" controls (a click-only div) WITHOUT pressing a key.
async function gtElementListeners(cdp, i) {
  try {
    const { result } = await cdp.send('Runtime.evaluate', {
      expression: 'document.querySelector(\'[data-reproit-gt="' + i + '"]\')',
    });
    if (!result || !result.objectId) return { pointer: false, key: false };
    const { listeners } = await cdp.send('DOMDebugger.getEventListeners', {
      objectId: result.objectId,
    });
    const ls = listeners || [];
    return {
      pointer: ls.some(
        (l) => l.type === 'click' || l.type === 'pointerdown' || l.type === 'mousedown',
      ),
      key: ls.some((l) => l.type === 'keydown' || l.type === 'keypress' || l.type === 'keyup'),
    };
  } catch (e) {
    return { pointer: false, key: false };
  }
}

// GRAPH 2 part A: a real Tab traversal from document.body. Press Tab up to
// `steps` times, recording the tagged index of document.activeElement each time
// (untagged focus stops record -1). An element's inTabOrder = its index appeared.
// Focus trap: Tab cycled through a set of elements that never returned focus to
// body (the active element kept changing among a bounded subset and body was
// never reached again after leaving it). Returns { inTab:Set<int>, focusTrap }.
async function gtTabOrder(page, count, steps) {
  // Start from a clean baseline: blur whatever is focused onto body.
  await page.evaluate(() => {
    try {
      if (document.activeElement) document.activeElement.blur();
      document.body.focus();
    } catch (e) {}
  });
  const inTab = new Set();
  const visited = [];
  for (let k = 0; k < steps; k++) {
    await page.keyboard.press('Tab');
    const idx = await page.evaluate(() => {
      const ae = document.activeElement;
      if (!ae || ae === document.body || ae === document.documentElement) return -2; // body/none
      const t = ae.getAttribute && ae.getAttribute('data-reproit-gt');
      return t == null ? -1 : parseInt(t, 10);
    });
    visited.push(idx);
    if (idx >= 0) inTab.add(idx);
  }
  // Focus trap: after focus first left body it never came back (no -2 after the
  // first real focus), yet focus kept moving. A page that lets you Tab back out
  // to the body/address bar is not trapped.
  let firstReal = visited.findIndex((v) => v >= 0 || v === -1);
  let returnedToBody = false;
  if (firstReal >= 0) {
    for (let k = firstReal + 1; k < visited.length; k++)
      if (visited[k] === -2) {
        returnedToBody = true;
        break;
      }
  }
  const focusTrap = firstReal >= 0 && !returnedToBody && inTab.size > 0 && inTab.size < count;
  return { inTab, focusTrap };
}

// Build and emit the EXPLORE:GROUNDTRUTH record for the current state. `sig` is
// the SAME signature the EXPLORE:STATE for this state carried. `cdp` may be null
// (no listener-based operability then). Best-effort throughout: any probe that
// fails is simply omitted, so we never emit a dimension we did not measure.
async function emitGroundtruth(page, cdp, sig) {
  let els;
  try {
    els = await gtCollect(page);
  } catch (e) {
    return;
  }
  if (!els || !els.length) {
    log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap: false, elements: [] }));
    return;
  }
  // GRAPH 1: listener-based operability via CDP (web + Electron).
  let docDelegates = false;
  const ownListener = new Array(els.length).fill(false);
  const keyListener = new Array(els.length).fill(false);
  let cdpListeners = false;
  if (cdp) {
    cdpListeners = true;
    docDelegates = await gtDocDelegates(cdp);
    for (let i = 0; i < els.length; i++) {
      const { pointer, key } = await gtElementListeners(cdp, i);
      ownListener[i] = pointer;
      keyListener[i] = key;
    }
  }
  // GRAPH 2 part A: Tab traversal.
  let inTab = new Set(),
    focusTrap = false;
  try {
    ({ inTab, focusTrap } = await gtTabOrder(page, els.length, 60));
  } catch (e) {}

  const records = [];
  for (let i = 0; i < els.length; i++) {
    const e = els[i];
    // operable is graph 1: what a pointer user can ACTUALLY operate in this
    // rendered state. An element a pointer cannot reach (off-screen, off-viewport,
    // occluded, or display:none) is not pointer-operable, so it cannot be a
    // pointer-only/keyboard gap either. The keyboard graph (the Tab walk) already
    // requires reachability, so without this guard an unreachable pointer control
    // (e.g. an off-screen skip-link, or a button below the fold) could never be in
    // graph 2 and was reported as a phantom gap. Gating here aligns the two graphs.
    const operable =
      e.reachable !== false &&
      (e.native || e.cursor || ownListener[i] || (docDelegates && e.deleg));
    const a11y = {};
    // rolePresent / namePresent are always determined (pure DOM).
    a11y.rolePresent = e.rolePresent;
    a11y.namePresent = e.namePresent;
    // inTabOrder: the Tab walk is authoritative for whether it can be reached.
    // An aria-activedescendant-managed item is reachable via its focusable
    // container (the container is in the Tab walk; arrows move the active item),
    // so it counts even though its own tabindex is -1.
    a11y.inTabOrder = inTab.has(i) || e.adManaged;
    a11y.focusable = inTab.has(i) || e.native || e.adManaged;
    // keyboardActivatable, derived WITHOUT firing the control (pressing Enter/
    // Space would trigger the app's real handler as a side effect). A native
    // control or one with a real key listener is keyboard-activatable; a
    // focusable, operable element that is NEITHER native NOR has a key listener
    // (a click-only div) is keyboard-DEAD -> a WCAG 2.1.1 gap. Without CDP we
    // can't see key handlers, so fall back to focusable && reachable.
    if (operable) {
      const focusableOnscreen = a11y.focusable && e.reachable !== false;
      // adManaged items are activated through the composite widget's container
      // (it owns the Enter/Space handler and moves the active descendant), so
      // their own per-element key listener is irrelevant.
      a11y.keyboardActivatable = e.adManaged
        ? focusableOnscreen
        : cdpListeners
          ? focusableOnscreen && (e.native || keyListener[i])
          : focusableOnscreen;
    }
    records.push({ id: e.sel, operable, gestureKind: e.gestureKind, a11y });
  }
  // Clean up the tagging so it never leaks into a later snapshot/signature.
  try {
    await page.evaluate(() => {
      for (const el of document.querySelectorAll('[data-reproit-gt]'))
        el.removeAttribute('data-reproit-gt');
    });
  } catch (e) {}

  log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap, elements: records }));
}

// STRUCTURAL tap: resolve a locale-invariant selector and click it. Returns
// true on success. Byte-identical to runners/web/runner.mjs's tap(): the SAME
// shared resolver decides identity and the SAME real pointer activation drives
// it, because the Electron renderer is Chromium behind the same Playwright API.
// No visible text is ever used to locate the element.
//   key:testid:<v> -> [data-testid="v"] (or data-test-id)
//   key:id:<v>     -> #<v>
//   key:name:<v>   -> [name="v"]
//   role:<role>#<idx> -> the idx-th visible tappable of that role, document order
async function tap(page, sel) {
  const handle = await page.evaluateHandle(resolveStructuralTarget, sel).catch(() => null);
  const target = handle ? handle.asElement() : null;
  if (!target) {
    if (handle) await handle.dispose().catch(() => {});
    return false;
  }
  const point = await page
    .evaluate((el) => {
      // Stash the clicked element for the post-tap oracle probes (the
      // duplicate-submit eligibility check and the focus-loss guards read it
      // in-page). A window ref only, never a DOM mutation, so the signature/
      // content/mutation oracles are untouched.
      try {
        window.__reproitLastTap = el;
        // Record whether the browser's own pointer activation focused the target,
        // observed on the real click rather than manufactured. The runner used to
        // call el.focus() here: focusLossCheck ignores that flag, so it bought
        // nothing, while it parked focus on the tapped control and thereby
        // manufactured the `pre === tapped` precondition for the NEXT action --
        // exactly the false positive the guard was written to kill.
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
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) return null;
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    }, target)
    .catch(() => null);
  await target.dispose().catch(() => {});
  if (!point) return false;
  try {
    // A REAL pointer activation through the driver, never el.click(). el.click()
    // dispatches an untrusted event straight at the node, so it skips hit-testing
    // and "succeeds" on a control an overlay completely covers; the runner then
    // reports an action a user could not have performed, and every oracle after
    // the tap judges a state no user can reach.
    await page.mouse.click(point.x, point.y, { delay: 10 });
    return true;
  } catch (_) {
    return false;
  }
}
