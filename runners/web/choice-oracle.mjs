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

export const CHOICE_OUTLIER_RATIO = 3;   // outlier magnitude >= 3x the sibling median ...
export const CHOICE_MIN_MAGNITUDE = 24;  // ...and at least this many px of global move.
export const CHOICE_ROLES = ['tab', 'radio', 'menuitemradio'];

// Total px the global layout moved between two fingerprints: horizontal-overflow
// delta + anchor displacement (matched by index; persistent chrome is stable).
// Host-pure: same fingerprints in -> same number out, on every backend.
export function layoutDelta(base, cur) {
  if (!base || !cur) return 0;
  let d = Math.abs((cur.hOverflow || 0) - (base.hOverflow || 0));
  const ba = base.anchors || [];
  const ca = cur.anchors || [];
  const n = Math.min(ba.length, ca.length);
  for (let i = 0; i < n; i++) {
    d += Math.abs(ca[i][0] - ba[i][0]);
    d += Math.abs(ca[i][1] - ba[i][1]);
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

// Capture a GLOBAL-layout fingerprint: page horizontal overflow + the PAGE-
// ABSOLUTE positions of persistent chrome anchors OUTSIDE any one component, so
// a component resizing ITSELF (expected) does not register, only a real reflow of
// the rest of the page. Page-absolute coords mean scrolling between choices is not
// counted as a move. Runs in the page; takes the live `document`/`window`.
// Identical body to the web runner's measureGlobalLayout, kept here as the single
// source the electron page.evaluate and the tauri execute() string both use.
export function measureGlobalLayoutInPage() {
  const de = document.documentElement;
  const sx = window.scrollX || 0, sy = window.scrollY || 0;
  const anchors = [];
  for (const el of document.querySelectorAll(
    'header, h1, h2, footer, [role=banner], [role=contentinfo], [role=navigation]'
  )) {
    const r = el.getBoundingClientRect();
    if (r.width > 0) anchors.push([Math.round(r.top + sy), Math.round(r.left + sx)]);
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
//      frameworks react; click() for ARIA/button options), waiting `settleMs`
//      between options, measuring the global-layout fingerprint each time,
//   3. classifies the outlier with the SHARED threshold rule, and
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
      if (ll) { const ref = document.getElementById(ll.split(/\s+/)[0]); if (ref) n = norm(ref.textContent); }
    }
    if (!n) n = norm(el.textContent);
    return n;
  };
  const measure = () => {
    const de = document.documentElement;
    const sx = window.scrollX || 0, sy = window.scrollY || 0;
    const anchors = [];
    for (const el of document.querySelectorAll(
      'header, h1, h2, footer, [role=banner], [role=contentinfo], [role=navigation]'
    )) {
      const r = el.getBoundingClientRect();
      if (r.width > 0) anchors.push([Math.round(r.top + sy), Math.round(r.left + sx)]);
    }
    return { hOverflow: Math.max(0, de.scrollWidth - window.innerWidth), anchors };
  };
  const delta = (base, cur) => {
    if (!base || !cur) return 0;
    let d = Math.abs((cur.hOverflow || 0) - (base.hOverflow || 0));
    const n = Math.min(base.anchors.length, cur.anchors.length);
    for (let i = 0; i < n; i++) {
      d += Math.abs(cur.anchors[i][0] - base.anchors[i][0]);
      d += Math.abs(cur.anchors[i][1] - base.anchors[i][1]);
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
    const cont = el.closest && el.closest('[role=tablist],[role=radiogroup],[role=menu],[role=menubar],fieldset');
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
    if (tag === 'input' && (el.getAttribute('type') || '').toLowerCase() === 'radio') return 'radio';
    return null;
  };
  const ariaGroups = new Map(); // key(container|role) -> { role, els:[] }
  let contId = 0;
  const contKey = new Map();
  let qList;
  try {
    qList = document.querySelectorAll(
      '[role=tab],[role=radio],[role=menuitemradio],input[type=radio]'
    );
  } catch (_) { qList = []; }
  for (const el of qList) {
    if (!visible(el)) continue;
    const role = ariaRoleOf(el);
    if (!role) continue;
    const cont = containerOf(el);
    let ckey;
    if (cont == null) ckey = 'role';
    else if (typeof cont === 'string') ckey = cont;
    else { if (!contKey.has(cont)) contKey.set(cont, 'c' + contId++); ckey = contKey.get(cont); }
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
        pick: () => { el.scrollIntoView({ block: 'center', inline: 'center' }); el.click(); },
      })),
      restore: () => { if (orig) { try { orig.click(); } catch (_) {} } },
    });
  }

  // 3) Button-cluster pickers: >= 3 same-parent plain buttons with EXACTLY ONE
  // selected (one-of-N) -- that selected state separates a choice picker from a
  // row of action buttons (Save/Delete), so a Delete is never blindly clicked.
  const byParent = new Map();
  let btnList;
  try { btnList = document.querySelectorAll('button,[role=button]'); } catch (_) { btnList = []; }
  for (const el of btnList) {
    if (!visible(el)) continue;
    if (el.closest && el.closest('[role=tablist],[role=radiogroup],[role=menu],[role=menubar]')) continue;
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
        pick: () => { el.scrollIntoView({ block: 'center', inline: 'center' }); el.click(); },
      })),
      restore: () => { if (orig) { try { orig.click(); } catch (_) {} } },
    });
  }

  // Exercise every component, classify with the SHARED rule, restore state.
  const findings = [];
  for (const comp of components) {
    const mags = [];
    let base = null;
    for (const opt of comp.options) {
      try { opt.pick(); } catch (_) { mags.push(null); continue; }
      await sleep(settleMs);
      const cur = measure();
      if (!base) { base = cur; mags.push(0); continue; }
      mags.push(delta(base, cur));
    }
    // Restore the component to its starting state -- non-destructive, like the
    // rest of the oracle.
    try { comp.restore(); } catch (_) {}
    await sleep(50);

    const valid = mags.filter((m) => typeof m === 'number' && Number.isFinite(m));
    if (valid.length < 3) continue;
    let maxI = 0;
    for (let i = 1; i < mags.length; i++) {
      if (typeof mags[i] === 'number' && Number.isFinite(mags[i]) && mags[i] > (mags[maxI] || 0)) maxI = i;
    }
    const max = mags[maxI];
    const siblings = valid.filter((m) => m !== max);
    const med = median(siblings);
    if (!(max >= MIN_MAG && max >= RATIO * Math.max(med, 1))) continue;
    findings.push({
      kind: comp.kind,
      role: comp.role,
      outlier: (comp.options[maxI] && comp.options[maxI].label) || '',
      magnitude: Math.round(max),
      siblingMedian: Math.round(med),
    });
  }
  return findings;
}

// The in-page pass as a SOURCE STRING, for the WebDriver execute() body (tauri),
// which takes a function-body string, not a function reference. Built from the
// function above with `.toString()` so there is no separate copy to drift: the
// stringified body is the exact code unit-tested via page.evaluate.
export const CHOICE_ANOMALY_IN_PAGE_SRC = choiceAnomalyInPage.toString();
