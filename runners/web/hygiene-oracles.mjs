// Shared DOM-hygiene oracle scans for every DOM-based runner (web, electron,
// tauri). Each export is a SELF-CONTAINED function passed straight to
// `page.evaluate(...)`: it closes over nothing but browser globals, so it
// serializes cleanly into any Chromium/webview page. Keeping these in one module
// (like `choice-oracle.mjs`) means the occlusion + security oracles are defined
// ONCE and shared across runners instead of copy-pasted per platform.
//
// These are pure, deterministic DOM/URL predicates (no pixels, no wall-clock), so
// a finding reproduces identically on any machine. Callers wrap the result in the
// per-state marker: `EXPLORE:OCCLUSION` / `EXPLORE:SECURITY` /
// `EXPLORE:RELATION`.

// DETACHED INDICATOR: an explicit structural relationship contract. This scan
// never guesses that a red/circular/small element is a badge. It evaluates only
// elements whose application author declared all three roles:
//
//   <nav id="main-nav" data-reproit-indicator-container>
//     <button id="inbox" data-reproit-indicator-owner
//             data-reproit-indicator-max-gap="8">Inbox</button>
//     <span id="inbox-unread" data-reproit-indicator-for="inbox"></span>
//   </nav>
//
// The indicator-for value is an exact DOM id reference. The referenced owner
// must opt in with data-reproit-indicator-owner, and its closest declared
// container must have a stable id and contain both nodes. Missing/ambiguous/
// hidden/animating relationships ABSTAIN and stay silent. A settled,
// uniquely-resolved relationship is SATISFIED when the indicator is within the
// declared max gap (8 CSS px by default) and inside the container, otherwise it
// is a VIOLATION. Callers confirm a VIOLATION item in a second settled sample
// before emitting a marker.
export function indicatorRelationshipScan() {
  const indicators = [...document.querySelectorAll('[data-reproit-indicator-for]')];
  const result = { outcome: 'ABSTAIN', items: [], checks: [], proven: 0, valid: 0, unknown: 0 };
  const visible = (el) => {
    if (!el || !el.isConnected) return false;
    const r = el.getBoundingClientRect();
    if (!(r.width > 0 && r.height > 0)) return false;
    for (let n = el; n && n.nodeType === 1; n = n.parentElement) {
      const s = getComputedStyle(n);
      if (s.display === 'none' || s.visibility === 'hidden' || s.visibility === 'collapse')
        return false;
      if (parseFloat(s.opacity) === 0 || s.contentVisibility === 'hidden') return false;
      if (
        n.hasAttribute('hidden') ||
        n.hasAttribute('inert') ||
        n.getAttribute('aria-hidden') === 'true'
      )
        return false;
    }
    return true;
  };
  const stable = (nodes) =>
    !nodes.some((node) => {
      try {
        return node
          .getAnimations({ subtree: true })
          .some(
            (animation) => animation.playState === 'running' || animation.playState === 'pending',
          );
      } catch (_) {
        return true;
      }
    });
  const rect = (el) => {
    const r = el.getBoundingClientRect();
    return {
      left: r.left,
      top: r.top,
      right: r.right,
      bottom: r.bottom,
      width: r.width,
      height: r.height,
    };
  };
  const gap = (a, b) => {
    const dx = Math.max(a.left - b.right, b.left - a.right, 0);
    const dy = Math.max(a.top - b.bottom, b.top - a.bottom, 0);
    return Math.hypot(dx, dy);
  };
  const contains = (outer, inner, epsilon = 1) =>
    inner.left >= outer.left - epsilon &&
    inner.top >= outer.top - epsilon &&
    inner.right <= outer.right + epsilon &&
    inner.bottom <= outer.bottom + epsilon;

  for (const indicator of indicators) {
    const ownerId = (indicator.getAttribute('data-reproit-indicator-for') || '').trim();
    // Stable structural identities are mandatory. CSS selectors, visible text,
    // and generated array indices are deliberately unsupported.
    if (!indicator.id || !ownerId) {
      result.unknown++;
      continue;
    }
    const owner = document.getElementById(ownerId);
    if (!owner || !owner.hasAttribute('data-reproit-indicator-owner')) {
      result.unknown++;
      continue;
    }
    // getElementById returns one node even for malformed duplicate ids. Refuse
    // to prove ownership unless both identities are globally unique.
    const esc = (value) => {
      try {
        return CSS.escape(value);
      } catch (_) {
        return null;
      }
    };
    const ownerEsc = esc(ownerId),
      indicatorEsc = esc(indicator.id);
    if (
      !ownerEsc ||
      !indicatorEsc ||
      document.querySelectorAll('#' + ownerEsc).length !== 1 ||
      document.querySelectorAll('#' + indicatorEsc).length !== 1
    ) {
      result.unknown++;
      continue;
    }
    const container = owner.closest('[data-reproit-indicator-container]');
    if (
      !container ||
      !container.id ||
      !container.contains(owner) ||
      !container.contains(indicator)
    ) {
      result.unknown++;
      continue;
    }
    const containerEsc = esc(container.id);
    if (!containerEsc || document.querySelectorAll('#' + containerEsc).length !== 1) {
      result.unknown++;
      continue;
    }
    if (
      !visible(indicator) ||
      !visible(owner) ||
      !visible(container) ||
      !stable([indicator, owner, container])
    ) {
      result.unknown++;
      continue;
    }
    const rawGap = owner.getAttribute('data-reproit-indicator-max-gap');
    const maxGap = rawGap == null || rawGap.trim() === '' ? 8 : Number(rawGap);
    if (!Number.isInteger(maxGap) || maxGap < 0 || maxGap > 64) {
      result.unknown++;
      continue;
    }
    const indicatorRect = rect(indicator),
      ownerRect = rect(owner),
      containerRect = rect(container);
    const distance = gap(indicatorRect, ownerRect);
    const violation = !contains(containerRect, indicatorRect)
      ? 'escaped-container'
      : distance > maxGap
        ? 'detached'
        : null;
    const identity = {
      kind: 'indicator-anchor',
      dependentKey: 'key:id:' + indicator.id,
      ownerKey: 'key:id:' + owner.id,
      containerKey: 'key:id:' + container.id,
    };
    if (!violation) {
      result.valid++;
      result.checks.push({ ...identity, outcome: 'SATISFIED' });
      continue;
    }
    result.proven++;
    result.checks.push({ ...identity, outcome: 'VIOLATION', violation });
    result.items.push({
      ...identity,
      violation,
      maxGap,
      gap: Math.round(distance * 100) / 100,
    });
  }
  result.items.sort((a, b) =>
    (a.dependentKey + '\0' + a.ownerKey).localeCompare(b.dependentKey + '\0' + b.ownerKey),
  );
  result.checks.sort((a, b) =>
    (a.dependentKey + '\0' + a.ownerKey).localeCompare(b.dependentKey + '\0' + b.ownerKey),
  );
  result.outcome =
    result.proven > 0
      ? 'VIOLATION'
      : result.unknown > 0
        ? 'ABSTAIN'
        : result.valid > 0
          ? 'SATISFIED'
          : 'ABSTAIN';
  return result;
}

// Require the exact same structural relationship and violation in two settled
// samples. Geometry is evidence, not identity, so harmless sub-pixel jitter does
// not flip a proof; a changing violation is an unstable layout and is dropped.
export function confirmRelationshipViolations(first, second) {
  if (!first || !second || !Array.isArray(first.items) || !Array.isArray(second.items)) return [];
  const identity = (item) =>
    [item.kind, item.dependentKey, item.ownerKey, item.containerKey, item.violation].join('\0');
  const confirmed = new Set(second.items.map(identity));
  return first.items.filter((item) => confirmed.has(identity(item)));
}

// OCCLUSION: an interactive element presented as usable (visible, in the
// viewport, effectively rendered) whose CENTER is covered by a FOREIGN, OPAQUE
// element that is NOT legitimate chrome and NOT an open overlay -- a click there
// hits the covering element, not the control (a z-index accident, a mispositioned
// opaque card, a stray badge over a button). FP guards, in order:
//   - EFFECTIVELY-HIDDEN (ancestor-aware): a control inside a CLOSED flyout /
//     collapsed disclosure / unopened widget is present in the DOM but not
//     presented as clickable. A closed panel commonly sets opacity:0 (which does
//     NOT inherit, so the control's own computed opacity is 1) or content-
//     visibility:hidden on an ANCESTOR, so the per-element visibility check misses
//     it. We walk ancestors and skip if any is display:none / visibility hidden|
//     collapse / opacity 0 / content-visibility hidden / [hidden] / aria-hidden /
//     inert. This alone drops closed <details>, unopened DocSearch, collapsed nav.
//   - OFF-SCREEN: center outside the viewport (an sr-only skip-link parked
//     off-viewport until focused, an off-screen tab panel / carousel slide).
//   - OPEN OVERLAY / BACKDROP as the cover: when a modal / dialog / popover / drawer
//     / ad iframe is open, the background is LEGITIMATELY covered. If the covering
//     element is (or sits inside) an overlay (role dialog/alertdialog, aria-modal,
//     :popover-open, <dialog>, a class like modal/backdrop/overlay/mask/scrim/
//     drawer/lightbox), or is a fixed/absolute element spanning most of the
//     viewport (a full-screen backdrop / promo iframe), the control is behind an
//     open overlay -- not a bug.
//   - SITE CHROME as the cover: a sticky/fixed header, a nav dropdown / flyout, a
//     toolbar legitimately overlays scrolled content and reveals-on-focus links.
//     If the covering element is (or sits inside) nav/header/menu/flyout/toolbar,
//     skip (this is the widget's own chrome over its own collapsed content, or a
//     sticky bar over the page, never a foreign overlay).
// Returns [{target, cover}]. Only a genuine foreign opaque cover survives.
export function occlusionScan() {
  const SEL =
    'a[href], button, input:not([type=hidden]), select, textarea, ' +
    '[role="button"], [role="link"], [role="checkbox"], [role="tab"], ' +
    '[role="menuitem"], [onclick]';
  const OVERLAY_SEL =
    '[role="dialog"], [role="alertdialog"], [aria-modal="true"], dialog, ' +
    'iframe, [class*="modal" i], [class*="backdrop" i], [class*="overlay" i]' +
    ', [class*="mask" i], [class*="scrim" i], [class*="popover" i], ' +
    '[class*="drawer" i], [class*="lightbox" i]';
  // Site chrome + page furniture the elementFromPoint mismatch is INTENDED for: a
  // sticky/fixed header or nav dropdown / flyout over scrolled content; a footer;
  // an ad / promo / cookie / sponsor placement band (MDN's <mdn-placement-top>, a
  // page-layout banner); and prose formatting (a <code>/<pre> token over a link).
  // None is a foreign occluding overlay -- covering here is by design.
  const CHROME_SEL =
    'nav, header, footer, [role="navigation"], [role="banner"], ' +
    '[role="contentinfo"], [role="menubar"], [role="menu"], [role="toolbar"]' +
    ', pre, code, kbd, samp, [class*="nav" i], [class*="header" i], ' +
    '[class*="footer" i], [class*="flyout" i], [class*="menu" i], ' +
    '[class*="navbar" i], [class*="toolbar" i], [class*="dropdown" i], ' +
    '[class*="banner" i], [class*="placement" i], [class*="advert" i], ' +
    '[class*="promo" i], [class*="sponsor" i], [class*="cookie" i]';
  const vw = window.innerWidth,
    vh = window.innerHeight;
  // Ancestor-aware "effectively not rendered": any ancestor that hides the subtree
  // (closed flyout / collapsed disclosure / aria-hidden region) means the control
  // is not presented as clickable right now.
  const effHidden = (el) => {
    for (let a = el; a && a.nodeType === 1; a = a.parentElement) {
      const s = getComputedStyle(a);
      if (s.display === 'none' || s.visibility === 'hidden' || s.visibility === 'collapse')
        return true;
      if (parseFloat(s.opacity) === 0) return true;
      if (s.contentVisibility === 'hidden') return true;
      if (
        a.hasAttribute('hidden') ||
        a.getAttribute('aria-hidden') === 'true' ||
        a.hasAttribute('inert')
      )
        return true;
      // A CLOSED <details> collapses everything but its <summary>. A control in the
      // collapsed body is NOT presented as clickable, even when the page keeps it
      // laid out (custom disclosures animate height and leave the content with a
      // real rect, so it hit-tests onto whatever paints in front of it -- the
      // svelte.dev section-picker FP: menu links inside a closed examples-select
      // <details> landing on the article/code behind them). The <summary> itself
      // stays shown, so only suppress content OUTSIDE it.
      if (a.tagName === 'DETAILS' && !a.open) {
        const summary = a.querySelector(':scope > summary');
        if (!(summary && summary.contains(el))) return true;
      }
    }
    return false;
  };
  // Scrolled OUT of a clipping ancestor's viewport: a control inside an
  // overflow:auto/scroll/hidden/clip container (a scrollable dropdown list, a
  // virtualized panel, a horizontally-scrolled row) keeps its layout rect even
  // when scrolled past the container's clip box, so its rect lands on whatever
  // paints behind the container -> elementFromPoint returns a foreign element and
  // it reads as "occluded". But the control is CLIPPED AWAY, not presented as
  // usable, so it is not an occlusion. (This was the svelte.dev tutorial-picker
  // FP: links scrolled below the examples dropdown's overflow:auto viewport
  // hit-tested onto the editor pane behind it.) Skip when the center is outside a
  // clipping ancestor's box.
  const clippedOut = (el, px, py) => {
    for (let a = el.parentElement; a && a.nodeType === 1; a = a.parentElement) {
      const s = getComputedStyle(a);
      const clips = (v) => v && v !== 'visible';
      if (!clips(s.overflowX) && !clips(s.overflowY)) continue;
      const ar = a.getBoundingClientRect();
      if (ar.width === 0 && ar.height === 0) continue;
      const outX = clips(s.overflowX) && (px < ar.left - 2 || px > ar.right + 2);
      const outY = clips(s.overflowY) && (py < ar.top - 2 || py > ar.bottom + 2);
      if (outX || outY) return true;
    }
    return false;
  };
  // The cover (or an ancestor of it) is a full-viewport backdrop: a fixed/absolute
  // box spanning most of the viewport, i.e. an open modal scrim or promo iframe.
  const isBackdrop = (el) => {
    for (let a = el, i = 0; a && a.nodeType === 1 && i < 6; a = a.parentElement, i++) {
      const s = getComputedStyle(a);
      if (s.position === 'fixed' || s.position === 'absolute' || s.position === 'sticky') {
        const r = a.getBoundingClientRect();
        if (r.width >= vw * 0.6 && r.height >= vh * 0.6) return true;
      }
    }
    return false;
  };
  // The cover VISUALLY obscures the control only if it paints OPAQUE pixels over
  // it. A transparent-background element (a text <p>, a wrapper <a>/<div> whose
  // only paint is its own text) does NOT hide the control beneath it -- the
  // control is still fully visible to the user, and the elementFromPoint mismatch
  // is a harmless DOM-stacking artifact of an INTENDED overlap (a stretched-link
  // card whose whole area is a link, overlapping nav/action links, a code editor
  // line over a token). Those were the bulk of the false positives. So the cover
  // must be replaced media, or carry a background image, or a background color
  // with real alpha, to count as an occlusion.
  const opaqueCover = (h) => {
    const tag = (h.tagName || '').toLowerCase();
    if (['img', 'svg', 'video', 'canvas', 'iframe', 'object', 'embed', 'picture'].includes(tag))
      return true;
    const cs = getComputedStyle(h);
    if (cs.backgroundImage && cs.backgroundImage !== 'none') return true;
    const m = (cs.backgroundColor || '').match(/rgba?\(([^)]+)\)/);
    if (m) {
      const p = m[1].split(',').map((s) => parseFloat(s));
      const a = p.length >= 4 ? p[3] : 1;
      if (a >= 0.5) return true;
    }
    return false;
  };
  const out = [];
  for (const el of document.querySelectorAll(SEL)) {
    const r = el.getBoundingClientRect();
    if (r.width < 4 || r.height < 4) continue;
    const cx = r.left + r.width / 2,
      cy = r.top + r.height / 2;
    if (cx < 0 || cy < 0 || cx > vw || cy > vh) continue;
    if (effHidden(el)) continue;
    if (clippedOut(el, cx, cy)) continue;
    const hit = document.elementFromPoint(cx, cy);
    if (!hit || hit === el || el.contains(hit) || hit.contains(el)) continue;
    // The cover is a legitimate open overlay / backdrop -> the background being
    // covered is expected, not a bug.
    if (hit.closest(OVERLAY_SEL) || isBackdrop(hit)) continue;
    // The cover is site chrome / page furniture (sticky header, nav dropdown /
    // flyout, footer, ad-placement / promo band, prose <code>) -- covering here is
    // by design, not a foreign overlay.
    if (hit.closest(CHROME_SEL)) continue;
    // A custom-element ad/placement container (e.g. <mdn-placement-top>) whose
    // tag name itself names the slot.
    if (/placement|advert|sponsor/i.test(hit.tagName || '')) continue;
    // A <label> covering the form control it labels IS the visual affordance for a
    // visually-hidden native input (the styled-checkbox / radio / toggle pattern,
    // e.g. Bootstrap's .btn-check + label.btn) -- the "covered" input is meant to
    // be driven through its label, not a bug.
    if (
      hit.closest('label') &&
      el.matches('input, select, textarea, [role="checkbox"], [role="radio"], ' + '[role="switch"]')
    )
      continue;
    // The cover must actually PAINT over the control (opaque). A transparent text
    // element on top (an intended stretched-link / overlapping-link / code-editor
    // overlap) leaves the control fully visible -- not an occlusion.
    if (!opaqueCover(hit)) continue;
    const key = el.id
      ? 'key:id:' + el.id
      : (el.getAttribute('aria-label') || el.textContent || '').trim().slice(0, 40) ||
        el.tagName.toLowerCase();
    const cover =
      hit.tagName.toLowerCase() +
      (hit.id
        ? '#' + hit.id
        : hit.className && typeof hit.className === 'string'
          ? '.' + hit.className.trim().split(/\s+/)[0]
          : '');
    out.push({ target: key, cover: cover.slice(0, 60) });
  }
  return out.slice(0, 20);
}

// Occlusion RE-CONFIRMATION (runner-side, pure): keep only the occlusions that
// survive a second occlusionScan taken a short beat later at the SAME state.
// A real z-index-buried control persists identically across both frames; a
// TRANSIENT overlap -- a menu/disclosure mid-open, a dropdown list mid-scroll,
// an animating panel -- has cleared (or its cover has shifted) by the second
// frame, so it drops out. This was the svelte.dev playground FP: an
// examples-menu link whose center momentarily landed on a neighbouring
// `span.icon` while the <details> was animating; the settled frame is clean.
// Matches on target AND cover so a shifting-cover transient (same control,
// different element under it each frame) is rejected, while a stable buried
// control is kept. Runs in Node over two plain arrays; the delay is the
// runner's own wait between the two evaluate() calls.
export function confirmOcclusions(first, second) {
  if (!Array.isArray(first) || !Array.isArray(second)) return [];
  const seen = new Set(second.map((o) => o.target + ' ' + o.cover));
  return first.filter((o) => seen.has(o.target + ' ' + o.cover));
}

// SECURITY hygiene: pure DOM/URL predicates.
//   - tabnabbing (reverse tabnabbing): a cross-origin target=_blank link that
//     EXPLICITLY opts back INTO the vulnerability with rel="opener". A plain
//     target=_blank is NOT a vulnerability: every modern engine implies
//     rel=noopener for target=_blank (Chrome 88+, Firefox 79+, Safari 12.1+, all
//     shipped by 2021), so the opened page cannot reach window.opener unless the
//     author deliberately re-enabled it with rel="opener". So we fire ONLY when
//     rel contains the "opener" token (and not noopener) on a cross-origin
//     _blank link -- an intentional, genuinely exploitable reverse-tabnabbing
//     surface. This makes the oracle near-silent in the wild (correctly: plain
//     _blank is safe now); an exploitable window.open() is a runtime JS call the
//     static DOM cannot observe, so it is out of scope here.
//   - insecure-form / mixed-content: an HTTPS document with an http: form action
//     or http: subresource. Gated on https so an http dev page never
//     false-positives. Returns [{kind, target}].
export function securityScan() {
  const out = [];
  const seen = new Set();
  const add = (kind, target) => {
    const k = kind + '|' + target;
    if (!seen.has(k)) {
      seen.add(k);
      out.push({ kind, target });
    }
  };
  const https = location.protocol === 'https:';
  for (const a of document.querySelectorAll('a[target="_blank"][href][rel]')) {
    const rel = (a.getAttribute('rel') || '').toLowerCase();
    // rel="opener" (and no noopener) is the ONLY DOM shape that re-enables the
    // window.opener leak the browser default suppresses.
    if (!/\bopener\b/.test(rel) || /\bnoopener\b/.test(rel)) continue;
    try {
      const u = new URL(a.href, location.href);
      if (u.origin !== location.origin && (u.protocol === 'http:' || u.protocol === 'https:')) {
        add('tabnabbing', (a.textContent || a.href).trim().slice(0, 60));
      }
    } catch (_) {}
  }
  if (https) {
    for (const f of document.querySelectorAll('form[action]')) {
      try {
        if (new URL(f.action, location.href).protocol === 'http:')
          add('insecure-form', f.getAttribute('action').slice(0, 60));
      } catch (_) {}
    }
    for (const el of document.querySelectorAll(
      'img[src], script[src], iframe[src], link[rel~="stylesheet"][href]',
    )) {
      const src = el.getAttribute('src') || el.getAttribute('href') || '';
      try {
        if (new URL(src, location.href).protocol === 'http:') {
          add('mixed-content', src.slice(0, 60));
          break;
        }
      } catch (_) {}
    }
  }
  return out.slice(0, 10);
}

// BLANK-SCREEN (white-screen-of-death): the page rendered NOTHING -- zero
// visible text nodes, zero interactive controls, and zero visible media
// (img/svg/canvas/video) -- in a non-empty viewport. The classic shape is an
// SPA whose mount threw before render: the server answered 200, the DOM holds
// a bare root div, and the user sees white. Load-time FP guards: the caller
// runs this only after its settle wait, and the scan itself requires a
// laid-out document.body with a non-zero box, so a document still parsing
// never fires. A media-only page (a full-bleed hero image, a canvas game) is
// NOT blank, hence the media check. Returns [{key, w, h}] -- one record naming
// the scanned root and the viewport -- or [] when any content is visible.
export function blankScreenScan() {
  if (!document.body) return [];
  const vw = window.innerWidth,
    vh = window.innerHeight;
  if (!(vw > 0 && vh > 0)) return [];
  const br = document.body.getBoundingClientRect();
  if (br.width <= 0 && br.height <= 0) return [];
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) return false;
    for (let current = el; current; ) {
      const cs = getComputedStyle(current);
      if (cs.visibility === 'hidden' || cs.display === 'none' || parseFloat(cs.opacity) === 0)
        return false;
      current = current.parentElement || current.getRootNode()?.host || null;
    }
    return true;
  };
  const roots = [document.body];
  for (let index = 0; index < roots.length; index++) {
    for (const element of roots[index].querySelectorAll('*')) {
      if (element.shadowRoot) roots.push(element.shadowRoot);
    }
  }
  // Any visible non-whitespace text node means the screen is not blank.
  // script/style/template text is not rendered, so it never counts.
  for (const root of roots) {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let node;
    while ((node = walker.nextNode())) {
      if (!/\S/.test(node.nodeValue || '')) continue;
      const el = node.parentElement;
      if (!el) continue;
      if (el.closest('script, style, noscript, template')) continue;
      if (visible(el)) return [];
    }
  }
  const SEL =
    'a[href], button, input:not([type=hidden]), select, textarea, ' +
    '[role="button"], [role="link"], [role="checkbox"], [role="tab"], ' +
    '[role="menuitem"], [onclick]';
  for (const root of roots) {
    for (const el of root.querySelectorAll(SEL)) if (visible(el)) return [];
    for (const el of root.querySelectorAll('img, svg, canvas, video, picture, object, embed'))
      if (visible(el)) return [];
  }
  // A screen made entirely from styled boxes still rendered content. This is
  // common in chart, overflow, skeleton, and visual-regression fixtures. Count
  // only a substantial painted box so a bare transparent SPA mount stays blank.
  for (const root of roots) {
    for (const el of root.querySelectorAll('*')) {
      if (!visible(el)) continue;
      const rect = el.getBoundingClientRect();
      if (rect.width * rect.height < 256) continue;
      const style = getComputedStyle(el);
      const painted =
        !['rgba(0, 0, 0, 0)', 'transparent'].includes(style.backgroundColor) ||
        style.backgroundImage !== 'none' ||
        style.boxShadow !== 'none' ||
        parseFloat(style.borderTopWidth) > 0 ||
        parseFloat(style.borderRightWidth) > 0 ||
        parseFloat(style.borderBottomWidth) > 0 ||
        parseFloat(style.borderLeftWidth) > 0;
      if (painted) return [];
    }
  }
  // A visible LOADING / spinner / skeleton / progress indicator means the screen is
  // MID-LOAD, not a permanently-blank WSOD -- never fire while one is shown. Reached
  // only when the page has no text/control/media, so the DOM is tiny and this walk
  // is cheap. Matches an aria-busy region, a progressbar/status role, <progress>, or
  // a class token like loading/loader/spinner/skeleton/shimmer/placeholder.
  const LOADING_RE = new RegExp(
    '(^|[\\s_-])(loading|loader|spinner|skeleton|shimmer|placeholder|busy)' + '([\\s_-]|$)',
    'i',
  );
  for (const root of roots) {
    for (const el of root.querySelectorAll('*')) {
      if (!visible(el)) continue;
      if (el.tagName === 'PROGRESS') return [];
      if ((el.getAttribute('aria-busy') || '') === 'true') return [];
      const role = (el.getAttribute('role') || '').toLowerCase();
      if (role === 'progressbar' || role === 'status') return [];
      if (LOADING_RE.test(el.getAttribute('class') || '')) return [];
    }
  }
  // MALFORMED-MARKUP guard (the "CSS-as-text" case): an unclosed <style> that ate
  // the document (or a big CSS dump left in the DOM) leaves a visually blank
  // viewport, but the page is NOT a white-screen-of-death -- kilobytes of real
  // content exist in the DOM, they just failed to render because the markup is
  // broken. A genuine WSOD (a failed SPA mount) leaves an EMPTY DOM, not a large
  // trapped-text blob. So a page whose <style> text is disproportionately large is
  // a markup/CSS bug, not blank.
  let styleTextLen = 0;
  for (const st of document.querySelectorAll('style'))
    styleTextLen += (st.textContent || '').length;
  if (styleTextLen > 10000) return [];
  return [{ key: 'tag:body', w: Math.round(vw), h: Math.round(vh) }];
}

// BROKEN-ASSET: dead subresources rendered in the state, three classes, all
// pure DOM/resource status facts (the verdict never depends on network timing
// because the caller runs after the settle wait, when loads have resolved):
//   - img : an <img> that FINISHED loading with no pixels (complete &&
//           naturalWidth === 0) and a non-empty src -- a wrong path, a 404, or
//           a corrupt file. A still-loading img has complete === false, so it
//           never false-positives mid-load.
//   - tofu: a VISIBLE text node containing U+FFFD, the replacement character an
//           encoding failure renders as tofu. Only rendered text counts
//           (script/style text and hidden nodes are skipped).
// Returns [{key, reason, detail}], capped; [] when every asset is healthy.
// `injectedValues` (optional) is the set of strings the fuzzer TYPED into the app
// this run. An asset that only exists because a fuzzer-injected value was reflected
// into the DOM (an XSS-probe `<img src=x>` typed into a field that the app echoes)
// is NOT an app bug, so it is excluded by provenance: any img whose src, or tofu
// whose text, is a fragment of an injected value is skipped.
export function brokenAssetScan(injectedValues) {
  const out = [];
  const push = (key, reason, detail) => {
    if (out.length < 20) out.push({ key, reason, detail: String(detail || '').slice(0, 80) });
  };
  // Normalized injected values for substring provenance checks.
  const injected = (Array.isArray(injectedValues) ? injectedValues : [])
    .map((v) => String(v == null ? '' : v).toLowerCase())
    .filter((v) => v.length > 0);
  // The asset/text is fuzzer-provenanced when a fuzz value contains it (a short
  // src/attr echoed from the probe) OR it contains a fuzz value (a rendered text
  // node that wraps the reflected probe). The contains-direction requires a
  // non-trivial fuzz value so a 1-char value cannot suppress everything.
  const fromFuzzInjection = (needle) => {
    const n = String(needle || '').toLowerCase();
    if (!n) return false;
    return injected.some((v) => v.indexOf(n) !== -1 || (v.length >= 3 && n.indexOf(v) !== -1));
  };
  // A favicon / touch-icon / manifest icon is BROWSER CHROME, never painted into
  // page content, so a broken one is not a rendered-content bug. Skip by src.
  const isChromeIcon = (src) =>
    new RegExp(
      '(^|\\/)(favicon(\\.ico)?|apple-touch-icon[\\w-]*\\.png|mstile[\\w-]*\\.png)' + '(\\?|#|$)',
      'i',
    ).test(src) || /\.ico(\?|#|$)/i.test(src);
  for (const img of document.querySelectorAll('img[src]')) {
    const src = img.getAttribute('src') || '';
    if (!src.trim()) continue;
    if (isChromeIcon(src)) continue;
    // Provenance: the raw src attribute (or the whole probe markup) came from a
    // fuzzer-injected value -> not the app's own content.
    if (fromFuzzInjection(src) || fromFuzzInjection(img.outerHTML)) continue;
    if (!(img.complete && img.naturalWidth === 0)) continue;
    // Only flag an image the user ACTUALLY SEES broken. A DOM-present img that is
    // not rendered (a lazy/off-screen image whose optimizer URL 404s but that the
    // user never scrolled to, a zero-size or hidden img, a preloaded swap target)
    // is not a rendered-content bug -- this was the Next.js /_next/image FP. So the
    // img must have a non-zero on-screen box and not be hidden.
    const r = img.getBoundingClientRect();
    if (r.width <= 1 || r.height <= 1) continue;
    if (
      r.bottom <= 0 ||
      r.top >= (window.innerHeight || 0) ||
      r.right <= 0 ||
      r.left >= (window.innerWidth || 0)
    )
      continue;
    const ics = getComputedStyle(img);
    if (ics.visibility === 'hidden' || ics.display === 'none' || parseFloat(ics.opacity) === 0)
      continue;
    if ((img.getAttribute('loading') || '').toLowerCase() === 'lazy' && !img.complete) continue;
    push(img.id ? 'key:id:' + img.id : 'tag:img', 'img', src);
  }
  // FONT findings are DELIBERATELY not emitted from FontFace.status: a headless
  // browser reports status==='error' for a webfont even when the system fallback
  // renders the text perfectly (no visible defect), which was a false positive. A
  // font problem that actually reaches the screen surfaces as rendered U+FFFD tofu,
  // caught by the text scan below (the original broken-asset spec).
  const root = document.body || document.documentElement;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let node;
  while ((node = walker.nextNode())) {
    const text = node.nodeValue || '';
    if (text.indexOf('�') === -1) continue;
    const el = node.parentElement;
    if (!el || el.closest('script, style, noscript, template')) continue;
    // Provenance: tofu the fuzzer itself typed (a unicode/RTL probe reflected back)
    // is not an app encoding bug.
    if (fromFuzzInjection(text.trim())) continue;
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    const cs = getComputedStyle(el);
    if (cs.visibility === 'hidden' || cs.display === 'none' || parseFloat(cs.opacity) === 0)
      continue;
    push(
      el.id ? 'key:id:' + el.id : 'tag:' + el.tagName.toLowerCase(),
      'tofu',
      text.trim().slice(0, 60),
    );
  }
  return out;
}

// CRITICAL RESOURCE observer + scan. The observer runs before page scripts and
// records browser-confirmed load errors for DOM-referenced stylesheets/scripts.
// The settled scan correlates those errors with Playwright response facts, so a
// document returning 200 cannot hide a missing or MIME-blocked render dependency.
// Same-origin only; inactive media stylesheets and non-executable script data
// blocks are excluded. This deliberately ignores prefetch/preload, analytics on
// third-party origins, and intentionally aborted requests.
export function installCriticalResourceObserver() {
  if (window.__reproitCriticalResourceObserver) return;
  window.__reproitCriticalResourceObserver = true;
  window.__reproitCriticalResourceFailed = new WeakSet();
  window.__reproitCriticalResourceLoaded = new WeakSet();
  const bindLoad = (el) => {
    if (!el || el.nodeType !== 1 || el.__reproitCriticalBound) return;
    const tag = (el.tagName || '').toLowerCase();
    const critical =
      (tag === 'script' && el.src) ||
      (tag === 'link' && (el.rel || '').toLowerCase().split(/\s+/).includes('stylesheet'));
    if (!critical) return;
    el.__reproitCriticalBound = true;
    el.addEventListener('load', () => window.__reproitCriticalResourceLoaded.add(el), {
      once: true,
    });
  };
  new MutationObserver((records) => {
    for (const record of records)
      for (const node of record.addedNodes) {
        bindLoad(node);
        if (node && node.querySelectorAll)
          for (const el of node.querySelectorAll('script[src], link[rel~="stylesheet"][href]'))
            bindLoad(el);
      }
  }).observe(document, { childList: true, subtree: true });
  const record = (event) => {
    const el = event && event.target;
    if (!el || el.nodeType !== 1) return;
    const tag = (el.tagName || '').toLowerCase();
    const isCss =
      tag === 'link' && (el.rel || '').toLowerCase().split(/\s+/).includes('stylesheet');
    const isScript = tag === 'script' && !!el.src;
    if (!isCss && !isScript) return;
    const url = isCss ? el.href : el.src;
    if (!url) return;
    if (event.type === 'error') window.__reproitCriticalResourceFailed.add(el);
    if (event.type === 'load') window.__reproitCriticalResourceLoaded.add(el);
  };
  addEventListener('error', record, true);
}

export function criticalResourceScan(networkFacts) {
  const out = [];
  const origin = location.origin;
  const norm = (value) => {
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
