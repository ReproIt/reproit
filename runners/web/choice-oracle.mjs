// CHOICE-ANOMALY oracle core (host-pure helpers + a self-contained in-page
// exercise pass), shared by every browser-backed runner.
//
// The oracle is DIFFERENTIAL, which is what keeps it false-positive-free. A
// multi-choice component (ARIA tab/radio group, a button-cluster picker, or a
// native <select>) has every choice produce a SIMILAR effect on the page outside
// itself (the expected, common behavior); a bug is the ONE choice whose effect on
// the GLOBAL layout is an OUTLIER versus its siblings. It fires only when one
// choice's effect is >= CHOICE_OUTLIER_RATIO x the sibling median AND at least
// CHOICE_MIN_MAGNITUDE px, so uniform choices produce nothing.
//
// This module holds the parts that are IDENTICAL across web / electron / tauri:
//   - the thresholds (one source of truth, never re-invented per runner),
//   - the host-pure differencing helpers (layoutDelta, medianOf) and the outlier
//     classifier (classifyChoiceOutlier),
//   - measureGlobalLayoutInPage: the global-layout fingerprint, as a function that
//     runs in the page (used directly by web/electron via page.evaluate, and
//     stringified into the WebDriver execute() body for tauri),
//   - CHOICE_ANOMALY_IN_PAGE_SRC: a self-contained async page routine that finds
//     choice components, exercises each option, measures the global layout, and
//     returns the outlier finding (or null) WITHOUT any host round-trips. Runners
//     that cannot drive option-by-option from the host (the WebDriver tauri
//     runner; the lean electron runner whose snapshot does not carry per-option
//     metadata) run this whole pass in-page. The web reference runner keeps its
//     richer host-driven exercise (it boxes the outlier for recorded clips) and
//     only reuses the thresholds + classifier from here, so there is exactly one
//     definition of "what counts as a choice anomaly".
//
// The host-pure pieces are exported so a unit test can drive them directly; the
// in-page pieces are exported both as functions (for page.evaluate) and as a
// source string (for execute()), so there is no second copy to drift.

export const CHOICE_OUTLIER_RATIO = 3; // outlier magnitude >= 3x the sibling median ...
export const CHOICE_MIN_MAGNITUDE = 24; // ...and at least this many px of global move.
export const CHOICE_ROLES = ['tab', 'radio', 'menuitemradio'];

// Total px the global layout moved between two fingerprints: horizontal-overflow
// delta + document-height (flow) delta + anchor displacement.
//
// The flow term (|d scrollH|) is THE canonical signal: the case this oracle was
// built for is a code-language picker where one language's selection reflows the
// whole page (hundreds of px of document height) while its siblings land within
// a few px of each other -- the page visibly jumps for that one choice. The
// DIFFERENTIAL thresholds keep the legit case quiet: a category/preview picker
// whose panes all differ by comparable amounts has a sibling median close to the
// max, so no outlier fires.
//
// Anchors are KEYED triples [key, top, left] in VIEWPORT coords and matched by
// key, never index: a sticky bar that un-pins between measurements drops out of
// the list, and index-matching would then compare unrelated elements. An anchor
// present on only one side contributes nothing (leaving the anchored set is not
// a positional move). Host-pure: same fingerprints in -> same number out, on
// every backend.
export function layoutDelta(base, cur) {
  if (!base || !cur) return 0;
  let d = Math.abs((cur.hOverflow || 0) - (base.hOverflow || 0));
  d += Math.abs((cur.scrollH || 0) - (base.scrollH || 0));
  const cm = new Map();
  for (const a of cur.anchors || []) cm.set(a[0], a);
  for (const b of base.anchors || []) {
    const c = cm.get(b[0]);
    if (!c) continue;
    d += Math.abs(c[1] - b[1]) + Math.abs(c[2] - b[2]);
  }
  return d;
}

export function medianOf(xs) {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
}

// THE outlier rule, in one place. `results` is a list of { mag } (one per option,
// `mag` is the option's global-layout delta vs the group baseline; null mags are
// dropped by the caller before this point). Returns the winning index + its stats
// when one option is an OUTLIER vs its siblings, else null. Needs >= 3 valid
// options so >= 2 siblings define the norm. Pure + deterministic: the same mags
// always yield the same verdict, identically on web, electron, and tauri.
export function classifyChoiceOutlier(mags) {
  const valid = mags.filter((m) => typeof m === 'number' && Number.isFinite(m));
  if (valid.length < 3) return null;
  let maxIdx = 0;
  for (let i = 1; i < valid.length; i++) if (valid[i] > valid[maxIdx]) maxIdx = i;
  const max = valid[maxIdx];
  const siblings = valid.filter((_, i) => i !== maxIdx);
  const med = medianOf(siblings);
  const isOutlier = max >= CHOICE_MIN_MAGNITUDE && max >= CHOICE_OUTLIER_RATIO * Math.max(med, 1);
  if (!isOutlier) return null;
  return { magnitude: Math.round(max), siblingMedian: Math.round(med) };
}

// Capture a GLOBAL-layout fingerprint: page horizontal overflow + the VIEWPORT
// positions of PINNED (fixed, or sticky-while-stuck) chrome anchors, so a choice
// that legitimately swaps content of a different height (a category / preview
// picker, a synced code-language picker) does NOT register -- only a real
// geometry break does. Pinned chrome must not move IN THE VIEWPORT regardless of
// the active choice, and its viewport position is scroll-invariant. (An earlier
// fingerprint used page-absolute coords, which for FIXED chrome means
// `rect.top + scrollY` -- scroll-DEPENDENT: a synced picker that changed content
// height above the component moved scrollY and read as "the header moved";
// measured FP on a docs quickstart page, blaming an innocent option.) A sticky
// bar that is NOT currently pinned is ordinary flow content whose movement under
// a content swap is expected, so it is skipped. Anchors are keyed by tag +
// query-list index so key identity survives the stuck-state filter. Identical
// body to the web runner's measureGlobalLayout, kept here as the single source
// the electron page.evaluate and the tauri execute() string both use.
export function measureGlobalLayoutInPage() {
  // Measure from a FIXED scroll (top): the choice exercise scrolls each option into
  // view, and on a lazy-loading page different scroll depths load different amounts
  // of content, drifting far-down anchors by thousands of px between options (a
  // progressive-load artifact, not a reflow). Pinning scroll to 0 gives every option
  // the same lazy-load state; only the above-the-fold hero is anchored below. Force
  // an INSTANT jump: many sites set CSS `scroll-behavior:smooth`, under which a
  // plain scrollTo animates and the rects below are read MID-SCROLL, which shifts
  // the "above-fold" set and injects huge phantom deltas.
  try {
    window.scrollTo({ top: 0, left: 0, behavior: 'instant' });
  } catch (_) {
    window.scrollTo(0, 0);
  }
  document.documentElement.scrollTop = 0;
  const de = document.documentElement;
  const anchors = [];
  const els = document.querySelectorAll('header, nav, [role=banner], [role=navigation]');
  for (let i = 0; i < els.length; i++) {
    const el = els[i];
    const cs = getComputedStyle(el);
    if (cs.position !== 'fixed' && cs.position !== 'sticky') continue;
    const r = el.getBoundingClientRect();
    if (r.width <= 0) continue;
    if (cs.position === 'sticky') {
      const topPx = parseFloat(cs.top);
      if (!Number.isFinite(topPx) || Math.abs(r.top - topPx) > 1) continue; // not pinned right now
    }
    anchors.push([el.tagName.toLowerCase() + ':' + i, Math.round(r.top), Math.round(r.left)]);
  }
  // FLOW-content landmarks in DOCUMENT-absolute coords (scroll-invariant for flow
  // content: rect.top + scrollY is the same no matter where the page is scrolled).
  // This is THE signal document.scrollHeight misses: when one option's pane is
  // taller (the code-language case -- Go's sample is ~60px taller than its
  // siblings), the page does NOT necessarily grow its total scrollHeight (trailing
  // whitespace / a height-coupled hero row absorbs it), yet every heading BELOW
  // the picker visibly shifts down. Keyed by tag + clipped text, which is stable
  // across a language switch (the CODE text changes, headings do not), so the
  // by-key delta compares the same element before/after. Summed displacement over
  // many shifted headings makes the outlier option tower over its (~0-shift)
  // siblings. Bounded for determinism. Pinned chrome (measured above in VIEWPORT
  // coords) is excluded here so a scroll change can never inject a phantom delta.
  const seen = {};
  const vh = window.innerHeight || 800;
  const marks = document.querySelectorAll('h1,h2,h3,h4,h5,h6,[role=heading]');
  for (let i = 0; i < marks.length && anchors.length < 40; i++) {
    const el = marks[i];
    const cs = getComputedStyle(el);
    if (cs.position === 'fixed' || cs.position === 'sticky') continue; // chrome, not flow
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    // Above-the-fold only (scroll is pinned to 0): a taller pane pushes these hero
    // headings down; below-fold headings are lazy/accumulating, so excluded.
    if (r.top < 0 || r.top > vh) continue;
    const txt = (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 40);
    if (!txt) continue;
    const key = 'f:' + el.tagName.toLowerCase() + ':' + txt;
    if (seen[key]) continue;
    seen[key] = 1;
    anchors.push([key, Math.round(r.top), Math.round(r.left)]);
  }
  return {
    hOverflow: Math.max(0, de.scrollWidth - window.innerWidth),
    scrollH: de.scrollHeight,
    anchors,
  };
}

// Self-contained in-page choice-anomaly pass. Designed to be serialized to a
// string (so it works over both Playwright page.evaluate and WebDriver execute(),
// which require a pure function body with NO closure over module scope) -- every
// helper it needs is defined inside. It:
//   1. finds choice components on the page:
//        - native <select> with >= 3 <option>s (FEATURE 1: the most common
//          real-world picker, which the snapshot maps to a text field so it is
//          otherwise never differenced),
//        - ARIA tab/radio/menuitemradio groups (>= 3 options, scoped by their
//          owning tablist/radiogroup/menu/fieldset container so two independent
//          groups never merge),
//        - button-cluster pickers (>= 3 same-parent buttons, exactly one
//          selected),
//   2. exercises EACH option (select() for <select> dispatching change+input so
//      frameworks react; click() for ARIA/button options), measuring the
//      global-layout fingerprint after it SETTLES (sampled to stability, so an
//      async shift is charged to the option that caused it, never the next one
//      measured) as CHAINED deltas, plus a wrap-around re-select so the chain's
//      baseline option gets a real magnitude too,
//   3. classifies candidates with the SHARED threshold rule and CAUSALLY
//      CONFIRMS the winner with an isolated quiet-sibling -> candidate A/B
//      re-toggle (a one-shot hysteresis shift that cannot re-trigger is kept
//      only when it is the single above-noise move), and
//   4. RESTORES every component's original value/selection (non-destructive).
// Returns an array of findings { kind, role, outlier, magnitude, siblingMedian }
// (kind is 'select' | 'tab' | 'radio' | 'menuitemradio' | 'button-cluster'), or
// []. Deterministic: keyed off DOM structure, not visible text or timing, and the
// thresholds match every other runner.
//
// `arg` is { settleMs, ratio, minMag, choiceRoles }. The thresholds are PASSED IN
// (not hard-coded here) so the one source of truth above governs the in-page pass
// too; a caller always forwards the module constants.
export async function choiceAnomalyInPage(arg) {
  const settleMs = (arg && arg.settleMs) || 600;
  const RATIO = (arg && arg.ratio) || 3;
  const MIN_MAG = (arg && arg.minMag) || 24;
  const CHOICE_ROLE_SET = new Set((arg && arg.choiceRoles) || ['tab', 'radio', 'menuitemradio']);

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const visible = (el) => {
    if (!el) return false;
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const nameOf = (el) => {
    let n = norm(el.getAttribute && el.getAttribute('aria-label'));
    if (!n && el.getAttribute) {
      const ll = el.getAttribute('aria-labelledby');
      if (ll) {
        const ref = document.getElementById(ll.split(/\s+/)[0]);
        if (ref) n = norm(ref.textContent);
      }
    }
    if (!n) n = norm(el.textContent);
    return n;
  };
  const measure = () => {
    const de = document.documentElement;
    const anchors = [];
    // PINNED chrome only (fixed, or sticky while actually stuck), in VIEWPORT
    // coords: pinned chrome must not move in the viewport regardless of the
    // active choice, and viewport position is scroll-invariant, so a choice that
    // legitimately swaps content of a different height (moving scrollY) never
    // registers. Unpinned sticky is ordinary flow content: skipped. Keyed by
    // tag + query index so the stuck-state filter cannot misalign comparisons.
    const els = document.querySelectorAll('header, nav, [role=banner], [role=navigation]');
    for (let i = 0; i < els.length; i++) {
      const el = els[i];
      const cs = getComputedStyle(el);
      if (cs.position !== 'fixed' && cs.position !== 'sticky') continue;
      const r = el.getBoundingClientRect();
      if (r.width <= 0) continue;
      if (cs.position === 'sticky') {
        const topPx = parseFloat(cs.top);
        if (!Number.isFinite(topPx) || Math.abs(r.top - topPx) > 1) continue;
      }
      anchors.push([el.tagName.toLowerCase() + ':' + i, Math.round(r.top), Math.round(r.left)]);
    }
    return {
      hOverflow: Math.max(0, de.scrollWidth - window.innerWidth),
      scrollH: de.scrollHeight,
      anchors,
    };
  };
  const delta = (base, cur) => {
    if (!base || !cur) return 0;
    let d = Math.abs((cur.hOverflow || 0) - (base.hOverflow || 0));
    d += Math.abs((cur.scrollH || 0) - (base.scrollH || 0));
    const cm = new Map();
    for (const a of cur.anchors || []) cm.set(a[0], a);
    for (const b of base.anchors || []) {
      const c = cm.get(b[0]);
      if (!c) continue;
      d += Math.abs(c[1] - b[1]) + Math.abs(c[2] - b[2]);
    }
    return d;
  };
  const median = (xs) => {
    if (!xs.length) return 0;
    const s = [...xs].sort((a, b) => a - b);
    const m = s.length >> 1;
    return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
  };
  const selectedState = (el) => {
    const a = (n) => (el.getAttribute(n) || '').toLowerCase();
    if (a('aria-pressed') === 'true' || a('aria-selected') === 'true') return true;
    if (a('aria-checked') === 'true' || el.getAttribute('aria-current') != null) return true;
    const ds = a('data-state');
    if (['active', 'selected', 'on', 'checked', 'open'].includes(ds)) return true;
    return false;
  };

  // Build the list of components to exercise. Each is { kind, role, options:[...],
  // restore: fn }. An option is { pick: fn }: calling it selects that option.
  const components = [];

  // 1) Native <select> (FEATURE 1). Set .value + dispatch change/input so a
  // framework bound to it reacts; restore the original value afterward.
  for (const sel of document.querySelectorAll('select')) {
    if (!visible(sel)) continue;
    const opts = Array.from(sel.options || []).filter((o) => !o.disabled);
    if (opts.length < 3) continue;
    const orig = sel.value;
    const options = opts.map((o) => ({
      label: norm(o.label || o.textContent) || o.value,
      pick: () => {
        sel.value = o.value;
        sel.dispatchEvent(new Event('input', { bubbles: true }));
        sel.dispatchEvent(new Event('change', { bubbles: true }));
      },
    }));
    components.push({
      kind: 'select',
      role: 'select',
      options,
      restore: () => {
        sel.value = orig;
        sel.dispatchEvent(new Event('input', { bubbles: true }));
        sel.dispatchEvent(new Event('change', { bubbles: true }));
      },
    });
  }

  // 2) ARIA choice groups (tab/radio/menuitemradio), scoped by owning container so
  // two independent groups never merge into one false comparison.
  const containerOf = (el) => {
    const cont =
      el.closest &&
      el.closest('[role=tablist],[role=radiogroup],[role=menu],[role=menubar],fieldset');
    if (cont) return cont;
    const tag = el.tagName ? el.tagName.toLowerCase() : '';
    if (tag === 'input' && (el.getAttribute('type') || '').toLowerCase() === 'radio') {
      const nm = el.getAttribute('name');
      if (nm) return 'name:' + nm;
    }
    return null;
  };
  const ariaRoleOf = (el) => {
    const ar = (el.getAttribute('role') || '').toLowerCase();
    if (CHOICE_ROLE_SET.has(ar)) return ar;
    const tag = el.tagName ? el.tagName.toLowerCase() : '';
    if (tag === 'input' && (el.getAttribute('type') || '').toLowerCase() === 'radio')
      return 'radio';
    return null;
  };
  const ariaGroups = new Map(); // key(container|role) -> { role, els:[] }
  let contId = 0;
  const contKey = new Map();
  let qList;
  try {
    qList = document.querySelectorAll(
      '[role=tab],[role=radio],[role=menuitemradio],input[type=radio]',
    );
  } catch (_) {
    qList = [];
  }
  for (const el of qList) {
    if (!visible(el)) continue;
    const role = ariaRoleOf(el);
    if (!role) continue;
    const cont = containerOf(el);
    let ckey;
    if (cont == null) ckey = 'role';
    else if (typeof cont === 'string') ckey = cont;
    else {
      if (!contKey.has(cont)) contKey.set(cont, 'c' + contId++);
      ckey = contKey.get(cont);
    }
    const key = role + '|' + ckey;
    if (!ariaGroups.has(key)) ariaGroups.set(key, { role, els: [] });
    ariaGroups.get(key).els.push(el);
  }
  for (const g of ariaGroups.values()) {
    if (g.els.length < 3) continue;
    const orig = g.els.find((e) => selectedState(e)) || null;
    components.push({
      kind: g.role,
      role: g.role,
      options: g.els.map((el) => ({
        label: nameOf(el),
        pick: () => {
          el.scrollIntoView({ block: 'center', inline: 'center' });
          el.click();
        },
      })),
      restore: () => {
        if (orig) {
          try {
            orig.click();
          } catch (_) {}
        }
      },
    });
  }

  // 3) Button-cluster pickers: >= 3 same-parent plain buttons with EXACTLY ONE
  // selected (one-of-N) -- that selected state separates a choice picker from a
  // row of action buttons (Save/Delete), so a Delete is never blindly clicked.
  const byParent = new Map();
  let btnList;
  try {
    btnList = document.querySelectorAll('button,[role=button]');
  } catch (_) {
    btnList = [];
  }
  for (const el of btnList) {
    if (!visible(el)) continue;
    if (el.closest && el.closest('[role=tablist],[role=radiogroup],[role=menu],[role=menubar]'))
      continue;
    if (!nameOf(el)) continue;
    const par = el.parentElement;
    if (!par) continue;
    if (!byParent.has(par)) byParent.set(par, []);
    byParent.get(par).push(el);
  }
  for (const els of byParent.values()) {
    if (els.length < 3) continue;
    if (els.filter((e) => selectedState(e)).length !== 1) continue;
    const orig = els.find((e) => selectedState(e)) || null;
    components.push({
      kind: 'button-cluster',
      role: 'button-cluster',
      options: els.map((el) => ({
        label: nameOf(el),
        pick: () => {
          el.scrollIntoView({ block: 'center', inline: 'center' });
          el.click();
        },
      })),
      restore: () => {
        if (orig) {
          try {
            orig.click();
          } catch (_) {}
        }
      },
    });
  }

  // Measure the layout AFTER IT SETTLES: sample until two consecutive
  // fingerprints match (or the cap hits). A choice whose layout effect lands
  // asynchronously settles PAST any fixed wait; with a fixed wait the late shift
  // lands in the NEXT option's window and the oracle blames the wrong sibling.
  const step = Math.min(settleMs, 300);
  const measureSettled = async () => {
    await sleep(step);
    let prev = measure();
    for (let waited = step; waited < settleMs * 4; waited += step) {
      await sleep(step);
      const cur = measure();
      if (delta(prev, cur) === 0) return cur;
      prev = cur;
    }
    return prev;
  };

  // Exercise every component, classify with the SHARED rule, restore state.
  const findings = [];
  for (const comp of components) {
    // FIRST PASS: select each option and capture its SETTLED ABSOLUTE layout
    // fingerprint -- a late-settling shift lands inside its own option's
    // settled state, and no baseline choice can hide or misattribute anything.
    const fps = [];
    for (const opt of comp.options) {
      try {
        opt.pick();
      } catch (_) {
        fps.push(null);
        continue;
      }
      fps.push(await measureSettled());
    }
    const validIdx = [];
    for (let i = 0; i < fps.length; i++) if (fps[i]) validIdx.push(i);
    if (validIdx.length < 3) {
      try {
        comp.restore();
      } catch (_) {}
      await sleep(50);
      continue;
    }
    // NORM: the MEDOID fingerprint (the option most like the others) is the
    // group's typical page geometry; each option's magnitude is its distance
    // from it. The pack defines the median deviation, so uniform pickers stay
    // quiet and only a genuine odd-one-out towers over the norm.
    let medoidI = validIdx[0];
    let bestSum = Infinity;
    for (const i of validIdx) {
      let s = 0;
      for (const j of validIdx) if (j !== i) s += delta(fps[i], fps[j]);
      if (s < bestSum) {
        bestSum = s;
        medoidI = i;
      }
    }
    const mag = {};
    for (const i of validIdx) mag[i] = delta(fps[medoidI], fps[i]);
    const sibMed = (ci) => median(validIdx.filter((i) => i !== ci).map((i) => mag[i]));
    const candIdx = validIdx
      .filter((i) => i !== medoidI && mag[i] >= MIN_MAG && mag[i] >= RATIO * Math.max(sibMed(i), 1))
      .sort((a, b) => mag[b] - mag[a]);
    // The contract is exactly one odd option. Several candidates mean the
    // component intentionally swaps differently sized content, so it is not a
    // choice anomaly.
    if (candIdx.length !== 1) {
      try {
        comp.restore();
      } catch (_) {}
      await sleep(50);
      continue;
    }
    // CAUSAL CONFIRMATION: park the group on the medoid, settle, then select
    // the candidate, settle -- the candidate owns a bug only if the deviation
    // FOLLOWS it in this isolated A/B pair.
    const hits = [];
    for (const ci of candIdx) {
      try {
        comp.options[medoidI].pick();
        const a = await measureSettled();
        comp.options[ci].pick();
        const b = await measureSettled();
        const m = a && b ? delta(a, b) : null;
        const med = sibMed(ci);
        if (m !== null && m >= MIN_MAG && m >= RATIO * Math.max(med, 1)) {
          hits.push({ i: ci, mag: m, med });
        }
      } catch (_) {}
    }
    // Restore the component to its starting state -- non-destructive, like the
    // rest of the oracle.
    try {
      comp.restore();
    } catch (_) {}
    await sleep(50);
    hits.sort((a, b) => b.mag - a.mag);
    for (const hit of hits) {
      findings.push({
        kind: comp.kind,
        role: comp.role,
        outlier: (comp.options[hit.i] && comp.options[hit.i].label) || '',
        magnitude: Math.round(hit.mag),
        siblingMedian: Math.round(hit.med),
      });
    }
  }
  return findings;
}

// Replay ONE already-confirmed choice anomaly for evidence video. Unlike the
// detector above, this does no page-wide discovery or measurement: it resolves
// the affected component from the recorded outlier label, keeps that component
// in view, selects each ordinary sibling once, then selects + tags the outlier
// last. The resulting clip shows the local comparison that makes the odd choice
// obvious without filming accessibility walks or unrelated page scans.
//
// Returns { ok, choices } so the caller can trust-gate the clip. Self-contained
// for page.evaluate; do not close over module helpers.
export async function replayChoiceComponentInPage(arg) {
  const label = String((arg && arg.label) || '');
  const settleMs = Math.max(0, Number((arg && arg.settleMs) || 450));
  if (!label) return { ok: false, choices: [] };

  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const nameOf = (el) => {
    if (!el) return '';
    let n = norm(el.getAttribute && el.getAttribute('aria-label'));
    if (!n && el.getAttribute) {
      const ll = el.getAttribute('aria-labelledby');
      if (ll) {
        const ref = document.getElementById(ll.split(/\s+/)[0]);
        if (ref) n = norm(ref.textContent);
      }
    }
    if (!n) n = norm(el.textContent);
    return n;
  };
  const visible = (el) => {
    if (!el) return false;
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) return false;
    const cs = getComputedStyle(el);
    return cs.display !== 'none' && cs.visibility !== 'hidden';
  };
  const inView = (el) => {
    const r = el.getBoundingClientRect();
    const vh = window.innerHeight || 800;
    const vw = window.innerWidth || 1280;
    return r.top >= 0 && r.left >= 0 && r.bottom <= vh && r.right <= vw;
  };

  // Native select: the finding label names an <option>, while the visual target
  // is its owning <select>. Exercise only that select and tag it at the end.
  for (const select of document.querySelectorAll('select')) {
    if (!visible(select)) continue;
    const options = [...(select.options || [])].filter((o) => !o.disabled);
    const target = options.find((o) => norm(o.label || o.textContent) === label);
    if (!target || options.length < 3) continue;
    if (!inView(select))
      select.scrollIntoView({ behavior: 'smooth', block: 'center', inline: 'nearest' });
    await sleep(250);
    const choose = async (opt) => {
      select.value = opt.value;
      select.dispatchEvent(new Event('input', { bubbles: true }));
      select.dispatchEvent(new Event('change', { bubbles: true }));
      await sleep(settleMs);
    };
    for (const opt of options) if (opt !== target) await choose(opt);
    await choose(target);
    for (const e of document.querySelectorAll('[data-reproit-trigger]'))
      e.removeAttribute('data-reproit-trigger');
    select.setAttribute('data-reproit-trigger', '1');
    return { ok: true, choices: options.map((o) => norm(o.label || o.textContent)) };
  }

  const all = [
    ...document.querySelectorAll(
      'button,[role=button],[role=tab],[role=radio],[role=menuitemradio]',
    ),
  ].filter(visible);
  const target = all.find((el) => nameOf(el) === label);
  if (!target) return { ok: false, choices: [] };

  // Prefer an explicit ARIA owner. Plain button-cluster pickers use same-parent
  // siblings, matching the detector's grouping rule.
  const owner = target.closest(
    '[role=tablist],[role=radiogroup],[role=menu],[role=menubar],fieldset',
  );
  let options;
  if (owner) {
    options = [
      ...owner.querySelectorAll(
        'button,[role=button],[role=tab],[role=radio],[role=menuitemradio]',
      ),
    ].filter(visible);
  } else {
    options = all.filter((el) => el.parentElement === target.parentElement);
  }
  if (options.length < 3 || !options.includes(target)) return { ok: false, choices: [] };

  if (!inView(target))
    target.scrollIntoView({ behavior: 'smooth', block: 'center', inline: 'nearest' });
  await sleep(250);
  // Ordinary choices establish the visual norm. The outlier is always last, so
  // the layout shift and final red box have an immediate before/after context.
  for (const option of options) {
    if (option === target) continue;
    option.click();
    await sleep(settleMs);
  }
  for (const e of document.querySelectorAll('[data-reproit-trigger]'))
    e.removeAttribute('data-reproit-trigger');
  target.click();
  target.setAttribute('data-reproit-trigger', '1');
  await sleep(settleMs);
  return { ok: true, choices: options.map(nameOf) };
}

// The in-page pass as a SOURCE STRING, for the WebDriver execute() body (tauri),
// which takes a function-body string, not a function reference. Built from the
// function above with `.toString()` so there is no separate copy to drift: the
// stringified body is the exact code unit-tested via page.evaluate.
export const CHOICE_ANOMALY_IN_PAGE_SRC = choiceAnomalyInPage.toString();
