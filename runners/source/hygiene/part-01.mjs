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
    // Map-library attribution links (MapLibre/Mapbox/Leaflet/Google/OpenLayers)
    // sit at the bottom of the map canvas and are routinely covered by a bottom
    // sheet or overlay card on mobile map UIs -- a standard, intended layout, not
    // a bug. Their occlusion is a foreign-cover artifact of that pattern, so the
    // covered attribution is excluded (like site chrome). Verified on mytwenda.app:
    // OpenFreeMap/OpenMapTiles/OpenStreetMap links buried under the destination
    // sheet were a real geometric occlusion but an intentional map layout.
    if (
      el.closest(
        '.maplibregl-ctrl-attrib, .mapboxgl-ctrl-attrib, .leaflet-control-attribution, ' +
          '.ol-attribution, .gmnoprint, .gm-style-cc, [class*="ctrl-attrib" i], ' +
          '[class*="attribution" i]',
      )
    )
      continue;
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
