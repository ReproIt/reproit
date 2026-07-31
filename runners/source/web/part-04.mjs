  try {
    await page.waitForLoadState('networkidle', { timeout: 2500 });
  } catch (_) {}
  try {
    await page.evaluate(async () => {
      const twoFrames = () =>
        new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
      // No DOM mutation for a 400ms stable window; hard cap 1.8s. The early-exit
      // (a quiet page resolves at 400ms) keeps well-behaved pages fast; the cap
      // bounds the cost on a page that keeps mutating (polling/analytics).
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
      // Running transitions / animations settled; hard cap 800ms (an infinite
      // animation never resolves its `finished`, so the race releases it).
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
    });
  } catch (_) {}
}

// BOT-WALL guard: when a WAF challenge interstitial (Cloudflare "Just a
// moment..." / "Checking your browser" / Turnstile / cf-challenge, PerimeterX, or
// a generic "verify you are human" wall) is served INSTEAD of the app, reproit
// never reached the app and every oracle would fire on the interstitial. Detect
// it so the scan is reported UNSCANNABLE with ZERO findings. The signature set is
// kept tight (specific title text + DOM challenge markers) so a real app page that
// merely mentions "security" or has a login CAPTCHA does not trip it. Returns
// { vendor, marker } when blocked, else null.
async function detectBotWall(page) {
  try {
    return await page.evaluate(() => {
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
      if (
        has(/verify you are (a )?human/) &&
        document.querySelector('iframe[src*="captcha"], .g-recaptcha, .h-captcha')
      ) {
        return { vendor: 'WAF', marker: 'human-verification' };
      }
      // A bare Cloudflare block page: dominated by a Ray ID with little else.
      if (/ray id:/.test(bodyText) && bodyText.length < 1200)
        return { vendor: 'Cloudflare', marker: 'ray-id-block' };
      return null;
    });
  } catch (_) {
    return null;
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
    // Same reachability floor as snapshot()/tap(): the tappable-walk index advance
    // below must stay byte-for-byte with snapshot()'s role+index, which now gates
    // on reachability, so the ground-truth role:<role>#<idx> selectors still join.
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
    const keyOf = (el) => {
      const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
      if (testid && testid.trim()) return 'testid:' + testid.trim();
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

    // Clear any stale tags from a prior state, then re-tag in document order.
    for (const e of document.querySelectorAll('[data-reproit-gt]'))
      e.removeAttribute('data-reproit-gt');
    const out = [];
    // perRole counts every style-visible interactive, so role:<role>#<idx>
    // selectors match the production SDK and snapshot() byte-for-byte even when
    // their viewports differ. The ground truth still emits only controls that
    // are reachable in the presented viewport. It also covers
    // a BROADER set: elements that are operable by pointer yet the tappable
    // grammar drops them (the <div role=option tabindex=-1> delegated case is
    // the motivating one). Such broader-only elements use the same explicit
    // author key or structural fallback as snapshot(), so they still join.
    const perRole = {};
    const root = document.body || document.documentElement;
    const walk = (el, isRoot) => {
      if (!isRoot && !visible(el)) {
        for (const c of el.children) walk(c, false);
        return;
      }
      if (!isRoot) {
        const role = roleOf(el);
        // The tappable walk takes only REACHABLE interactives, lockstep with
        // snapshot(), so role:<role>#<idx> indices match EXPLORE:STATE.
        const isReachable = reachable(el);
        const isInteractive = interactive(el, role);
        const structuralIndex = isInteractive ? perRole[role] || 0 : -1;
        if (isInteractive) perRole[role] = structuralIndex + 1;
        const inTappableWalk = isInteractive && isReachable;
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
        // Ground truth describes what a user can operate on the presented
        // viewport. Offscreen/occluded controls cannot be pointer-operable and
        // previously caused tens of thousands of serial CDP inspections on
        // virtualized docs trees without contributing a possible finding.
        const candidate = isReachable && (inTappableWalk || native || cursor || deleg);
        // The structural index was advanced above for every style-visible
        // interactive; candidate filtering must not renumber it.
        let sel;
        if (inTappableWalk) {
          const key = keyOf(el);
          sel = key ? 'key:' + key : 'role:' + role + '#' + structuralIndex;
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
            // reachable: a real user can hit this (on-screen + hit-testable). The
            // keyboard-activation probe must NOT focus+Enter an UNreachable control
            // (offstage / occluded), doing so fires its handler and lets reproit
            // reach a control a user can't, e.g. an offstage submit that throws.
            reachable: isReachable,
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

// What kinds of input listener does this tagged element carry? CDP-only.
// `pointer` = a real click/pointer handler (graph-1 operability); `key` = a real
// keydown/keypress/keyup handler. The key signal lets us catch "focusable but
// keyboard-dead" controls (a click-only div) WITHOUT pressing a key: if a
// non-native focusable control has a pointer handler but no key handler, Enter/
// Space genuinely do nothing -> a WCAG 2.1.1 gap. Cheaper and more precise than
// the old focus+Enter probe, and side-effect-free.
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
  const scroll = await page.evaluate(() => ({ x: window.scrollX, y: window.scrollY }));
  // Start from a clean baseline: blur whatever is focused onto body.
  await page.evaluate(() => {
    try {
      if (document.activeElement) document.activeElement.blur();
      document.body.focus();
    } catch (e) {}
  });
  const inTab = new Set();
  const visited = [];
  try {
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
  } finally {
    // Tab is a real user input, so focus-driven frameworks may mount tooltips,
    // contextual-layer portals, or lazy footer chrome while we measure keyboard
    // reachability. Do not leak that audit-only UI into the next app snapshot:
    // it would hash as a new screen and trigger another 60-step audit forever.
    await page
      .evaluate(({ x, y }) => {
        try {
          if (document.activeElement) document.activeElement.blur();
          const body = document.body;
          if (body) {
            const old = body.getAttribute('tabindex');
            body.setAttribute('tabindex', '-1');
            body.focus({ preventScroll: true });
            if (old == null) body.removeAttribute('tabindex');
            else body.setAttribute('tabindex', old);
          }
          window.scrollTo(x, y);
        } catch (_) {}
      }, scroll)
      .catch(() => {});
    // Give focusout-driven portal teardown two presented frames to finish before
    // exploration observes or acts on the page again.
    await page
      .evaluate(
        () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
      )
      .catch(() => {});
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

// Decode a Playwright PNG screenshot Buffer into a flat RGBA pixel array. Pure
// wrapper over pngjs so the diff (probe.mjs changedFraction) stays host-pure.
function pngToRgba(buf) {
  const { PNG } = createRequire(import.meta.url)('pngjs');
  const png = PNG.sync.read(buf);
  return { data: png.data, width: png.width, height: png.height };
}

// Tier-2 flicker oracle (gated, chromium/CDP only). Records the frames the
// compositor PRESENTS during a transition via CDP screencast, so the detector
// (flicker-oracle.mjs transientDivergence) can spot a transient flash that the
// settled-frame visual oracle never sees. Pixel + frame timing, so it is OFF by
// default and only emits when REPROIT_FLICKER_PIXELS=1; the engine treats it as
// a flicker finding that must reproduce across `check` repeats.
const FLICKER_PIXELS = process.env.REPROIT_FLICKER_PIXELS === '1';

// Start a screencast on a CDP session, buffering presented frames (small PNGs).
// Returns a handle with stop() -> Buffer[], or null when unavailable.
async function startScreencastCapture(cdp) {
  if (!FLICKER_PIXELS || !cdp) return null;
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

// Stop a capture, score the frame sequence for a transient divergence, and emit
// EXPLORE:FLICKER when one is found. Best-effort: any decode/diff failure is
// swallowed (the gated oracle never breaks a run).
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
  // Per-frame distance to the FINAL settled frame. Skip any frame whose
  // dimensions differ from the final (a resize, not a flash) rather than score
  // it as fully-different.
  const diffs = [];
  for (const f of rgbas) {
    if (
      f.width !== final.width ||
      f.height !== final.height ||
      f.data.length !== final.data.length
    ) {
      continue;
    }
    diffs.push(changedFraction(f.data, final.data));
  }
  const fl = transientDivergence(diffs);
  if (fl) {
    log('EXPLORE:FLICKER ' + JSON.stringify({ from, action, peak: fl.peak, frames: fl.frames }));
  }
}

// PIECE 2: the universal framebuffer-probe floor. For a bounded grid of viewport
// points, screenshot -> click the point -> screenshot -> diff. A point whose
// click changed pixels (operable) but which is covered by NO a11y/DOM
// interactive node is an operable region with no accessible control. DETERMINISTIC
// pixel-diff only (no ML); the same fraction-of-changed-pixels rule as the
// flicker oracle. Side-effecting (it clicks the page), so it runs only under
// REPROIT_PROBE=1 and stays bounded. Returns the operable-but-a11y-absent
// elements (probeRegionsToGroundtruth shape). Best-effort: any failure -> [].
// The page is reloaded to the start URL afterwards so the clicks don't corrupt
// the state the explorer is mapping.
async function runFramebufferProbe(page) {
  let vp;
  try {
    vp = page.viewportSize() || { width: 1280, height: 800 };
  } catch (_) {
    vp = { width: 1280, height: 800 };
  }
  const pts = gridPoints(vp.width, vp.height, DEFAULT_GRID);
  const probed = [];
  for (const pt of pts) {
    // a11y coverage: is there a DOM interactive / a11y-roled node under this
    // point? If so the point is already in graph 2; only UNCOVERED operable
    // points are findings. This is the deterministic "covered by an a11y node"
    // test the floor needs (elementFromPoint + a role/interactive check).
    let a11yCovered = true;
    let beforeBuf, afterBuf;
    try {
      a11yCovered = await page.evaluate(({ x, y }) => {
        const el = document.elementFromPoint(x, y);
        if (!el) return false;
        // Walk up: an ancestor may carry the role/handler for this hit area.
        for (let n = el; n; n = n.parentElement) {
          const tag = n.tagName ? n.tagName.toLowerCase() : '';
          if (['a', 'button', 'input', 'select', 'textarea'].includes(tag)) return true;
          const role = (n.getAttribute && n.getAttribute('role')) || '';
          if (role) return true;
          if (n.hasAttribute && (n.hasAttribute('onclick') || n.tabIndex >= 0)) return true;
        }
        return false;
      }, pt);
    } catch (_) {
      a11yCovered = true; /* unknown -> don't flag */
    }

    try {
      beforeBuf = await page.screenshot({ clip: clipAround(pt, vp), animations: 'disabled' });
      await page.mouse.click(pt.x, pt.y, { delay: 10 });
      await page.waitForTimeout(120);
      afterBuf = await page.screenshot({ clip: clipAround(pt, vp), animations: 'disabled' });
    } catch (_) {
      continue;
    }

    let changed = 0;
    try {
      const a = pngToRgba(beforeBuf);
      const b = pngToRgba(afterBuf);
      changed = changedFraction(a.data, b.data);
    } catch (_) {
      changed = 0;
    }
    probed.push({ x: pt.x, y: pt.y, changed, a11yCovered });
  }
  // The clicks may have navigated/mutated the page; restore the start screen so
  // the explorer's next snapshot reflects the real state, not a probe artifact.
  try {
    await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 });
    await page.waitForTimeout(300);
  } catch (_) {}
  const gaps = probeRegionsToGroundtruth(probed);
  if (gaps.length)
    log(
      `JOURNEY[a] step: framebuffer-probe found ${gaps.length} operable ` +
        'region(s) with no a11y control',
    );
  return gaps;
}

// A small clip box around a probe point (so each diff is local + cheap, and a
// click's local repaint isn't drowned out by a full-page diff). Clamped to the
// viewport. The box is the SAME before/after, so the diff is well-defined.
function clipAround(pt, vp) {
  const half = 40;
  const x = Math.max(0, Math.min(pt.x - half, vp.width - 1));
  const y = Math.max(0, Math.min(pt.y - half, vp.height - 1));
  const width = Math.max(1, Math.min(2 * half, vp.width - x));
  const height = Math.max(1, Math.min(2 * half, vp.height - y));
  return { x, y, width, height };
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
  // PIECE 2 floor: when opted in, the framebuffer probe contributes operable
  // regions that have NO a11y/DOM node (so gtCollect, which is DOM-based, can't
  // see them). Run it first; its results are appended to the records below.
  let probeEls = [];
  if (PROBE) {
    try {
      probeEls = await runFramebufferProbe(page);
    } catch (_) {
      probeEls = [];
    }
  }
  if (!els || !els.length) {
    // No DOM-discoverable elements, but the framebuffer probe may still have
    // found operable canvas/custom regions with no control.
    log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap: false, elements: probeEls }));
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
      // Native controls have structural pointer/keyboard semantics and need no
      // listener lookup. Avoid two serial CDP round trips per native element.
      if (els[i].native || els[i].reachable === false) continue;
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
    // keyboardActivatable, derived WITHOUT firing the control. Pressing Enter/
    // Space to probe activation would fire the app's real handler (a navigation
    // or a destructive/crashing action) as a side effect, polluting the crash
    // oracle and corrupting fuzz exploration. Instead we reason from structure:
    //  - must be focusable and on-screen at all; else not activatable.
    //  - a native control (button/a[href]/input/summary) is activated by the
    //    platform on Enter/Space, so it counts.
    //  - any element with a real key listener (keydown/keypress/keyup) counts.
    //  - a focusable, operable element that is NEITHER native NOR has a key
    //    listener (the classic click-only `<div role=button tabindex=0>`) is
    //    keyboard-DEAD: Enter does nothing -> keyboardActivatable=false, a real
    //    WCAG 2.1.1 gap. This is the case the old focus+Enter probe was meant to
    //    catch; we now catch it precisely and without side effects.
    // Without CDP (no listener enumeration) we can't see key handlers, so we
    // fall back to focusable && reachable rather than flag a gap we can't prove.
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

  // Append the framebuffer-probe floor's findings (operable regions with no DOM/
  // a11y node). These are addressed by spatial selector, so they never collide
  // with the DOM `sel` ids above.
  log(
    'EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap, elements: records.concat(probeEls) }),
  );
}

// STRUCTURAL tap: resolve a locale-invariant selector and click it. Returns
// true on success. Mirrors explorer.dart's tapSelector. No visible text is ever
// used to locate the element.
//   key:testid:<v> -> [data-testid="v"] (or data-test-id)
//   key:id:<v>     -> #<v>
//   key:name:<v>   -> [name="v"]
//   role:<role>#<idx> -> the idx-th visible tappable of that role, document order
async function tap(page, sel, opts) {
  // Identity first, through the SHARED resolver: whatever the snapshot indexed,
  // this finds. The element crosses back as a handle so the activation pass
  // below never re-derives it (a second walk is a second chance to disagree).
  const handle = await page.evaluateHandle(resolveStructuralTarget, sel).catch(() => null);
  const target = handle ? handle.asElement() : null;
  if (!target) {
    if (handle) await handle.dispose().catch(() => {});
    return false;
  }
