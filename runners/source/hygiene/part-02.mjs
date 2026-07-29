    try {
      const u = new URL(value, location.href);
      u.hash = '';
      return u.href;
    } catch (_) {
      return '';
    }
  };
  const facts = new Map();
  for (const fact of Array.isArray(networkFacts) ? networkFacts : []) {
    const url = norm(fact && fact.url);
    if (url) facts.set(url, fact);
  }
  const failedElements = window.__reproitCriticalResourceFailed || new WeakSet();
  const loadedElements = window.__reproitCriticalResourceLoaded || new WeakSet();
  const loadedSheets = new Set(
    Array.from(document.styleSheets || [])
      .map((sheet) => norm(sheet.href))
      .filter(Boolean),
  );
  const refs = [];
  for (const link of document.querySelectorAll('link[href]')) {
    const rel = (link.rel || '').toLowerCase().split(/\s+/);
    if (!rel.includes('stylesheet') || rel.includes('alternate') || link.disabled) continue;
    const media = link.media || '';
    if (media && media.toLowerCase() !== 'all' && !matchMedia(media).matches) continue;
    const url = norm(link.href);
    try {
      if (!url || new URL(url).origin !== origin) continue;
    } catch (_) {
      continue;
    }
    refs.push({
      type: 'stylesheet',
      url,
      key: link.id ? 'key:id:' + link.id : 'tag:link',
      element: link,
    });
  }
  for (const script of document.querySelectorAll('script[src]')) {
    const type = (script.getAttribute('type') || '').trim().toLowerCase();
    if (type && type !== 'module' && !/(java|ecma)script/.test(type)) continue;
    const url = norm(script.src);
    try {
      if (!url || new URL(url).origin !== origin) continue;
    } catch (_) {
      continue;
    }
    const protectedEmailTargets = Array.from(
      document.querySelectorAll('a.__cf_email__, [data-cfemail]'),
    );
    const codeSampleDecoder =
      /\/cdn-cgi\/scripts\/[^/]+\/cloudflare-static\/email-decode\.min\.js$/i.test(
        new URL(url).pathname,
      ) &&
      protectedEmailTargets.length > 0 &&
      protectedEmailTargets.every((target) => target.closest('pre,code,[role="code"]'));
    if (codeSampleDecoder) continue;
    refs.push({
      type: 'script',
      url,
      key: script.id ? 'key:id:' + script.id : 'tag:script',
      element: script,
    });
  }
  // CSSOM exposes the import graph on every engine. Walk same-origin active
  // stylesheets recursively so a failed @import is attributed to the exact URL,
  // not merely to the healthy parent <link> that referenced it.
  const seenSheets = new Set();
  const walkImports = (sheet, rootKey, parentUrl) => {
    if (!sheet || seenSheets.has(sheet)) return;
    seenSheets.add(sheet);
    let rules;
    try {
      rules = sheet.cssRules;
    } catch (_) {
      return;
    }
    for (const rule of Array.from(rules || [])) {
      if (rule.type !== 3 || !rule.href) continue; // CSSRule.IMPORT_RULE
      const media = (rule.media && rule.media.mediaText) || '';
      if (media && media.toLowerCase() !== 'all' && !matchMedia(media).matches) continue;
      const url = norm(rule.href);
      try {
        if (!url || new URL(url).origin !== origin) continue;
      } catch (_) {
        continue;
      }
      refs.push({
        type: 'stylesheet',
        url,
        key: rootKey,
        parent: parentUrl,
        imported: true,
        element: null,
      });
      walkImports(rule.styleSheet, rootKey, url);
    }
  };
  for (const link of document.querySelectorAll('link[href]')) {
    const rel = (link.rel || '').toLowerCase().split(/\s+/);
    if (!rel.includes('stylesheet') || rel.includes('alternate') || link.disabled) continue;
    const media = link.media || '';
    if (media && media.toLowerCase() !== 'all' && !matchMedia(media).matches) continue;
    const href = norm(link.href);
    const sheet = Array.from(document.styleSheets || []).find(
      (candidate) => norm(candidate.href) === href,
    );
    walkImports(sheet, link.id ? 'key:id:' + link.id : 'tag:link', href);
  }
  // The DOM has no standard JavaScript module-graph API. A root module's error
  // event plus a hard failed same-origin script request proves the dependency
  // chain broke, but the portable browser APIs do not expose the direct
  // initiator edge. Report the exact failed URL on every engine. Associate it
  // with a root only when exactly one root failed; otherwise say unavailable
  // instead of guessing.
  const directUrls = new Set(refs.map((ref) => ref.url));
  const rejectedRoots = refs.filter(
    (ref) => ref.type === 'script' && ref.element && failedElements.has(ref.element),
  );
  if (rejectedRoots.length) {
    const root = rejectedRoots.length === 1 ? rejectedRoots[0] : null;
    for (const fact of facts.values()) {
      const url = norm(fact && fact.url);
      const hardFailure =
        fact &&
        (fact.status === 404 ||
          fact.status === 410 ||
          fact.status >= 500 ||
          (fact.failure && !fact.cancelled));
      if (
        !url ||
        directUrls.has(url) ||
        fact.resourceType !== 'script' ||
        fact.optional ||
        !hardFailure
      )
        continue;
      try {
        if (new URL(url).origin !== origin) continue;
      } catch (_) {
        continue;
      }
      refs.push({
        type: 'script',
        url,
        key: root ? root.key : 'tag:script',
        parent: root && root.url,
        dependency: true,
        element: null,
      });
      directUrls.add(url);
    }
  }
  const seen = new Set();
  const add = (ref, reason, fact) => {
    const id = ref.type + '|' + ref.url;
    if (seen.has(id) || out.length >= 20) return;
    seen.add(id);
    const detail = [
      ref.url,
      ref.parent ? 'root=' + ref.parent : '',
      ref.dependency ? 'parent=unavailable' : '',
      fact && fact.status != null ? 'status=' + fact.status : '',
      fact && fact.contentType ? 'content-type=' + fact.contentType : '',
      fact && fact.failure ? 'failure=' + fact.failure : '',
    ]
      .filter(Boolean)
      .join(' ');
    out.push({ key: ref.key, reason, detail: detail.slice(0, 240) });
  };
  for (const ref of refs) {
    const fact = facts.get(ref.url);
    if (fact && fact.optional) continue;
    const sameUrlLoaded = refs.some(
      (other) => other.url === ref.url && loadedElements.has(other.element),
    );
    const browserRejected = failedElements.has(ref.element) && !sameUrlLoaded;
    if (fact && (fact.status === 404 || fact.status === 410 || fact.status >= 500)) {
      add(
        ref,
        ref.imported
          ? 'stylesheet-import-http'
          : ref.dependency
            ? 'module-dependency-http'
            : ref.type + '-http',
        fact,
      );
      continue;
    }
    if (
      fact &&
      fact.failure &&
      !/(ERR_ABORTED|NS_BINDING_ABORTED|cancelled|canceled)/i.test(fact.failure)
    ) {
      add(
        ref,
        ref.imported
          ? 'stylesheet-import-request'
          : ref.dependency
            ? 'module-dependency-request'
            : ref.type + '-request',
        fact,
      );
      continue;
    }
    const mime = String((fact && fact.contentType) || '')
      .split(';')[0]
      .trim()
      .toLowerCase();
    if (
      ref.type === 'stylesheet' &&
      mime &&
      mime !== 'text/css' &&
      (browserRejected || !loadedSheets.has(ref.url))
    ) {
      add(ref, 'stylesheet-mime', fact);
      continue;
    }
    const jsMime =
      /^(text|application)\/(x-)?(java|ecma)script$/.test(mime) || mime === 'application/node';
    if (ref.type === 'script' && mime && !jsMime && browserRejected) {
      add(ref, 'script-mime', fact);
      continue;
    }
    if (fact && fact.cancelled) continue;
    const exactChildFailure = refs.some((child) => {
      if (child.parent !== ref.url || (!child.imported && !child.dependency)) return false;
      const childFact = facts.get(child.url);
      return (
        childFact &&
        (childFact.status === 404 ||
          childFact.status === 410 ||
          childFact.status >= 500 ||
          (childFact.failure && !childFact.cancelled))
      );
    });
    if (browserRejected && exactChildFailure) continue;
    if (browserRejected) add(ref, ref.type + '-load', fact);
  }
  return out;
}

// SAFE-AREA oracle: EXCLUDED on web (no scan here) for lack of ground truth.
// CSS env(safe-area-inset-*) is the only web signal for a device inset, and the
// headless Chromium/WebKit the runner drives report all four insets as 0 -- the
// browser is never told about a physical display cutout, so there is no notch /
// Dynamic Island / home-indicator geometry to measure a control against. The
// oracle is therefore native-only (Flutter viewPadding / Appium Android
// getSystemBars); porting a zero-inset scan here would only ever be silent.
//
// PERMISSION-WALK oracle: EXCLUDED on web -- a browser has no runtime OS
// permission the runner can DENY the way Appium/Flutter can (permission prompts
// are per-origin gated by the user agent, not a fuzzer-drivable environment), so
// there is no denial sweep to run.

// ZOOM-REFLOW support (WCAG 1.4.10 Reflow, EAA-mandatory), two self-contained
// in-page halves around the runner's viewport swap:
//   1. zoomTappableKeys() runs at the ORIGINAL viewport and returns the stable
//      keys and origins of the interactive elements that are actually visible
//      in the viewport (not hidden/aria-hidden/inert or off-canvas). These are the controls a
//      zoomed re-render must keep usable.
//   2. zoomReflowScan(preKeys) runs at the HALVED viewport (the CSS-size
//      equivalent of 200% zoom) and returns the WCAG violations:
//        - hscroll  : the document now requires TWO-DIMENSIONAL scrolling (its
//                     scrollWidth exceeds the zoomed innerWidth by >16px, i.e.
//                     a horizontal scrollbar appeared on vertically-scrolling
//                     content -- fixed-width content that does not reflow).
//        - collapsed: a pre-zoom-visible tappable whose hit rect collapsed
//                     below 1px while still rendered. An element the page
//                     HIDES at the narrow width (display:none / visibility:
//                     hidden ancestor -- the responsive hamburger pattern) is
//                     intentional adaptation, not a break, so it is skipped:
//                     only a still-rendered, still-visible control squeezed to
//                     zero counts.
// Both are pure layout facts at fixed viewports (no pixels, no timing), so a
// finding reproduces identically on any machine. Returns [{key, kind, by}].
export function zoomTappableKeys() {
  const SEL =
    'a[href], button, input:not([type=hidden]), select, textarea, ' +
    '[role="button"], [role="link"], [role="checkbox"], [role="tab"], ' +
    '[role="menuitem"], [onclick]';
  const keys = [];
  for (const el of document.querySelectorAll(SEL)) {
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) continue;
    // Only controls that are actually in the initial viewport establish the
    // reflow relation. Skip links, route announcers, and carousel rails commonly
    // keep rendered controls far off-canvas until focus/animation brings them in.
    if (r.right <= 0 || r.bottom <= 0 || r.left >= innerWidth || r.top >= innerHeight) continue;
    const cs = getComputedStyle(el);
    if (cs.visibility === 'hidden' || cs.display === 'none' || parseFloat(cs.opacity) === 0)
      continue;
    if (el.closest('[aria-hidden="true"], [inert]')) continue;
    const key = el.id
      ? 'key:id:' + el.id
      : (el.getAttribute('aria-label') || el.textContent || '').trim().slice(0, 40) ||
        'tag:' + el.tagName.toLowerCase();
    keys.push({ key, x: Math.round(r.left), y: Math.round(r.top) });
    if (keys.length >= 200) break;
  }
  return keys;
}

export function zoomReflowScan(preKeys) {
  const out = [];
  // Two-dimensional scrolling: the whole document grew a horizontal scrollbar
  // at the zoomed width. The 16px tolerance absorbs scrollbar gutters and
  // rounding, matching the WCAG understanding doc's "small tolerance" intent.
  const doc = document.documentElement;
  const width = Math.max(doc.scrollWidth, document.body ? document.body.scrollWidth : 0);
  const over = Math.round(width - window.innerWidth);
  if (over > 16) {
    // Attribute the overflow before firing. WCAG 1.4.10 EXEMPTS content that
    // requires two-dimensional layout for its use or meaning -- data tables, code
    // blocks, images/diagrams/maps, and anything the user scrolls inside its own
    // horizontal-scroll region. A doc/marketing page whose only sideways scroll at
    // the zoomed width comes from a lone wide code sample or table is NOT a reflow
    // break (that was the false positive). So hscroll fires ONLY when a NON-exempt,
    // non-locally-scrollable element itself exceeds the viewport width -- a
    // fixed-width layout container that genuinely failed to reflow.
    const EXEMPT =
      'pre, code, table, thead, tbody, tr, td, th, figure, img, svg, video, ' +
      'canvas, iframe, object, embed, map, [class*="highlight" i], ' +
      '[class*="code" i], [class*="carousel" i], [class*="marquee" i]';
    const vw = window.innerWidth;
    let culprit = false;
    const all = document.body ? document.body.querySelectorAll('*') : [];
    let scanned = 0;
    for (const el of all) {
      if (scanned++ > 4000) break;
      const r = el.getBoundingClientRect();
      if (r.width < vw) continue; // not itself wider than the viewport
      if (r.right <= vw + 16 && r.left >= -16) continue; // fully on-screen (no sideways spill)
      if (el.matches(EXEMPT) || el.closest(EXEMPT)) continue; // 2D-layout-exempt content
      // Inside a horizontal-scroll region -> intended local scrolling, not a
      // whole-page reflow break.
      let local = false;
      for (let a = el.parentElement; a; a = a.parentElement) {
        const s = getComputedStyle(a);
        if (
          (s.overflowX === 'auto' || s.overflowX === 'scroll') &&
          a.scrollWidth > a.clientWidth + 4
        ) {
          local = true;
          break;
        }
      }
      if (local) continue;
      culprit = true;
      break;
    }
    if (culprit) out.push({ key: 'tag:html', kind: 'hscroll', by: over });
  }
  const pre = new Map(
    (preKeys || [])
      .map((v) => (typeof v === 'string' ? [v, null] : [v && v.key, v]))
      .filter(([k]) => k),
  );
  const SEL =
    'a[href], button, input:not([type=hidden]), select, textarea, ' +
    '[role="button"], [role="link"], [role="checkbox"], [role="tab"], ' +
    '[role="menuitem"], [onclick]';
  const seen = new Set();
  for (const el of document.querySelectorAll(SEL)) {
    const key = el.id
      ? 'key:id:' + el.id
      : (el.getAttribute('aria-label') || el.textContent || '').trim().slice(0, 40) ||
        'tag:' + el.tagName.toLowerCase();
    if (!pre.has(key) || seen.has(key)) continue;
    // A collapsed control is only a reportable USABILITY loss if it is a NAMED
    // control (an id, an accessible name, or visible text). A bare, empty anchor
    // (key falls back to `tag:a` -- no id, no aria-label, no text) that shrinks to
    // zero at the narrow width is a decorative / spacer / icon-wrapper link, not a
    // control the user lost; flagging it was a false positive. So skip the tag-only
    // fallback key.
    if (key.startsWith('tag:')) continue;
    // Intentionally hidden at this width (self OR ancestor display:none gives
    // zero client rects; visibility inherits) -> responsive design, skip.
    if (!el.getClientRects().length) continue;
    const cs = getComputedStyle(el);
    if (cs.visibility === 'hidden' || cs.display === 'none' || parseFloat(cs.opacity) === 0)
      continue;
    if (el.closest('[aria-hidden="true"], [inert]')) continue;
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) {
      // A narrow-layout breakpoint may replace a desktop control and leave its
      // old node as a zero-sized/off-canvas shell. That is responsive adaptation,
      // not an unusable control. A genuine squeeze remains at its former screen
      // position; require the collapsed origin to stay in the viewport and near
      // the baseline origin.
      const was = pre.get(key);
      if (r.left < -1 || r.top < -1 || r.left > innerWidth + 1 || r.top > innerHeight + 1) continue;
      if (was && (Math.abs(r.left - was.x) > 32 || Math.abs(r.top - was.y) > 32)) continue;
      seen.add(key);
      out.push({ key, kind: 'collapsed', by: Math.round(Math.min(r.width, r.height)) });
      if (out.length >= 20) break;
    }
  }
  return out;
}

// SCROLL ROUND-TRIP (list-recycling / virtualization): the content at a pinned
// offset must be IDENTICAL after scrolling a list away and back. A virtualized
// list that recycles a row without rebinding its data shows DIFFERENT content at
// the same position after the round-trip. Metamorphic: scroll-down-then-back is
// an identity for the content at a fixed offset. The fingerprint is read via
// elementFromPoint at fixed SCREEN coordinates near the top of the scroller, so
// a stable list returns identical text and a recycler returns different text;
// pure-number tokens are normalized out so legitimately dynamic value-state (a
// clock, a counter) never counts as a mismatch. Self-restoring (the original
// scroll offset is put back). Async so virtualization can settle across frames.
// Returns [{pos, before, after}] capped; [] when the list is stable or there is
// no scroller to drive.
export async function scrollRoundTripScan() {
  const raf = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  const norm = (s) =>
    String(s || '')
      .replace(/\d[\d.,:]*/g, '#')
      .replace(/\s+/g, ' ')
      .trim()
      .slice(0, 120);
  const MARGIN = 200; // a scroller must have this much hidden content to test
  // Find the primary vertical scroller: the largest by area, document included.
  const candidates = [];
  const de = document.scrollingElement || document.documentElement;
  if (de && de.scrollHeight - de.clientHeight > MARGIN) {
    candidates.push({
      el: de,
      doc: true,
      area: (de.clientWidth || window.innerWidth) * (de.clientHeight || window.innerHeight),
    });
  }
  let scanned = 0;
  for (const el of document.querySelectorAll('*')) {
    if (scanned++ > 4000) break;
    const cs = getComputedStyle(el);
    if (cs.overflowY !== 'auto' && cs.overflowY !== 'scroll') continue;
    if (el.scrollHeight - el.clientHeight <= MARGIN) continue;
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    candidates.push({ el, doc: false, area: r.width * r.height });
    if (candidates.length >= 50) break;
  }
  if (!candidates.length) return [];
  candidates.sort((a, b) => b.area - a.area);
  const sc = candidates[0];
  const el = sc.el;
  // Three fixed points near the top of the scroller's viewport band.
  const rect = sc.doc
    ? { left: 0, top: 0, width: window.innerWidth, height: window.innerHeight }
    : el.getBoundingClientRect();
  const band = Math.min(rect.height, MARGIN);
  const cx = Math.round(rect.left + rect.width / 2);
  const pts = [0.2, 0.5, 0.8].map((f) => ({ x: cx, y: Math.round(rect.top + band * f) }));
  const sampleAt = (p) => {
    const e = document.elementFromPoint(p.x, p.y);
    if (!e) return null;
    const text = norm(e.textContent);
    if (!text) return null;
    const r = e.getBoundingClientRect();
    const role = (e.getAttribute('role') || '').toLowerCase();
    // Comparability matters more than raw text: a virtualized surface can be
    // briefly empty after the return, making elementFromPoint hit its large
    // ancestor. Comparing a former leaf with that ancestor's concatenated text
    // manufactured list-recycling findings. A real recycled row presents the
    // same element shape at the same point with different bound content.
    const shape = [
      e.tagName.toLowerCase(),
      role,
      Math.min(e.childElementCount, 9),
      Math.round(Math.min(r.width, 1000) / 20),
      Math.round(Math.min(r.height, 1000) / 10),
    ].join('|');
    return { text, shape };
  };
  const startTop = sc.doc ? window.scrollY || de.scrollTop || 0 : el.scrollTop;
  const toTop = () => {
    if (sc.doc) window.scrollTo(0, 0);
    else el.scrollTop = 0;
  };
  const toBottom = () => {
    if (sc.doc) window.scrollTo(0, de.scrollHeight);
    else el.scrollTop = el.scrollHeight;
  };
  try {
    toTop();
    await raf();
    const before = pts.map(sampleAt);
    toBottom();
    await raf();
    await raf();
    toTop();
    await raf();
    await raf();
    const after = pts.map(sampleAt);
    await raf();
    const confirmed = pts.map(sampleAt);
    const out = [];
    const seen = new Set();
    for (let i = 0; i < pts.length; i++) {
      if (!before[i] || !after[i] || !confirmed[i]) continue;
      // Same structural row/leaf before and after, and a stable post-return
      // sample. Shape drift or a changing second sample means virtualization is
      // still settling, not that the application rebound a row incorrectly.
      if (before[i].shape !== after[i].shape || after[i].shape !== confirmed[i].shape) continue;
      if (after[i].text !== confirmed[i].text) continue;
      if (before[i].text === after[i].text) continue;
      const pos = 'y=' + pts[i].y;
      if (seen.has(pos)) continue;
      seen.add(pos);
      out.push({ pos, before: before[i].text, after: after[i].text });
      if (out.length >= 10) break;
    }
    return out;
  } finally {
    // Restore the original scroll offset so the walk continues undisturbed.
    try {
      if (sc.doc) window.scrollTo(0, startTop);
      else el.scrollTop = startTop;
    } catch (_) {}
  }
}

// DUPLICATE-SUBMIT eligibility: is the element the runner just clicked (stashed
// by tap() as window.__reproitLastTap) a submit-like control? True for a
// submit-type control inside a <form> (a <button> in a form defaults to type
// submit) regardless of its name, and for any button-role control whose
// accessible name reads like a submission verb (submit/save/pay/order/confirm/
// checkout/send/post/buy). Evaluated in-page between the probe's first and
// second click, so the opt-in double dispatch (REPROIT_DUPSUBMIT=1) only ever
// targets real submit controls. Self-contained (browser globals only), like
// every helper in this module.
export function dupSubmitEligible() {
  const el = window.__reproitLastTap;
  if (!el || !el.isConnected) return false;
  const tag = el.tagName ? el.tagName.toLowerCase() : '';
  const type = ((el.getAttribute && el.getAttribute('type')) || '').toLowerCase();
  if (el.closest && el.closest('form')) {
    if (tag === 'input' && type === 'submit') return true;
    if (tag === 'button' && (type === '' || type === 'submit')) return true;
  }
  const role = ((el.getAttribute && el.getAttribute('role')) || '').toLowerCase();
  const isButton =
    tag === 'button' ||
    role === 'button' ||
    (tag === 'input' && (type === 'submit' || type === 'button'));
  if (!isButton) return false;
  const name = (el.getAttribute('aria-label') || el.value || el.textContent || '').trim();
  return /submit|save|pay|order|confirm|checkout|send|post|buy/i.test(name);
}

// FOCUS-LOSS support: did a non-navigating tap drop keyboard focus to <body>?
// focusLossArm() is evaluated in-page immediately BEFORE a tap: it records the
// pre-tap activeElement and the open dialog/popover count, and arms the probe
// flag that makes tap()'s doClick focus the control before clicking (a real
// user click gives the control keyboard focus; el.click() alone does not).
// focusLossCheck() is evaluated after the settle and applies the guards:
//   - the tapped control must still exist (a control removed by its own
//     re-render legitimately resets focus: skip),
//   - link/anchor taps and elements with href/target never fire (navigation
//     controls are expected to move focus),
//   - a dialog/popover count change never fires (opening or closing a modal
//     legitimately moves focus),
//   - focus must have been somewhere real at the tap (the control took focus,
//     or a live element already held it) and be on <body>/null now.
// A true result means the interaction's re-render stole keyboard focus, so a
// keyboard user loses their place. Window refs only, never a DOM mutation, so
// the signature/content/mutation oracles are untouched.
export function focusLossArm() {
  window.__reproitFocusProbe = true;
  window.__reproitTapFocused = false;
  window.__reproitFocusPre = document.activeElement;
  // Count only RENDERED dialogs/popovers: a display:none [role=dialog] shell
  // that a tap then shows must register as a count CHANGE, or the guard misses
  // the open. (Kept inline: every helper here must be self-contained.)
  let dialogs = 0;
  for (const d of document.querySelectorAll(
    '[aria-modal="true"], dialog[open], [role="dialog"], ' + '[role="alertdialog"]',
  )) {
    const cs = getComputedStyle(d);
    if (d.getClientRects().length && cs.visibility !== 'hidden' && cs.display !== 'none') dialogs++;
  }
  try {
    dialogs += document.querySelectorAll(':popover-open').length;
  } catch (_) {}
  window.__reproitDialogsPre = dialogs;
}

export function focusLossCheck() {
  const pre = window.__reproitFocusPre;
  const tapped = window.__reproitLastTap;
  window.__reproitFocusProbe = false;
  if (!tapped || !tapped.isConnected) return false;
  const tag = tapped.tagName ? tapped.tagName.toLowerCase() : '';
  if (tag === 'a' || (tapped.closest && tapped.closest('a'))) return false;
  if (tapped.hasAttribute('href') || tapped.hasAttribute('target')) return false;
  // Rendered dialogs/popovers only, mirroring focusLossArm's count.
  let dialogs = 0;
  for (const d of document.querySelectorAll(
    '[aria-modal="true"], dialog[open], [role="dialog"], ' + '[role="alertdialog"]',
  )) {
    const cs = getComputedStyle(d);
    if (d.getClientRects().length && cs.visibility !== 'hidden' && cs.display !== 'none') dialogs++;
  }
  try {
    dialogs += document.querySelectorAll(':popover-open').length;
  } catch (_) {}
  if (dialogs !== (window.__reproitDialogsPre | 0)) return false;
  // The TAPPED control itself must have held focus BEFORE activation -- the exact
  // keyboard flow this oracle exists for: a user TABS to a control (so the control
  // is focused), activates it, and the interaction's re-render then steals focus
  // to <body>, leaving the user's place gone. Two artifacts must NOT be mistaken
  // for that, and both are excluded by requiring pre === the tapped control:
  //   1. A fresh MOUSE activation of a never-focused button. On macOS Chromium and
  //      WebKitGTK a real mouse click does not focus a button (an OS convention),
  //      so activeElement stays on <body>; the probe's synthetic pre-click
  //      el.focus() (recorded in __reproitTapFocused) is not a user's focus and is
  //      ignored. This fired on EVERY ordinary button on the Electron/Tauri clean
  //      apps -- a platform artifact, not a loss.
  //   2. Focus that was on some OTHER element (an input the user typed into, or a
  //      control the previous action left focused, incl. the probe's own leftover
  //      synthetic focus). Activating THIS control while focus sat elsewhere and
  //      ending on <body> is not this control losing its own focus.
  // pre is captured by focusLossArm BEFORE the probe's focus(), so it reflects the
  // genuine pre-interaction activeElement.
  const hadFocus = pre && pre === tapped && pre.isConnected;
  if (!hadFocus) return false;
  const now = document.activeElement;
  return !now || now === document.body || now === document.documentElement;
}

// LISTENER-LEAK support (opt-in revisit probe, REPROIT_LISTENERLEAK=1), two
// self-contained in-page halves:
//   1. installListenerLeakCounter() is injected as an INIT script so it runs
//      before any page script on every document. It wraps
//      EventTarget.prototype.add/removeEventListener to tally live listeners
//      (adds - removes) on window.__reproitLL. Idempotent per document (the
//      patched flag), and because a client-side SPA navigation keeps the same
//      document, the tally accumulates across in-app route changes -- exactly the
//      surface a mount/unmount listener leak lives on. A FULL page load re-runs
//      the init script and resets the tally, so a classic multi-page site (which
//      cannot leak listeners across a document swap) never false-positives.
//   2. listenerLeakSample() reads the live listener count and the attached DOM
//      node count (getElementsByTagName('*').length) for one revisit sample.
// Both are pure reads/window refs (no DOM mutation), so they never perturb the
// signature/content/mutation oracles. The runner drives the revisit loop and
// decides the monotonic-climb verdict; these just install + sample.
export function installListenerLeakCounter() {
  try {
    if (window.__reproitLLPatched) return;
    window.__reproitLLPatched = true;
    window.__reproitLL = { adds: 0, removes: 0 };
    const EP = EventTarget.prototype;
    const origAdd = EP.addEventListener;
    const origRemove = EP.removeEventListener;
    EP.addEventListener = function () {
      try {
        window.__reproitLL.adds++;
      } catch (_) {}
      return origAdd.apply(this, arguments);
    };
    EP.removeEventListener = function () {
      try {
        window.__reproitLL.removes++;
      } catch (_) {}
      return origRemove.apply(this, arguments);
    };
  } catch (_) {}
}

export function listenerLeakSample() {
  const ll = window.__reproitLL || { adds: 0, removes: 0 };
  let nodes = 0;
  try {
    nodes = document.getElementsByTagName('*').length;
  } catch (_) {}
  return { live: (ll.adds | 0) - (ll.removes | 0), nodes };
}
