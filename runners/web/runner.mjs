// ReproIt web runner: drives a browser with Playwright and emits the SAME
// marker protocol the Rust orchestrator already parses, so the entire
// map / graph / fuzz / soak / a11y / evidence pipeline works on web
// unchanged. The browser is to web what the Dart explorer is to Flutter:
// it walks the DOM and prints EXPLORE/FUZZ/FRAMES markers.
//
// Records (one JSON per line, parsed from stdout):
//   EXPLORE:STATE {"sig":..,"labels":[..],"elements":[{sel,role,label,nokey?}]}
//                 sig is STRUCTURAL + locale-invariant (roles + DOM tree shape +
//                 stable developer keys); labels are DISPLAY-ONLY visible text.
//   EXPLORE:EDGE  {"from":..,"action":"tap:<selector>"|"back","to":..}
//                 selector = "key:<kind>:<v>" (data-testid/name) or
//                 "role:<role>#<idx>" (aria role + structural index), never text.
//
// Invoked by the orchestrator's web runner with env:
//   REPROIT_URL          the app URL to explore
//   REPROIT_VIDEO_DIR    where to save the run video (optional)
//   REPROIT_FUZZ_CONFIG  path to fuzz config json (seed/budget/replay/prefix)
//   REPROIT_HEADLESS     "0" to show the browser (default headless)
//
// stdout is the marker stream; the orchestrator captures it like a drive log.

// playwright and pngjs are loaded lazily at their use sites: electron.mjs,
// tauri.mjs, and the signature parity test import this module for its pure
// helpers, and a top-level dependency import would make that impossible
// without a full npm install (the parity CI job has none).
import { readFileSync, existsSync, mkdirSync, appendFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
import {
  gridPoints, changedFraction, classifyPoint, probeRegionsToGroundtruth, DEFAULT_GRID,
} from './probe.mjs';
import { transientDivergence } from './flicker-oracle.mjs';
import {
  occlusionScan, confirmOcclusions, securityScan,
  dupSubmitEligible, focusLossArm, focusLossCheck,
  blankScreenScan, brokenAssetScan, zoomTappableKeys, zoomReflowScan,
  scrollRoundTripScan,
  installListenerLeakCounter, listenerLeakSample,
} from './hygiene-oracles.mjs';
import {
  CHOICE_OUTLIER_RATIO, CHOICE_MIN_MAGNITUDE, CHOICE_ROLES as CHOICE_ROLE_LIST,
  layoutDelta, medianOf, choiceAnomalyInPage, replayChoiceComponentInPage,
} from './choice-oracle.mjs';

const APP_URL = process.env.REPROIT_URL || "http://localhost:8080";
const APP_ORIGIN = (() => { try { return new URL(APP_URL).origin; } catch (e) { return ''; } })();
const VIDEO_DIR = process.env.REPROIT_VIDEO_DIR || undefined;
const NETWORK_FILE = process.env.REPROIT_NETWORK_FILE || undefined;
const NETWORK_ACTOR = process.env.REPROIT_DEVICE || 'a';
// 0 is the immutable bootstrap phase; user actions are 1-based. This keeps
// initial API/config traffic hermetic without conflating it with the first tap.
let causalActionIndex = 0;
let causalOrdinal = 0;

// First-party check for the exception oracle: an uncaught error is the app's
// bug only if its stack touches the app's own origin. Errors thrown ENTIRELY
// inside third-party scripts (analytics, ad SDKs, tracking pixels - which big
// sites load by the dozen) are NOT app bugs and must not be reported, or every
// fbevents.js / imasdk.googleapis.com throw becomes a false "crash" finding.
// Keep an error when any http(s) stack frame is on the app origin, OR when the
// stack has no resolvable http(s) frame at all (inline/eval/anonymous - could be
// app code; never drop on missing evidence). Drop only when EVERY http(s) frame
// is off-origin. Pure + exported for unit testing.
//
// NOTE on "any app frame keeps it": deliberate. A real app bug whose bundle is
// served from a sibling asset domain (BBC's `bundle.js` on `static.files.bbci.co.uk`
// with a `www.bbc.com` frame deeper) must stay. The origin shape of that is
// IDENTICAL to an analytics script the app self-hosts on its own CDN, so the
// origin filter cannot separate them - that case is handled by
// `exceptionThrownInTracker` below, which keys on the SCRIPT's identity, not its
// origin (the only signal that actually tells them apart).
export function exceptionIsFirstParty(stack, appOrigin) {
  if (!appOrigin) return true;
  const urls = String(stack || '').match(/https?:\/\/[^\s)'"]+/g) || [];
  if (urls.length === 0) return true; // no script evidence -> do not drop
  let sawOffOrigin = false;
  for (const u of urls) {
    let origin;
    try { origin = new URL(u).origin; } catch (e) { continue; }
    if (origin === appOrigin) return true; // a frame on the app -> first-party
    sawOffOrigin = true;
  }
  return !sawOffOrigin; // every frame off-origin -> third-party, drop
}

// A throw whose INNERMOST (top) frame is a well-known analytics / tag-manager /
// tracking / error-monitor script is not the app's bug even when the script is
// self-hosted on the app's OWN CDN (so the origin filter keeps it) - the stack's
// deeper frames are just the app code that loaded the SDK. We key on the script's
// IDENTITY by filename/host (Adobe `s_code.js`, GTM, GA, Facebook Pixel, Hotjar,
// Segment, Sentry/NewRelic, ...), a small set of stable industry conventions, and
// ONLY on the throwing frame, so an app that merely loads analytics is unaffected
// unless the throw is literally inside the vendor script. This is what the origin
// filter structurally cannot see: it removed the self-hosted `awshome_s_code.js`
// false crash a docs scan surfaced without touching a real same-CDN app bundle.
// Pure + exported for unit testing.
const TRACKER_SCRIPT_RE =
  /s_code\.js|adobedtm|\bat\.js\b|fbevents\.js|connect\.facebook\.net|googletagmanager|\/gtag(\/|\.js)|gtm\.js|google-analytics\.com|\/ga\.js|\/analytics\.js|ima3\.js|doubleclick\.net|adsbygoogle|hotjar\.com|static\.hotjar|cdn\.mixpanel|cdn\.segment\.com|clarity\.ms|\/clarity\.js|cdn\.optimizely|amplitude\.com|fullstory\.com|quantserve|scorecardresearch|chartbeat|js-agent\.newrelic\.com|nr-data\.net|browser\.sentry-cdn\.com|bugsnag/i;
export function exceptionThrownInTracker(stack) {
  const urls = String(stack || '').match(/https?:\/\/[^\s)'"]+/g) || [];
  if (!urls.length) return false;
  return TRACKER_SCRIPT_RE.test(urls[0]); // the innermost (throwing) frame only
}

// Non-deterministic / non-app exception classes that must not become a crash
// finding. A failed `fetch(...).json()` whose body was an HTML error page
// ("Unexpected token '<', \"<!DOCTYPE \"... is not valid JSON"), or a bare fetch
// rejection, is a NETWORK condition (a 4xx/5xx, a login redirect, an offline
// blip), not a deterministic UI bug: it depends on a server response, would not
// reproduce on replay, and so fails reproit's determinism bar. Only honored for a
// STACKLESS throw - a real app-code JSON.parse / fetch-handling bug carries an app
// stack frame and is kept by the first-party rule above. Pure + exported for tests.
const NONDET_ERROR_RE =
  /is not valid JSON|Unexpected end of JSON input|Failed to fetch|NetworkError when attempting to fetch|Load failed/i;
export function exceptionIsNonDeterministic(message, stack) {
  if (!NONDET_ERROR_RE.test(String(message || ''))) return false;
  return (String(stack || '').match(/https?:\/\//g) || []).length === 0;
}

// Known-benign browser-policy errors that are NOT app bugs and must not be
// reported as crashes: (1) a same-origin-policy SecurityError from first-party
// code reaching into a cross-origin iframe (ads, embeds) - it has a first-party
// or EMPTY stack, so the origin filter alone keeps it, but it is just the SOP
// doing its job; (2) the ResizeObserver loop notification, a benign layout-thrash
// warning the browser recovers from, suppressed by default in every error tracker.
// Matched by message because the signal is in the message, not the stack. Keep
// this list TIGHT - over-suppression hides real bugs. Pure + exported for tests.
const BENIGN_ERROR_RE =
  /Blocked a frame with origin|accessing a cross-origin frame|Permission denied to access property .* on cross-origin|ResizeObserver loop/i;
export function exceptionIsBenign(message) {
  return BENIGN_ERROR_RE.test(String(message || ''));
}
const HEADLESS = process.env.REPROIT_HEADLESS !== '0';
// Desired UI locale for the run, a BCP47 tag (e.g. "de", "ar", "pt-BR"). When
// set, the browser context is created with this locale so the page renders in
// that language (navigator.language/languages + Accept-Language), letting
// reproit fuzz the app in a chosen language. When unset the page renders in the
// browser default (today's behavior). Scoped to the run: it only lives for this
// context. It changes visible LABELS only, never the structural signature
// (which excludes text by construction).
const LOCALE = (process.env.REPROIT_LOCALE || '').trim();
// Browser engine to drive. The DOM a11y state tree is identical across engines,
// so the same authored test / state graph runs on all three. Driving more than
// one engine is how cross-engine bugs (a layout/animation that breaks in Gecko
// but not Blink, or vice-versa) get caught: same actions, divergent result.
const ENGINE = (process.env.REPROIT_ENGINE || 'chromium').toLowerCase();
async function launchBrowser(opts) {
  const pw = await import('playwright');
  const engines = { chromium: pw.chromium, firefox: pw.firefox, webkit: pw.webkit };
  return (engines[ENGINE] || pw.chromium).launch(opts);
}
// Universal framebuffer-probe floor (PIECE 2, docs/operability-graph.md). OPT-IN
// because it is SIDE-EFFECTING + coarse: it synthesizes clicks at a small grid
// and diffs screenshots to find operable regions with no a11y control (e.g. a
// canvas/WebGL hit area). Off unless REPROIT_PROBE=1. See probe.mjs.
const PROBE = process.env.REPROIT_PROBE === '1';

// `--header "Name: value"` passthrough (repeatable CLI flag, delivered as a JSON
// object env). Lets an agent / CI inject clearance or auth headers (a
// cf_clearance cookie, an Authorization bearer, a preview token) into the browser
// context so a WAF-fronted or authed target is reachable. Empty object when unset.
const EXTRA_HEADERS = (() => {
  try {
    const raw = (process.env.REPROIT_EXTRA_HEADERS || '').trim();
    if (!raw) return {};
    const o = JSON.parse(raw);
    return (o && typeof o === 'object' && !Array.isArray(o)) ? o : {};
  } catch (_) { return {}; }
})();
// A caller may override the User-Agent via `--header "User-Agent: ..."`.
const UA_OVERRIDE = (() => {
  for (const k of Object.keys(EXTRA_HEADERS)) {
    if (k.toLowerCase() === 'user-agent') return String(EXTRA_HEADERS[k]);
  }
  return '';
})();
// Stable, identifiable scanner token appended to the real browser User-Agent so a
// WAF operator can allowlist reproit by name while the page still renders as a
// normal Chromium (a fully-synthetic UA gets challenged harder).
const REPROIT_UA_TOKEN = 'ReproIt-Scanner/1 (+https://reproit.dev/bot)';

// Substitute ${VAR} from the environment. Journeys encode `secret:` fills as
// ${REPROIT_SECRET_<ACCT>_<FIELD>} placeholders so plaintext credentials never
// touch disk; the orchestrator injects the secrets as env. Unset vars expand to
// "" (a missing credential then just types blank, which the app rejects).
function expandEnv(s) {
  return String(s).replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (_, name) => process.env[name] || '');
}

// Count VISIBLE elements matching a journey finder, for `expect: count`. Runs in
// the page context (passed to page.evaluate). Supports the same key grammar as
// tap()/typeInto(); anything else is treated as a raw CSS selector.
function countMatching(finder) {
  const esc = (v) => (window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\]/g, '\\$&'));
  let sel = finder;
  if (finder.startsWith('key:')) {
    const body = finder.slice(4);
    const ci = body.indexOf(':');
    const kind = ci >= 0 ? body.slice(0, ci) : '';
    const val = ci >= 0 ? body.slice(ci + 1) : body;
    if (kind === 'testid') sel = '[data-testid="' + esc(val) + '"],[data-test-id="' + esc(val) + '"]';
    else if (kind === 'id') sel = '#' + esc(val);
    else if (kind === 'name') sel = '[name="' + esc(val) + '"]';
  }
  let els;
  try { els = document.querySelectorAll(sel); } catch (_) { return -1; }
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

// Tier-1 flicker oracle (persistent-anchor churn). A re-render flicker is a
// transition that tears down and rebuilds chrome that did NOT need to change:
// for a frame the header/nav/list vanish, then settle back to the same thing.
// The settled-frame visual oracle cannot see it (both endpoints are correct).
// We catch it deterministically from the DOM instead of from pixels: tag the
// persistent "anchors" before a transition, then after it settles check whether
// any anchor that is VISUALLY UNCHANGED (same key, text, box) was nonetheless
// REPLACED (its DOM node identity changed). A framework that reconciles
// (React/Vue/Svelte) preserves node identity for unchanged nodes, so it does
// not trip; only an innerHTML-wipe-and-rebuild does, which is the flicker bug.
// Anchors are keyed by a stable id/testid or a unique landmark/tag so the same
// logical element re-resolves across the transition; ambiguous (duplicated)
// keys are skipped to avoid false positives. Navigation resets window, so the
// stash is gone and we report nothing (a page load is not flicker). Pure DOM,
// no frame timing, so it reproduces across `check` repeats.
const ANCHOR_SEL =
  'header,nav,main,footer,aside,' +
  '[role=banner],[role=navigation],[role=main],[role=contentinfo],' +
  '[role=complementary],[role=region],[role=search],[role=listbox],' +
  '[role=list],[role=tablist],[role=toolbar],[role=dialog],[id]';

// Clear a page's client-side persistence between seeds: localStorage,
// sessionStorage, and any IndexedDB databases, plus an app-provided
// window.__reproitReset() hook if one exists (a server-backed / custom reset stays
// compatible). Re-navigating alone does NOT reset a state-persisting app (a
// TodoMVC-style list kept in localStorage survives a reload), so a later seed would
// inherit an earlier seed's state and a kept repro would diverge on its own
// re-check. Best-effort throughout (a blocked IndexedDB delete never hangs the
// reset). Exported so resetToRoot and its test share one implementation.
export async function clearClientStorage(page) {
  await page.evaluate(async () => {
    try { if (typeof window.__reproitReset === 'function') await window.__reproitReset(); } catch (_) {}
    try { localStorage.clear(); } catch (_) {}
    try { sessionStorage.clear(); } catch (_) {}
    try {
      if (window.indexedDB && typeof indexedDB.databases === 'function') {
        const dbs = await indexedDB.databases();
        await Promise.all((dbs || []).map((d) => (d && d.name)
          ? new Promise((res) => {
              let done = false; const fin = () => { if (!done) { done = true; res(); } };
              const req = indexedDB.deleteDatabase(d.name);
              req.onsuccess = fin; req.onerror = fin; req.onblocked = fin;
              setTimeout(fin, 500); // never hang the reset on a blocked delete
            })
          : Promise.resolve()));
      }
    } catch (_) {}
  }).catch(() => {});
}

// shared by markAnchors/churnedAnchors; inlined into each (page.evaluate
// serializes a single function, so they cannot close over module scope).
function markAnchors(sel) {
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const keyOf = (el) => {
    const id = (el.getAttribute('id') || '').trim();
    if (id) return 'id:' + id;
    const tid = (el.getAttribute('data-testid') || el.getAttribute('data-test-id') || '').trim();
    if (tid) return 'testid:' + tid;
    const role = (el.getAttribute('role') || '').trim();
    return 'tag:' + el.tagName.toLowerCase() + (role ? '[' + role + ']' : '');
  };
  const anchors = [];
  for (const el of document.querySelectorAll(sel)) {
    if (!visible(el)) continue;
    const r = el.getBoundingClientRect();
    anchors.push({
      key: keyOf(el), node: el,
      text: (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 256),
      x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height),
    });
  }
  window.__reproitAnchors = anchors;
  window.__reproitAnchorDoc = document;
  return anchors.length;
}

function churnedAnchors(sel) {
  const old = window.__reproitAnchors;
  // No mark, or the document was replaced (navigation): not a flicker candidate.
  if (!old || window.__reproitAnchorDoc !== document) { window.__reproitAnchors = null; return null; }
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const keyOf = (el) => {
    const id = (el.getAttribute('id') || '').trim();
    if (id) return 'id:' + id;
    const tid = (el.getAttribute('data-testid') || el.getAttribute('data-test-id') || '').trim();
    if (tid) return 'testid:' + tid;
    const role = (el.getAttribute('role') || '').trim();
    return 'tag:' + el.tagName.toLowerCase() + (role ? '[' + role + ']' : '');
  };
  const cur = new Map();
  const dup = new Set();
  for (const el of document.querySelectorAll(sel)) {
    if (!visible(el)) continue;
    const k = keyOf(el);
    if (cur.has(k)) { dup.add(k); continue; }
    cur.set(k, el);
  }
  const churned = [];
  for (const a of old) {
    if (dup.has(a.key)) continue;        // ambiguous key -> skip
    const now = cur.get(a.key);
    if (!now) continue;                  // gone in the new state -> a real removal, not flicker
    if (now === a.node) continue;        // same node survived -> reconciled, no churn (good)
    const r = now.getBoundingClientRect();
    const sameBox =
      Math.round(r.x) === a.x && Math.round(r.y) === a.y &&
      Math.round(r.width) === a.w && Math.round(r.height) === a.h;
    const sameText = (now.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 256) === a.text;
    if (sameBox && sameText) churned.push(a.key); // unchanged yet rebuilt = flicker
  }
  window.__reproitAnchors = null;
  return churned;
}

// CONTENT-BUG oracle (deterministic, DOM/label-based). A rendered label that is
// clearly broken CONTENT: a literal artifact a stringify/template bug leaks to the
// screen, matched on STRUCTURE (a literal token), never a pixel or timing read, so
// the same DOM yields the same finding byte-for-byte on every run and on replay.
// Only two GROUND-TRUTH artifacts fire, both impossible to render as legitimate
// copy:
//   - [object Object]   : an object coerced to a string label (the canonical bug)
//   - {{ ... }} / ${ }  : an unrendered template placeholder (the binding never ran)
// The bare words `undefined`/`null`/`NaN` are NOT matched: they occur in real copy
// ("undefined behavior", a "Null Island" map pin, a "NaN" glossary entry) and in
// code samples, so keying on them false-positived on legitimate content. We fire
// only when the template binding itself or the object-coercion literal survived
// into the DOM -- neither has a benign rendered form.
// We scan only the OWN text of keyed, visible elements (id/testid/name), so the
// finding is addressed by a stable, locale-invariant key (never the text itself),
// and a parent's text is not double-counted against every descendant. Text inside
// a CODE context (<code>/<pre>/<script>/<style>/<textarea>/[contenteditable]) is
// SKIPPED: those legitimately display template/markup syntax (docs, code samples),
// so a `{{ user.name }}` shown as documentation is not a leaked binding.
// Empty/whitespace labels are NOT flagged here (that is an a11y/semantics concern,
// handled elsewhere); this oracle is strictly about VISIBLE broken content. Clean
// apps render neither token, so the control stays silent (no marker, no finding).
function detectContentBugs(injectedValues) {
  // Fuzzer provenance: a value reproit's own fuzzer TYPED into the app this run,
  // reflected back into a label, is not the app's broken content -- it is our probe
  // echoed (the XSS/template-injection probe `"><img src=x onerror=alert(1)>{{7*7}}`
  // reflected into a <strong> was a false positive). Mirror brokenAssetScan: skip a
  // label whose text contains, or is contained by, a non-trivial injected value.
  const injected = (Array.isArray(injectedValues) ? injectedValues : [])
    .map((v) => String(v == null ? '' : v).toLowerCase())
    .filter((v) => v.length > 0);
  const fromFuzzInjection = (text) => {
    const n = String(text || '').toLowerCase();
    if (!n) return false;
    // Direct: the whole label is fuzzer-provenanced (either containment direction).
    if (injected.some((v) => n.indexOf(v) !== -1 || (v.length >= 3 && v.indexOf(n) !== -1))) return true;
    // Fragmented: when the browser PARSES a reflected probe (e.g.
    // `"><img src=x onerror=alert(1)>{{7*7}}`), the `<img>` markup is stripped from
    // the visible text, leaving a fragment that is not a contiguous substring of the
    // raw injected value. So also check the specific ARTIFACT tokens that trigger a
    // finding -- a `{{...}}`/`${...}` binding, or the object-coercion literal -- for
    // fuzzer provenance (the probe that produced them was typed by us).
    const arts = [];
    const tm = n.match(/\{\{[^}]*\}\}/g); if (tm) arts.push(...tm);
    const dm = n.match(/\$\{[^}]*\}/g); if (dm) arts.push(...dm);
    if (n.indexOf('[object object]') !== -1) arts.push('[object object]');
    return arts.some((a) => injected.some((v) => v.indexOf(a) !== -1));
  };
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  // A CODE context legitimately shows template/markup syntax as literal text
  // (documentation, a code sample, an editable field), so its text is never a
  // leaked binding. True if the element or any ancestor is a code container or is
  // contenteditable.
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
  // The OWN (non-descendant) trimmed text of an element: only text directly under
  // it, so a container's text isn't attributed to it via its children.
  const ownText = (el) => {
    let t = '';
    for (const c of el.childNodes) if (c.nodeType === 3) t += c.textContent;
    return t.replace(/\s+/g, ' ').trim();
  };
  // The artifact classifiers. Each returns a stable reason tag or null. Order is
  // fixed and the first match wins, so a label can only carry one reason.
  // Shared PROSE GUARD: a real leaked artifact IS the label (a bare token, or a
  // short field-name prefix like "Price: X"). Documentation PROSE that merely
  // MENTIONS the token -- "The rendered result will be [object Object] because...",
  // "As with transitions... the double {{ }} syntax" -- has natural-language words
  // around it. Fire only when, with the artifact(s) removed, the remainder is a
  // SHORT label with no sentence structure. This kills the docs-site FP for BOTH
  // the object-coercion literal AND the template-brace token (every templating
  // framework's docs shows `{{ }}` in prose).
  const dominates = (stripped) => stripped.length <= 24 && !/[.!?]/.test(stripped);
  const reasonOf = (text) => {
    if (!text) return null;
    if (text.includes('[object Object]')) {
      const s = text.replace(/\[object Object\]/g, ' ').replace(/\s+/g, ' ').trim();
      if (dominates(s)) return 'object-object';
      // else prose mention -- fall through to the template check below.
    }
    // An unrendered template placeholder: a `{{ expr }}` or `${ expr }` survived
    // into the DOM (the binding engine never evaluated it), gated by the prose guard.
    if (/\{\{[^}]*\}\}/.test(text) || /\$\{[^}]*\}/.test(text)) {
      const s = text.replace(/\{\{[^}]*\}\}/g, ' ').replace(/\$\{[^}]*\}/g, ' ').replace(/\s+/g, ' ').trim();
      if (dominates(s)) return 'unrendered-template';
    }
    return null;
  };
  const out = [];
  const seen = new Set();
  const all = document.body ? document.body.querySelectorAll('*') : [];
  // Document-order index per tag, so an UNKEYED element still gets a stable,
  // distinct positional key (`tag:<tag>#<idx>`) -- a plain `<span>[object
  // Object]</span>` with no id/testid was silently skipped before, missing a
  // whole common class of broken-render artifacts. Same grammar as the overflow
  // oracle's tag fallback; the index keeps two unkeyed artifacts from colliding.
  const tagIdx = {};
  for (const el of all) {
    if (!visible(el)) continue;
    if (inCodeContext(el)) continue;
    const tag = el.tagName.toLowerCase();
    const n = tagIdx[tag] || 0;
    tagIdx[tag] = n + 1;
    const key = keyOf(el) || 'tag:' + tag + '#' + n;
    const text = ownText(el);
    const reason = reasonOf(text);
    if (!reason) continue;
    // Reflected fuzzer probe, not the app's own content -> not a bug.
    if (fromFuzzInjection(text)) continue;
    const dedup = key + '|' + reason;
    if (seen.has(dedup)) continue;
    seen.add(dedup);
    // Clip the offending text so the marker stays bounded; the reason+key are the
    // stable identity, the text is human detail.
    out.push({ key, reason, text: text.slice(0, 80) });
  }
  // Stable order: by key then reason, so the marker is byte-identical run to run.
  out.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : (a.reason < b.reason ? -1 : a.reason > b.reason ? 1 : 0)));
  return out;
}

// JANK / HANG watchdog (deterministic, recorded-trace based). The wall-clock
// DURATION of a synchronous handler flakes near any threshold, so we do NOT
// sample it: we key off the browser's own Long Tasks trace. A `longtask`
// PerformanceObserver entry is emitted for any task that blocks the main thread
// > 50ms; the observer buffers entries and delivers them once the blocking task
// finishes, so an action that ran a long synchronous stall leaves exactly one
// (or more) longtask entries we can read AFTER the action returns. A clean
// handler runs in well under 50ms and leaves ZERO entries. We classify by the
// MAX blocked duration, bucketed into coarse, well-separated floors so timing
// jitter can never flip the verdict:
//   - >= HANG_FLOOR_MS  -> a freeze (the app stopped making progress)
//   - >= JANK_FLOOR_MS  -> jank (a dropped-frame stall)
//   - else              -> nothing (a clean action)
// The floors are far from the fixtures (a 600ms stall vs a 3500ms freeze) so the
// classification is discrete: 600ms is always >= 200 and < 2000 (jank), 3500ms is
// always >= 2000 (hang). The marker carries the BUCKET, not the raw ms, so even
// the detail is reproducible; the finding id is the action-trace hash, which is
// already deterministic for a fixed seed.
const JANK_FLOOR_MS = 200;
const HANG_FLOOR_MS = 2000;
// Deterministic (machine-invariant) jank floor: an action forcing this many
// synchronous layouts is thrashing (repeated read-after-write reflow). The COUNT
// does not depend on machine speed, so -- unlike the ms floors above -- this
// verdict reproduces identically on any runner. Clean actions force ~0-1
// layouts; a thrash loop forces dozens to hundreds (measured: 300 for a 300-iter
// forced-reflow loop, 1 for a clean DOM write, 0 for a pure-compute loop).
const JANK_LAYOUT_FLOOR = 50;
// Install the longtask observer once per page; it accumulates entries into a
// window-global the per-action probe drains. Best-effort: a browser without the
// Long Tasks API (firefox/webkit) simply records nothing, so jank/hang are a
// chromium-tier signal (stated honestly), never a false positive elsewhere.
async function installLongTaskObserver(page) {
  await page.addInitScript(() => {
    try {
      window.__reproitLongTasks = [];
      const obs = new PerformanceObserver((list) => {
        for (const e of list.getEntries()) window.__reproitLongTasks.push(Math.round(e.duration));
      });
      obs.observe({ entryTypes: ['longtask'] });
    } catch (_) { /* no Long Tasks API: jank/hang silent on this engine */ }
  }).catch(() => {});
}
// Drain the longtask buffer and return the classification for the action that
// just ran, or null when nothing crossed the jank floor. `kind` is 'hang' or
// 'jank'; `bucket` is the coarse blocked-time floor (deterministic detail).
async function drainJank(page) {
  const tasks = await page.evaluate(() => {
    const t = window.__reproitLongTasks || [];
    window.__reproitLongTasks = [];
    return t;
  }).catch(() => []);
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

// Read the cumulative forced-layout / style-recalc counters from the CDP
// Performance domain. Returns { layout, recalc } or null (non-chromium / no CDP).
async function readLayoutCounters(cdp) {
  if (!cdp) return null;
  try {
    const { metrics } = await cdp.send('Performance.getMetrics');
    const g = (n) => { const m = metrics.find((x) => x.name === n); return m ? m.value : 0; };
    return { layout: g('LayoutCount'), recalc: g('RecalcStyleCount') };
  } catch (_) { return null; }
}
// Classify the deterministic layout-thrash signal from two counter snapshots
// taken TIGHTLY around the action (before the tap, and right after it returns --
// BEFORE the settle wait), so only the handler's SYNCHRONOUS forced reflows are
// counted, not async animation frames over the settle window (whose count is
// machine-dependent). Returns { count } (machine-invariant forced layouts) or
// null. Async/rAF-scheduled thrash is left to the timing watchdog.
function layoutThrash(before, after) {
  if (!before || !after) return null;
  const dLayout = after.layout - before.layout;
  return dLayout >= JANK_LAYOUT_FLOOR ? { count: dLayout } : null;
}

// CROSS-ENGINE jank/hang fallback (deterministic, requestAnimationFrame based).
// The Long Tasks API above is CHROMIUM-ONLY: on firefox/webkit the longtask
// observer records nothing, so jank/hang would be silent there. But reproit
// drives a cross-engine differential (chromium,firefox,webkit), so those engines
// ARE exercised and a Gecko/WebKit-only freeze must not go unseen. rAF works in
// all three: the browser fires the callback once per would-be paint, so the
// interval between two callbacks is how long the main thread blocked between two
// frames. A clean handler keeps frames near the vsync cadence (~16-33ms, or the
// browser's throttled headless rate); a synchronous stall shows up as ONE very
// long inter-frame interval bracketing the block, and a sustained stutter shows
// up as a RUN of long intervals.
//
// rAF timing is NOISIER than Long Tasks (a major GC, headless throttling, or a
// background-tab clamp can stretch a single frame to ~100-250ms with no app
// fault), so the classifier is deliberately conservative to stay FALSE-POSITIVE-
// FREE. We never flag a single mid-range late frame:
//   - HANG: a single interval >= HANG_FLOOR_MS (2000ms). Nothing benign blocks
//     paint for two whole seconds; the freeze fixture stalls 3500ms.
//   - JANK: EITHER a LONE long frame >= RAF_JANK_LONE_MS (a stall far above any
//     GC/scheduling blip; the jank fixture stalls 600ms), OR a SUSTAINED RUN of
//     >= RAF_JANK_RUN_MIN consecutive long (>= RAF_FRAME_MS) frames whose summed
//     blocked time reaches JANK_FLOOR_MS. A single GC pause is one sub-lone-floor
//     frame, so it is NEITHER a lone-jank nor a run: it is dropped.
// The EMITTED bucket is the SAME reused JANK_FLOOR_MS / HANG_FLOOR_MS constant as
// the Long Tasks path, so the marker is byte-identical across paths. `count` is
// the number of distinct stall EVENTS (runs), not raw frames: a 600ms block is
// one stall regardless of how rAF chopped it, so the detail is reproducible even
// though the raw intervals are not. The fixtures (600ms / 3500ms) sit far from
// the floors, so the verdict is discrete and a same-seed replay reproduces it.
const RAF_FRAME_MS = 100;       // an inter-frame interval this long is a "long frame"
const RAF_JANK_RUN_MIN = 2;     // a sustained jank run needs >= this many long frames
const RAF_JANK_LONE_MS = 350;   // a single frame this long is jank on its own (> GC noise, < the 600ms fixture)

// Pure classifier over a list of inter-frame intervals (ms). Deterministic: the
// SAME interval list always yields the same verdict. Exported for unit tests.
// Returns { kind, bucket, count } or null (clean). `count` = number of stall runs.
function classifyFrameIntervals(intervals) {
  if (!intervals || !intervals.length) return null;
  // A HANG is any single frame that blocked paint past the hang floor. Counted as
  // distinct events so the detail is stable.
  let hangRuns = 0;
  for (const iv of intervals) if (iv >= HANG_FLOOR_MS) hangRuns++;
  if (hangRuns > 0) return { kind: 'hang', bucket: HANG_FLOOR_MS, count: hangRuns };
  // Group consecutive long frames into runs; a run is jank if it is a LONE frame
  // past the lone floor, or a sustained run (>= RAF_JANK_RUN_MIN frames) whose
  // total blocked time reaches the jank floor. A single sub-lone-floor frame
  // (a GC blip) forms a length-1 run that meets neither test -> not jank.
  let jankRuns = 0;
  let i = 0;
  const n = intervals.length;
  while (i < n) {
    if (intervals[i] < RAF_FRAME_MS) { i++; continue; }
    let j = i;
    let total = 0;
    let peak = 0;
    while (j < n && intervals[j] >= RAF_FRAME_MS) {
      total += intervals[j];
      if (intervals[j] > peak) peak = intervals[j];
      j++;
    }
    const runLen = j - i;
    const lone = peak >= RAF_JANK_LONE_MS;
    const sustained = runLen >= RAF_JANK_RUN_MIN && total >= JANK_FLOOR_MS;
    if (lone || sustained) jankRuns++;
    i = j;
  }
  if (jankRuns > 0) return { kind: 'jank', bucket: JANK_FLOOR_MS, count: jankRuns };
  return null;
}

// Install the rAF frame-interval recorder once per page, alongside the longtask
// observer. It runs a self-perpetuating requestAnimationFrame loop that appends
// each inter-frame interval to a window-global the per-action probe drains.
// Works in all three engines (rAF is universal), so it is the cross-engine
// jank/hang path. Cheap (one timestamp per frame) and side-effect-free.
async function installFrameObserver(page) {
  await page.addInitScript(() => {
    try {
      window.__reproitFrameIntervals = [];
      let last = -1;
      const tick = (now) => {
        if (last >= 0) {
          const d = now - last;
          // Cap the buffer so a long idle stretch cannot grow it unbounded; the
          // per-action window is short, so this never trims a real stall.
          const buf = window.__reproitFrameIntervals;
          if (buf.length < 4096) buf.push(Math.round(d));
        }
        last = now;
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    } catch (_) { /* no rAF: cross-engine jank/hang silent (never a false positive) */ }
  }).catch(() => {});
}
// Drain the rAF interval buffer and classify it. Returns the SAME shape as
// drainJank ({ kind, bucket, count }) or null. The cross-engine path.
async function drainFrameJank(page) {
  const intervals = await page.evaluate(() => {
    const t = window.__reproitFrameIntervals || [];
    window.__reproitFrameIntervals = [];
    return t;
  }).catch(() => []);
  return classifyFrameIntervals(intervals);
}
// Per-action jank/hang verdict, engine-aware. On chromium we keep the PRECISE
// Long Tasks path UNCHANGED (it is more accurate than rAF); the rAF path is the
// cross-engine fallback used on firefox/webkit, where Long Tasks is unavailable.
// This keeps chromium byte-for-byte identical (no rAF can flip its verdict) while
// closing the silence on the other two engines.
async function drainJankForEngine(page) {
  if (ENGINE === 'chromium') return drainJank(page);
  return drainFrameJank(page);
}

// LEAK sampler (deterministic, web heap). `--soak` replays a reversible cycle N
// times and reads the heap slope; the Rust soak oracle flags growth that scales
// with the cycle count. The web runner has no Dart VM service, so we read the v8
// heap directly. PRECISION MATTERS HERE: `performance.memory.usedJSHeapSize` is
// QUANTIZED by Chromium to a coarse bucket (it pins to a rounded value like 10MB
// and barely moves) to defeat fingerprinting, so it CANNOT see a multi-MB leak
// and is useless for this. The CDP `Runtime.getHeapUsage` reports the REAL,
// unrounded v8 used-heap size, so we use that when a CDP session is available
// (chromium) and force a GC first (`HeapProfiler.collectGarbage`) so the reading
// is the RETAINED (live) heap, not transient garbage: a true leak survives GC and
// grows monotonically, while a resource-neutral cycle collapses back flat. We emit
// a MEMORY:SAMPLE marker per cycle; the soak side reconstructs the series from
// these when no VM-service memory file exists. CHROMIUM-ONLY by design: the
// precise heap needs the CDP `Runtime.getHeapUsage` domain. There is deliberately
// NO `performance.memory` fallback -- it is quantized to a coarse ~10MB bucket
// (anti-fingerprinting) so it cannot see a multi-MB leak; emitting it would feed
// the slope a leak-blind number, which docs/oracles.md rightly calls worse than
// silence. Off Chromium the leak oracle is an honest `gap` (no sample emitted).
async function sampleHeap(page, cdp, tMs) {
  let used = null;
  if (cdp) {
    try {
      // Force a GC so the reading reflects RETAINED memory, then read the precise
      // v8 used-heap size. Both are CDP domains available without page changes.
      await cdp.send('HeapProfiler.collectGarbage').catch(() => {});
      const r = await cdp.send('Runtime.getHeapUsage');
      if (r && typeof r.usedSize === 'number') used = Math.round(r.usedSize);
    } catch (_) { used = null; }
  }
  if (used == null) return;
  // DETERMINISTIC leak signal alongside the bytes: the live DOM element count.
  // Heap bytes are allocator/machine-dependent, but the node count over identical
  // cycles is an integer that reproduces on any runner, so monotonic node growth
  // is a machine-invariant leak verdict. Counted AFTER the forced GC above.
  const domNodes = await page.evaluate(() => document.getElementsByTagName('*').length).catch(() => null);
  log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: used, ...(domNodes != null ? { dom_nodes: domNodes } : {}) }));
}

const ACTION_BUDGET = 36;
// Zero-config map mode used to be unbounded and relied on the host's 300s kill.
// A deterministic work bound makes the same app produce the same explored prefix
// regardless of machine speed. Exhaustion is reported as bounded/truncated but
// the runner completes normally, leaving the observed map usable.
const MAP_ACTION_BUDGET = Math.max(1, Number(process.env.REPROIT_MAP_ACTION_BUDGET) || 72);
const MAX_LABEL_LEN = 40;
// Layer-1 value-class cap (docs/signature.md "Value-state"): once a structural
// node has shown more than this many DISTINCT value-class combinations, the
// runner drops it to structural-only so an adversarial value generator cannot
// explode the graph. The oracle is stateless; the cap is purely runner-local.
const VALUE_CLASS_CAP = 8;

// Layer-3 opt-in (docs/signature.md "Value-state"): read `value_nodes:`
// selectors from reproit.yaml. We avoid adding a YAML dependency: the block is
// a simple flat list of strings, so a tiny line parser is enough and keeps the
// runner dependency-free. Path precedence: REPROIT_CONFIG env, else
// ./reproit.yaml in the cwd. A missing/unparseable file yields an empty list,
// so value-state is strictly opt-in.
function loadValueNodes() {
  let p = (process.env.REPROIT_CONFIG || '').trim();
  if (!p) { const def = resolve(process.cwd(), 'reproit.yaml'); if (existsSync(def)) p = def; }
  if (!p || !existsSync(p)) return [];
  let text = '';
  try { text = readFileSync(p, 'utf8'); } catch { return []; }
  return parseValueNodes(text);
}
// Extract the `value_nodes:` list items from a YAML document. Supports the two
// shapes the spec shows: a block sequence (`value_nodes:` then `  - sel` lines)
// and an inline flow sequence (`value_nodes: [a, b]`). Comments and quotes are
// stripped. This is intentionally minimal: only the value_nodes key is read.
function parseValueNodes(text) {
  const lines = text.split(/\r?\n/);
  const out = [];
  const clean = (s) => {
    let v = s.trim();
    const h = v.indexOf('#'); if (h >= 0) v = v.slice(0, h).trim();
    if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) v = v.slice(1, -1);
    return v.trim();
  };
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(/^(\s*)value_nodes\s*:(.*)$/);
    if (!m) continue;
    const indent = m[1].length;
    const inline = m[2].trim();
    if (inline.startsWith('[')) {
      // inline flow sequence: value_nodes: [a, b, c]
      const body = inline.replace(/^\[/, '').replace(/\].*$/, '');
      for (const part of body.split(',')) { const v = clean(part); if (v) out.push(v); }
      return out;
    }
    // block sequence: subsequent more-indented `- item` lines.
    for (let j = i + 1; j < lines.length; j++) {
      const raw = lines[j];
      if (!raw.trim() || raw.trim().startsWith('#')) continue;
      const childIndent = raw.length - raw.replace(/^\s*/, '').length;
      if (childIndent <= indent) break; // dedented: block ended
      const item = raw.trim();
      if (!item.startsWith('-')) break; // not a sequence item
      const v = clean(item.slice(1));
      if (v) out.push(v);
    }
    return out;
  }
  return out;
}

// Adversarial text inputs for fuzzing text fields. Covers empty, an overlong
// string (length/overflow handling), emoji + RTL/unicode (encoding + bidi),
// an injection-ish payload (escaping), and a plain value (happy path). The
// pick is DETERMINISTIC: derived from the fuzz seed (no Math.random), so a run
// is reproducible and a replay reproduces the exact value. Each entry has an
// `id` so the chosen value is encoded into the action/edge ("type:<sel>=<id>")
// and a replay reconstructs the same text by id, not by re-rolling the seed.
const ADVERSARIAL = [
  { id: 'empty', value: '' },
  { id: 'long', value: 'A'.repeat(512) },
  { id: 'emoji', value: '🙂🚀✨🧪🔥' },
  { id: 'rtl', value: 'مرحبا שלום ‮abc‬' },
  { id: 'inject', value: '"><img src=x onerror=alert(1)>{{7*7}}' },
  { id: 'normal', value: 'Buy milk' },
];
const ADVERSARIAL_BY_ID = Object.fromEntries(ADVERSARIAL.map((a) => [a.id, a.value]));

// Map a non-negative integer (derived from the seeded rng) to an adversarial
// entry, deterministically. Same input -> same entry on every run.
function adversarialFor(n) {
  const i = ((n % ADVERSARIAL.length) + ADVERSARIAL.length) % ADVERSARIAL.length;
  return ADVERSARIAL[i];
}

// Property-matched replay (fixture inputs). The fuzz config may carry an
// `inputs` array, each `{ field, value }`, written by the CLI's
// crate::fixture::synthesize from the cloud's fixtureSpec: a CONCRETE,
// property-matched value (a 312-char unicode name, an emoji, an empty / RTL
// field) reconstructed from production telemetry. When a `type:` action targets
// a field with a provided input value, we type THAT value instead of only the
// fixed adversarial-class token, so the data-dependent bug actually reproduces.
// The provided value is itself deterministic (synthesis uses no RNG), so this
// path is as reproducible as the adversarial-class path.
//
// Normalize the config's `inputs` into a flat [{field, value}] list. `field`
// is the field identifier, either a semantic key ("email") or a full structural
// selector ("key:id:email"). Entries with no usable field key are dropped.
// Tolerant of a missing/garbage array (returns []), so a config without
// `inputs` is unaffected.
function loadInputs(fuzz) {
  const arr = fuzz && Array.isArray(fuzz.inputs) ? fuzz.inputs : [];
  const out = [];
  for (const it of arr) {
    if (!it || typeof it !== 'object') continue;
    const field = typeof it.field === 'string' && it.field ? it.field : '';
    if (!field) continue;
    const value = it.value != null ? String(it.value) : '';
    out.push({ field, value });
  }
  return out;
}

// Resolve a `type:` selector to a provided input value, or null when no input
// matches. The fixture `field` is a semantic identifier (e.g. "name"); the
// runner's selectors are structural (`key:<kind>:<v>` or `role:<role>#<idx>`).
// A field matches when it equals the full selector OR the key VALUE of a
// `key:<kind>:<v>` selector (so `field:"name"` matches `key:id:name`,
// `key:name:name`, or `key:testid:name`). First matching entry wins (config
// order). Empty `inputs` -> null (the adversarial-class path is untouched).
function inputValueFor(sel, inputs) {
  if (!inputs || !inputs.length || !sel) return null;
  let keyVal = null;
  if (sel.startsWith('key:')) {
    const body = sel.slice(4);
    const ci = body.indexOf(':');
    keyVal = ci >= 0 ? body.slice(ci + 1) : body;
  }
  for (const inp of inputs) {
    if (inp.field === sel || (keyVal != null && inp.field === keyVal)) return inp.value;
  }
  return null;
}

function log(line) {
  if (String(line).startsWith('FUZZ:ACT ')) {
    causalActionIndex++;
    causalOrdinal = 0;
  }
  process.stdout.write(line + '\n');
}

const SECRET_FIELD_RE = /password|passwd|secret|token|authorization|cookie|email|phone/i;
export function redactNetworkValue(value) {
  if (Array.isArray(value)) return value.map(redactNetworkValue);
  if (value && typeof value === 'object') {
    const out = {};
    for (const key of Object.keys(value).sort()) {
      const child = value[key];
      out[key] = SECRET_FIELD_RE.test(key)
        ? `<reproit:${typeof child === 'string' ? `string:length=${[...child].length}` : typeof child}>`
        : redactNetworkValue(child);
    }
    return out;
  }
  return value;
}
export function redactNetworkHeaders(headers) {
  const out = {};
  for (const key of Object.keys(headers || {}).sort()) {
    out[key] = SECRET_FIELD_RE.test(key) ? '<reproit:secret>' : String(headers[key]);
  }
  return out;
}
export function parseNetworkBody(raw, contentType = '') {
  if (raw == null || raw === '') return undefined;
  if (/json/i.test(contentType)) {
    try { return redactNetworkValue(JSON.parse(raw)); } catch (_) { return '<reproit:invalid-json>'; }
  }
  // Persist structure, not arbitrary production content. Exact binary/text
  // bodies require an explicit future project policy and capability.
  return `<reproit:body:length=${Buffer.byteLength(String(raw), 'utf8')}>`;
}
function appendNetworkFact(fact) {
  if (!NETWORK_FILE) return;
  try { appendFileSync(NETWORK_FILE, JSON.stringify(fact) + '\n', { encoding: 'utf8', mode: 0o600 }); } catch (_) {}
}

function canonicalNetworkUrl(raw) {
  try {
    const u = new URL(raw);
    const pairs = [...u.searchParams.entries()].sort(([ak, av], [bk, bv]) => ak.localeCompare(bk) || av.localeCompare(bv));
    u.search = '';
    for (const [k, v] of pairs) u.searchParams.append(k, v);
    return u.toString();
  } catch (_) { return String(raw); }
}

export async function installCapsuleReplay(context, path = process.env.REPROIT_CAPSULE) {
  if (!path) return;
  const capsule = JSON.parse(readFileSync(path, 'utf8'));
  const exchanges = (capsule.exchanges || []).filter((e) => e.required && /^(https?|sse)$/.test(e.protocol));
  const used = new Set();
  await context.route('**/*', async (route) => {
    const req = route.request();
    if (!['xhr', 'fetch', 'eventsource'].includes(req.resourceType())) return route.continue();
    const actionIndex = Math.max(causalActionIndex, 0);
    const wantedUrl = canonicalNetworkUrl(req.url());
    const idx = exchanges.findIndex((e, i) =>
      !used.has(i) && e.actor === NETWORK_ACTOR && e.actionIndex === actionIndex &&
      String(e.method).toUpperCase() === req.method().toUpperCase() && canonicalNetworkUrl(e.url) === wantedUrl
    );
    if (idx < 0) {
      log(`CAPSULE:MISS ${req.method()} ${req.url()} action=${actionIndex}`);
      return route.abort('blockedbyclient');
    }
    used.add(idx);
    const e = exchanges[idx];
    const headers = { ...(e.responseHeaders || {}) };
    let body = '';
    if (e.responseBody !== undefined) {
      body = typeof e.responseBody === 'string' ? e.responseBody : JSON.stringify(e.responseBody);
      if (typeof e.responseBody !== 'string' && !headers['content-type']) headers['content-type'] = 'application/json';
    }
    log(`CAPSULE:HIT ${e.id}`);
    return route.fulfill({ status: e.status, headers, body });
  });
  log(`CAPSULE:READY ${capsule.id || ''} exchanges=${exchanges.length}`);
}

function websocketFrameValue(message) {
  if (typeof message !== 'string') return { supported: false, value: `<reproit:body:length=${message.length}>` };
  try { return { supported: true, value: redactNetworkValue(JSON.parse(message)) }; }
  catch (_) { return { supported: false, value: `<reproit:body:length=${Buffer.byteLength(message, 'utf8')}>` }; }
}

function websocketReplayFrame(value) {
  return typeof value === 'string' ? value : JSON.stringify(value);
}

/** Ordered JSON WebSocket capture/replay. Non-JSON frames downgrade the
 * capability instead of persisting opaque user content or claiming replay. */
export async function installWebSocketCausal(context, path = process.env.REPROIT_CAPSULE) {
  let replay = [];
  if (path) {
    const capsule = JSON.parse(readFileSync(path, 'utf8'));
    replay = (capsule.exchanges || []).filter((e) => e.required && /^(ws|wss)$/.test(e.protocol));
  }
  const used = new Set();
  await context.routeWebSocket(/.*/, (socket) => {
    const url = socket.url();
    if (path) {
      const next = () => replay
        .map((exchange, index) => ({ exchange, index }))
        .filter(({ exchange, index }) => !used.has(index) && exchange.actor === NETWORK_ACTOR &&
          exchange.actionIndex === causalActionIndex && canonicalNetworkUrl(exchange.url) === canonicalNetworkUrl(url))
        .sort((a, b) => a.exchange.ordinal - b.exchange.ordinal)[0];
      const deliver = () => {
        for (;;) {
          const item = next();
          if (!item || item.exchange.method !== 'RECV') break;
          used.add(item.index);
          socket.send(websocketReplayFrame(item.exchange.responseBody));
          log(`CAPSULE:HIT ${item.exchange.id}`);
        }
      };
      queueMicrotask(deliver);
      socket.onMessage((message) => {
        const frame = websocketFrameValue(message);
        const item = next();
        if (!item || item.exchange.method !== 'SEND' ||
            JSON.stringify(item.exchange.requestBody) !== JSON.stringify(frame.value)) {
          log(`CAPSULE:MISS WS SEND ${url} action=${causalActionIndex}`);
          socket.close({ code: 1008, reason: 'reproit capsule miss' });
          return;
        }
        used.add(item.index);
        log(`CAPSULE:HIT ${item.exchange.id}`);
        deliver();
      });
      return;
    }

    const server = socket.connectToServer();
    const capture = (method, message, forward) => {
      const frame = websocketFrameValue(message);
      if (!frame.supported) {
        log('REPROIT:CAPABILITIES {"websocket":{"status":"unsupported","detail":"non-JSON frame cannot be safely persisted"},"websocket_replay":{"status":"unsupported","detail":"non-JSON frame cannot be safely persisted"}}');
        forward(message);
        return;
      }
      const ordinal = causalOrdinal++;
      appendNetworkFact({
        id: `${NETWORK_ACTOR}-${causalActionIndex}-${ordinal}`, actor: NETWORK_ACTOR,
        actionIndex: causalActionIndex, ordinal,
        protocol: new URL(url).protocol.replace(':', ''), method, url,
        requestHeaders: {}, requestBody: method === 'SEND' ? frame.value : undefined,
        status: 101, responseHeaders: {}, responseBody: method === 'RECV' ? frame.value : undefined,
        required: true,
      });
      forward(message);
    };
    socket.onMessage((message) => capture('SEND', message, (value) => server.send(value)));
    server.onMessage((message) => capture('RECV', message, (value) => socket.send(value)));
  });
  log('REPROIT:CAPABILITIES {"websocket":{"status":"captured"},"websocket_replay":{"status":"captured"},"sse":{"status":"captured"},"sse_replay":{"status":"captured"}}');
}

export function redactSse(raw) {
  let supported = true;
  const body = String(raw).split(/(\r?\n)/).map((line) => {
    if (!line.startsWith('data:')) return line;
    const prefix = line.match(/^data:\s*/)[0];
    try { return prefix + JSON.stringify(redactNetworkValue(JSON.parse(line.slice(prefix.length)))); }
    catch (_) { supported = false; return 'data:<reproit:unsupported-non-json>'; }
  }).join('');
  return { body, supported };
}

// Screenshot-capture contract (drive.rs): on a named "shoot" point, capture the
// current screen to $REPROIT_SHOTS_DIR/<name>.png, then print `SHOOT:<name>` so
// the orchestrator confirms the file and logs it. `name` is restricted to
// [A-Za-z0-9_/-] (the orchestrator filters to those anyway). If REPROIT_SHOTS_DIR
// is unset we skip the capture but STILL print the marker, so non-screenshot runs
// are unaffected. Playwright's page.screenshot writes the PNG directly.
async function shoot(page, name) {
  const dir = process.env.REPROIT_SHOTS_DIR;
  if (dir) {
    try {
      mkdirSync(dir, { recursive: true });
      await page.screenshot({ path: join(dir, name + '.png'), fullPage: false });
    } catch (e) { /* capture is best-effort; still emit the marker below */ }
  }
  log('SHOOT:' + name);
}

function loadFuzz() {
  const p = process.env.REPROIT_FUZZ_CONFIG;
  if (!p) return {};
  try { return JSON.parse(readFileSync(p, 'utf8')); } catch { return {}; }
}

// The list of per-seed fuzz configs to run in this session. Mirrors the other
// runners' batch contract (templates/explorer_headless.dart FuzzCfg.loadBatch,
// runners/rn, runners/linux-atspi.py load_batch): reproit's multi-seed fuzz
// writes {"batch":[ <cfg>, ... ]} where each <cfg> is the single-seed shape
// ({seed, budget, edgeWeights, prefix, replay, ...}). A single-seed run writes
// the bare {"seed":..} object with no "batch" key. Returns
// { seeds, isBatch } where isBatch is true ONLY for the multi-seed shape; the
// caller wraps each seed in SEED:BEGIN/SEED:END only when isBatch, so the
// single-seed path stays byte-for-byte identical (no SEED markers).
function loadBatch() {
  const j = loadFuzz();
  if (j && Array.isArray(j.batch) && j.batch.length) {
    return { seeds: j.batch.map((b) => (b && typeof b === 'object' ? b : {})), isBatch: true };
  }
  return { seeds: [j || {}], isBatch: false };
}

const FUZZ_CONFIGURED = !!process.env.REPROIT_FUZZ_CONFIG;

function edgeKey(sig, action) { return sig + '|' + action; }
function rememberActions(actionsByState, sig, actions) {
  const known = actionsByState.get(sig) || [];
  for (const action of actions) if (!known.includes(action)) known.push(action);
  actionsByState.set(sig, known);
}
function firstUntriedAction(actionsByState, tried, sig) {
  for (const action of actionsByState.get(sig) || []) {
    if (!tried.has(edgeKey(sig, action))) return action;
  }
  return null;
}
function hasFrontier(actionsByState, tried) {
  for (const sig of actionsByState.keys()) if (firstUntriedAction(actionsByState, tried, sig)) return true;
  return false;
}
function rememberEdge(graph, from, action, to) {
  const edges = graph.get(from) || [];
  if (!edges.some((e) => e.action === action && e.to === to)) edges.push({ action, to });
  graph.set(from, edges);
}
function pathToFrontier(graph, actionsByState, tried, start) {
  if (firstUntriedAction(actionsByState, tried, start)) return [];
  const seen = new Set([start]);
  const q = [{ sig: start, path: [] }];
  for (let i = 0; i < q.length; i++) {
    const { sig, path } = q[i];
    for (const { action, to } of graph.get(sig) || []) {
      if (seen.has(to)) continue;
      seen.add(to);
      const nextPath = path.concat(action);
      if (firstUntriedAction(actionsByState, tried, to)) return nextPath;
      q.push({ sig: to, path: nextPath });
    }
  }
  return null;
}

// xorshift32, identical to explorer.dart so seeds mean the same thing.
function rng(seed) {
  let s = (seed >>> 0) || 1;
  return (n) => {
    s ^= (s << 13); s >>>= 0;
    s ^= (s >> 17);
    s ^= (s << 5); s >>>= 0;
    return (s & 0x7fffffff) % n;
  };
}

// The shared UTF-8 encoder for the canonical hash + V: byte-order sort. The
// descriptor and V: keys can carry non-ASCII (a localized anchor, a non-ASCII
// id, an emoji icon), so we MUST fold the UTF-8 BYTES, exactly like the Rust
// oracle's `desc.as_bytes()`. Folding UTF-16 code units silently diverged.
const REPROIT_UTF8 = new TextEncoder();

// FNV-1a over the UTF-8 BYTES of an arbitrary descriptor string. Used for the
// STRUCTURAL signature (fed a structure descriptor) and for hashing long labels
// in clipLabel. Matches explorer.dart's fnv1a so seeds/hashes line up.
function fnv1a(s) {
  const bytes = REPROIT_UTF8.encode(s);
  let h = 0x811c9dc5;
  for (let i = 0; i < bytes.length; i++) {
    h ^= bytes[i];
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return (h >>> 0).toString(16).padStart(8, '0');
}

// Lexicographic comparison by UTF-8 byte sequence, matching Rust's String::cmp
// (byte order). JS `<` compares UTF-16 code units, which diverges for astral vs
// high-BMP keys, so the canonical V: section MUST sort with this.
function reproitCmpUtf8(a, b) {
  const ab = REPROIT_UTF8.encode(a);
  const bb = REPROIT_UTF8.encode(b);
  const n = Math.min(ab.length, bb.length);
  for (let i = 0; i < n; i++) { if (ab[i] !== bb[i]) return ab[i] < bb[i] ? -1 : 1; }
  return ab.length === bb.length ? 0 : ab.length < bb.length ? -1 : 1;
}

// ====================================================================
//  CANONICAL STRUCTURAL SIGNATURE (pure, Node-tree -> 8 hex)
//  Byte-identical to the Rust oracle (crates/reproit/src/model/signature.rs),
//  sdk/reproit-web.js, and the golden vectors (signature_vectors.json).
//  Spec: docs/signature.md. This block is host-pure (no DOM) so the parity
//  test imports it directly; the browser-side snapshot() builds a Node tree in
//  page context and feeds it here in Node.
// ====================================================================
const ROLES = {
  screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
  icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
  slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
};
const TRANSIENT_ROLES = { toast: 1, snackbar: 1, spinner: 1, progress: 1, tooltip: 1, badge: 1 };
// Value-role set (docs/signature.md "Value-state", Layer 2). A node is value-
// bearing iff it has a `value` AND either its RAW role is one of these OR it
// carries the opt-in value_node flag (Layer 3). status/log/progressbar/meter/
// timer/output are NOT in the structural vocabulary so they normalize to "node"
// in the body; the value-role test uses the RAW role on purpose. Chrome roles
// (button/header/text/link) are NEVER value-bearing (rule 1 preserved).
const VALUE_ROLES = { textfield: 1, status: 1, log: 1, progressbar: 1, meter: 1, timer: 1, output: 1 };

function normalizeRole(role) { return ROLES[role] ? role : 'node'; }
function isTransientNode(node) { return !!node.transient || !!TRANSIENT_ROLES[node.role]; }
function isValueBearing(node) {
  return node.value != null && (!!VALUE_ROLES[node.role] || !!node.value_node);
}

function normalizeNode(node) {
  if (isTransientNode(node)) return null;
  const kids = [];
  const children = node.children || [];
  for (const c of children) { const n = normalizeNode(c); if (n) kids.push(n); }
  return {
    role: normalizeRole(node.role),
    type: node.type != null ? node.type : null,
    icon: node.icon != null ? node.icon : null,
    id: node.id != null ? node.id : null,
    children: kids,
  };
}
function tokenBody(n) {
  let s = n.role;
  if (n.type != null) s += ':' + n.type;
  if (n.icon != null) s += '#' + n.icon;
  if (n.id != null) s += '@' + n.id;
  return s;
}
function subtreeKey(n) {
  const tokens = [];
  (function walk(node, depth) {
    tokens.push(depth + ':' + tokenBody(node));
    for (const c of node.children) walk(c, depth + 1);
  })(n, 0);
  return tokens.join(';');
}
function serializeNode(n, depth, repeated, tokens) {
  let tok = depth + ':' + tokenBody(n);
  if (repeated) tok += '*';
  tokens.push(tok);
  serializeChildren(n.children, depth + 1, tokens);
}
function serializeChildren(children, depth, tokens) {
  let i = 0;
  while (i < children.length) {
    const key = subtreeKey(children[i]);
    let j = i + 1;
    while (j < children.length && subtreeKey(children[j]) === key) j++;
    serializeNode(children[i], depth, (j - i) >= 2, tokens);
    i = j;
  }
}
// ---- Layer 2: value-class identity (canonical, mirrors the Rust oracle) ----
// Strict ^[+-]?[0-9]+(\.[0-9]+)?$: optional sign, >=1 ASCII digits, optional
// period + >=1 ASCII digits. No grouping, no exponent, no leading/trailing dot.
function isStrictDecimal(s) {
  let i = 0; const n = s.length;
  if (i < n && (s.charCodeAt(i) === 43 || s.charCodeAt(i) === 45)) i++;
  const intStart = i;
  while (i < n && s.charCodeAt(i) >= 48 && s.charCodeAt(i) <= 57) i++;
  if (i === intStart) return false;
  if (i < n && s.charCodeAt(i) === 46) {
    i++; const fracStart = i;
    while (i < n && s.charCodeAt(i) >= 48 && s.charCodeAt(i) <= 57) i++;
    if (i === fracStart) return false;
  }
  return i === n;
}
// Bounded, deterministic, locale-safe value-class token (docs/signature.md
// "Value-state"). EMPTY / strict-decimal -> ZERO|NEG|POS1|POS2|POS3|POSL / else
// NONEMPTY. Identical rule to the oracle's value_class.
function valueClass(s) {
  const t = (s == null ? '' : String(s)).replace(/^\s+|\s+$/g, '');
  if (t.length === 0) return 'EMPTY';
  if (isStrictDecimal(t)) {
    const num = parseFloat(t);
    const a = Math.abs(num);
    if (num === 0) return 'ZERO';
    if (num < 0) return 'NEG';
    if (a < 10) return 'POS1';
    if (a < 100) return 'POS2';
    if (a < 1000) return 'POS3';
    return 'POSL';
  }
  return 'NONEMPTY';
}
function valueKeyOf(node, structuralIndex) {
  if (node.id != null) return 'key:' + node.id;
  return 'role:' + normalizeRole(node.role) + '#' + structuralIndex;
}
function collectValues(node, out) {
  if (isTransientNode(node)) return;
  if (isValueBearing(node)) out.push([valueKeyOf(node, 0), valueClass(node.value)]);
  collectValuesChildren(node, out);
}
function collectValuesChildren(node, out) {
  const roleCounts = {};
  const children = node.children || [];
  for (const child of children) {
    if (isTransientNode(child)) continue;
    const role = normalizeRole(child.role);
    const idx = roleCounts[role] || 0;
    roleCounts[role] = idx + 1;
    if (isValueBearing(child)) out.push([valueKeyOf(child, idx), valueClass(child.value)]);
    collectValuesChildren(child, out);
  }
}
// Build the V: section suffix. "" when no value-bearing node exists (byte-
// identical to a pre-value-state tree); else "\nV:" + sorted key=class entries.
function valueSection(root) {
  const pairs = [];
  collectValues(root, pairs);
  if (pairs.length === 0) return '';
  pairs.sort((a, b) => reproitCmpUtf8(a[0], b[0]));
  return '\nV:' + pairs.map((p) => p[0] + '=' + p[1]).join(';');
}

function descriptorOf(anchor, root) {
  const tokens = [];
  const norm = normalizeNode(root);
  if (norm) serializeNode(norm, 0, false, tokens);
  return 'A:' + (anchor == null ? '' : anchor) + '\n' + tokens.join(';') + valueSection(root);
}
function signatureOf(anchor, root) { return fnv1a(descriptorOf(anchor, root)); }

// BROKEN-ROUTE ground-truth predicates (shared so the same rule is used by the
// runner and by its unit tests). A route is DEAD only when the resource is
// GENUINELY GONE: HTTP 404 (not found) or 410 (gone). Never 405/501 (method
// semantics -- a CDN answering HEAD 501 while GET is 200 was a false positive),
// 3xx (redirect), 401/403/429 (auth / rate limit), or 5xx (a transient server
// error is not a broken LINK).
function isDeadRouteStatus(s) { return s === 404 || s === 410; }
// Source for the non-app asset/download extensions the end-of-crawl probe must
// NOT fetch: archives, installers, media, fonts, and static web assets. NOTE:
// .html / .htm are deliberately NOT here -- they are navigable pages (a real 404
// on `pages/examples/invoice.html` must still fire). A 404 on an actual asset is a
// broken-asset concern, not a broken-route.
const ASSET_EXT_SOURCE = '\\.(zip|pdf|dmg|exe|msi|pkg|deb|rpm|apk|tar|gz|tgz|bz2|xz|7z|rar|iso|mp4|mp3|wav|mov|avi|mkv|webm|png|jpe?g|gif|svg|webp|avif|ico|bmp|css|js|mjs|cjs|map|wasm|woff2?|ttf|otf|eot|xml|csv|txt|rss|atom)$';
function isAssetPath(pathname) { return new RegExp(ASSET_EXT_SOURCE, 'i').test(pathname || ''); }

// In-page collector of same-origin APP link targets for the end-of-crawl
// broken-route probe (self-contained so it can be passed to page.evaluate and
// imported by tests). Dedup + first-source-wins is the caller's job; this returns
// the normalized pathnames to probe, EXCLUDING links a real user never GET-reaches:
//   - download links + asset/download extensions (a 404 there is a broken-asset,
//     and many assets legitimately answer non-200 to a bare fetch);
//   - rel=nofollow / rel=external: POST-only OAuth buttons, sponsored / externally
//     owned links (the OpenStreetMap Google/Facebook/GitHub login buttons that GET
//     to a false 404);
//   - javascript:/mailto:/tel: and bare #fragments (not routes);
//   - form-submit targets (a POST the user submits, not a GET route).
// URLs resolve against <base href> when present (`a.href` is base-aware, unlike
// new URL(getAttribute)), and the trailing slash is normalized so /docs and /docs/
// collapse.
function collectRouteLinks(assetExtSrc) {
  const out = [];
  const ASSET_EXT = new RegExp(assetExtSrc, 'i');
  const norm = (p) => (p.length > 1 ? (p.replace(/\/+$/, '') || '/') : p);
  const relTokens = (a) => (a.getAttribute('rel') || '').toLowerCase().split(/\s+/);
  for (const a of document.querySelectorAll('a[href]')) {
    try {
      if (a.hasAttribute('download')) continue;
      const rel = relTokens(a);
      if (rel.includes('nofollow') || rel.includes('external')) continue;
      const rawHref = a.getAttribute('href') || '';
      if (/^(javascript:|mailto:|tel:|#)/i.test(rawHref.trim())) continue;
      if (a.closest('form') && (a.getAttribute('type') === 'submit' || a.hasAttribute('data-submit'))) continue;
      const u = new URL(a.href);
      if (u.origin !== location.origin || !u.pathname) continue;
      if (ASSET_EXT.test(u.pathname)) continue;
      out.push(norm(u.pathname));
    } catch (_) {}
  }
  return out;
}

// In-page signals used to tell a SPA SOFT-404 (a static host 404s a deep path but
// still serves index.html and the client router renders the correct view) from a
// GENUINE dead route. Self-contained for page.evaluate + tests. The host decides:
// a filled app mount with real interactive content and no dominant not-found
// heading == the app served a real view (NOT a broken route).
function soft404View() {
  const body = document.body;
  if (!body) return { controls: 0, mountFilled: false, notFound: false };
  const mount = document.querySelector('#root,#app,#__next,#__nuxt,[data-reactroot],main,[role=main]');
  const mountFilled = !!(mount && mount.querySelectorAll('*').length > 12);
  const controls = document.querySelectorAll(
    'a[href],button,[role=button],input,select,textarea,[role=tab],[role=menuitem]'
  ).length;
  const heads = Array.from(document.querySelectorAll('h1,h2,[role=heading]'))
    .map((h) => (h.textContent || '').trim().toLowerCase());
  const notFound = heads.some((t) => t.length < 60 && /(^|\b)(404|not found|page not found|doesn'?t exist|no such page)\b/.test(t));
  return { controls, mountFilled, notFound };
}
// Host-side decision from the soft404View signals: true == the app served a real
// view (soft 404), so it is NOT a broken route.
function isSoftHandled(view) {
  return !!(view && view.mountFilled && view.controls >= 8 && !view.notFound);
}

export { signatureOf, descriptorOf, valueClass, snapshot, gtCollect, gtTabOrder, detectContentBugs, typeInto, loadInputs, inputValueFor, classifyFrameIntervals, drawFindingBoxes, tap, isDeadRouteStatus, isAssetPath, ASSET_EXT_SOURCE, collectRouteLinks, soft404View, isSoftHandled, settleForSignature, normalizePathname, detectBotWall };

// Snapshot the DOM: a STRUCTURAL, locale-invariant signature plus display-only
// labels and the structural selectors for each tappable. Mirrors
// templates/explorer.dart: the signature is a hash of the tag/role tree shape +
// stable developer identifiers (data-testid, name, aria role, input type) +
// structural position, with ALL user-facing text excluded. Visible text is kept
// only as a display label for `map show`, never folded into the hash or into a
// selector. Elements are addressed by stable selector preference
// (data-testid > name > aria-role + structural index); a tappable lacking
// any explicit author key falls back to role+index and is flagged `nokey`.
// A raw DOM `id` is an implementation-local reference, not a stability contract:
// frameworks and applications routinely allocate it per render or per process.
// A single snapshot cannot distinguish an allocator id from a human-readable but
// still generated id without site/library heuristics. Canonical identity therefore
// uses explicit author contracts (`data-testid` / `data-test-id` / `name`) and
// otherwise falls back to role + structural position. Raw ids remain available to
// authored CSS and ARIA in the page, but never enter a state hash or saved replay.

async function snapshot(page, valueNodeSelectors) {
  const snap = await page.evaluate(async ({ maxLen, valueNodeSelectors }) => {
    const labels = [];          // DISPLAY-ONLY visible text
    const rawTaps = [];         // tappable nodes in document order
    const extraTaps = [];       // keyed pointer-operable nodes interactive() drops
    // Parent registry: a stable per-container index so sibling tappables can be
    // grouped (a button-cluster choice picker). Plus a selected-state read, so a
    // mutually-exclusive choice group (exactly one selected) is distinguishable
    // from a row of action buttons (none selected). Used by detectChoiceGroups.
    const parentReg = new Map(); let parentIdx = 0;
    const groupOf = (el) => {
      const par = el.parentElement; if (!par) return -1;
      if (!parentReg.has(par)) parentReg.set(par, parentIdx++);
      return parentReg.get(par);
    };
    // Owning-container id for a choice option: the CLOSEST ARIA choice container
    // (tablist / radiogroup / menu(bar)) or a <fieldset>, registered to a stable
    // id in DOM order. This scopes the choice-anomaly oracle per component so two
    // INDEPENDENT tablists/radiogroups on one page are not compared as one (which
    // produced false outliers). A radio with no container still groups by its
    // `name`. null when nothing owns it (the oracle then falls back to bare role).
    const choiceReg = new Map();
    let choiceIdx = 0;
    const choiceContainerOf = (el) => {
      const cont = el.closest && el.closest(
        '[role=tablist],[role=radiogroup],[role=menu],[role=menubar],fieldset'
      );
      if (cont) {
        if (!choiceReg.has(cont)) choiceReg.set(cont, 'c' + choiceIdx++);
        return choiceReg.get(cont);
      }
      const tag = el.tagName ? el.tagName.toLowerCase() : '';
      if (tag === 'input' && (el.getAttribute('type') || '').toLowerCase() === 'radio') {
        const nm = el.getAttribute('name');
        if (nm) return 'name:' + nm;
      }
      return null;
    };
    const selectedState = (el) => {
      const a = (n) => (el.getAttribute(n) || '').toLowerCase();
      if (a('aria-pressed') === 'true' || a('aria-selected') === 'true') return true;
      if (a('aria-checked') === 'true' || el.getAttribute('aria-current') != null) return true;
      const ds = a('data-state'); if (['active', 'selected', 'on', 'checked', 'open'].includes(ds)) return true;
      return false;
    };
    const textNodes = [];       // (stable-key, trimmed text) for the Layer-1 fingerprint

    // Fixed canonical role vocabulary (docs/signature.md "Roles").
    const ROLES = {
      screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
      icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
      slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
    };
    const TRANSIENT_ROLES = { toast: 1, snackbar: 1, spinner: 1, progress: 1, tooltip: 1, badge: 1 };

    // DOM -> canonical role, from tag + aria role + input type, NEVER text.
    const roleOf = (el) => {
      const tag = el.tagName.toLowerCase();
      const ariaRole = (el.getAttribute('role') || '').toLowerCase();
      if (ariaRole) {
        if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox') return 'textfield';
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

    // Optional input type refinement (textfield only).
    const typeOf = (el, role) => {
      if (role !== 'textfield') return null;
      if (el.tagName.toLowerCase() !== 'input') return null;
      const t = (el.getAttribute('type') || 'text').toLowerCase();
      const allowed = { text: 1, password: 1, email: 1, number: 1, search: 1 };
      return allowed[t] ? t : 'text';
    };

    // Language-independent icon identity: svg <use> href / data-icon. No text.
    const iconOf = (el) => {
      const di = el.getAttribute('data-icon') || el.getAttribute('data-icon-name');
      if (di && di.trim()) return di.trim();
      const use = el.querySelector ? el.querySelector('use[href], use[xlink\\:href]') : null;
      if (use) {
        const href = use.getAttribute('href') || use.getAttribute('xlink:href');
        if (href && href.trim()) return href.trim().replace(/^#/, '');
      }
      return null;
    };

    // Stable author contract: data-testid > name (for the descriptor token).
    const idOf = (el) => {
      const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
      if (testid && testid.trim()) return testid.trim();
      const name = el.getAttribute('name');
      if (name && name.trim()) return name.trim();
      return null;
    };

    // Selector KEY (for replay): kind-tagged so tap() can resolve it. Same
    // Raw DOM ids are intentionally skipped: their lifetime is not knowable from
    // one capture, so they cannot support a deterministic saved replay.
    const keyOf = (el) => {
      const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
      if (testid && testid.trim()) return 'testid:' + testid.trim();
      const name = el.getAttribute('name');
      if (name && name.trim()) return 'name:' + name.trim();
      return null;
    };

    // Elements running an INFINITE animation (a spinner/pulse/marquee that never
    // settles), computed ONCE per snapshot from a single document.getAnimations()
    // call. A per-node el.getAnimations() made every snapshot O(nodes) on a large
    // DOM (a code editor renders thousands of line nodes) and dominated the crawl;
    // this precompute + Set lookup is O(animations).
    const infiniteAnimEls = new Set();
    try {
      const all = document.getAnimations ? document.getAnimations() : [];
      for (const a of all) {
        if (a.playState !== 'running') continue;
        const t = a.effect && a.effect.getComputedTiming ? a.effect.getComputedTiming() : null;
        if (t && t.iterations === Infinity && a.effect && a.effect.target) infiniteAnimEls.add(a.effect.target);
      }
    } catch (_) {}

    // Transient heuristic: role / aria-live / class flag a flickering node.
    const isTransientEl = (el) => {
      const ariaRole = (el.getAttribute('role') || '').toLowerCase();
      if (TRANSIENT_ROLES[ariaRole]) return true;
      if (ariaRole === 'alert' || ariaRole === 'status') return true;
      const live = (el.getAttribute('aria-live') || '').toLowerCase();
      if (live === 'assertive' || live === 'polite') return true;
      const cls = (el.getAttribute('class') || '').toLowerCase();
      if (/\b(toast|snackbar|spinner|progress|loader|loading|tooltip|badge)\b/.test(cls)) return true;
      if (el.hasAttribute('data-transient')) return true;
      // A node mid-INFINITE-animation samples a different frame every capture, so
      // two renders of the same page diverge on it: exclude it. Finite animations
      // are already settled by settleForSignature before a parity capture.
      if (infiniteAnimEls.has(el)) return true;
      return false;
    };

    // RAW value-role (docs/signature.md "Value-state"): the value-role name for
    // a value-bearing DOM element, NEVER from text. role=status/log/progressbar/
    // meter/timer pass through; <output>/role=output -> output; an aria-live
    // region (polite/assertive) -> status (so a live counter is value-bearing
    // WITHOUT opt-in); text form fields -> textfield. null for chrome / non-text
    // inputs (password is never read).
    const valueRoleOf = (el) => {
      const tag = el.tagName.toLowerCase();
      const ar = (el.getAttribute('role') || '').toLowerCase();
      if (ar === 'status' || ar === 'log' || ar === 'progressbar' || ar === 'meter' || ar === 'timer') return ar;
      if (tag === 'output' || ar === 'output') return 'output';
      const live = (el.getAttribute('aria-live') || '').toLowerCase();
      if (live === 'polite' || live === 'assertive') return 'status';
      if (tag === 'input') {
        const t = (el.getAttribute('type') || 'text').toLowerCase();
        if (['checkbox', 'radio', 'range', 'button', 'submit', 'reset', 'image', 'hidden', 'file', 'password'].includes(t)) return null;
        return 'textfield';
      }
      if (tag === 'textarea' || tag === 'select') return 'textfield';
      if (ar === 'textbox' || ar === 'searchbox' || ar === 'combobox') return 'textfield';
      return null;
    };
    // The displayed value: the field .value for form controls, else trimmed
    // textContent for output/status/live nodes.
    const valueOf = (el) => {
      const tag = el.tagName.toLowerCase();
      if (tag === 'input' || tag === 'textarea' || tag === 'select') return el.value != null ? String(el.value) : '';
      return (el.textContent != null ? el.textContent : '').trim();
    };
    // Layer-3 opt-in: does this element match one of the value_nodes selectors?
    // key:<id> | role:<role>#<idx> | raw CSS. Same grammar as reproit.yaml.
    const selList = valueNodeSelectors || [];
    const matchesValueNode = (el) => {
      for (const sel of selList) {
        if (!sel) continue;
        if (sel.indexOf('key:') === 0) {
          const id = sel.slice(4);
          const got = (el.getAttribute('data-testid') || el.getAttribute('data-test-id') ||
            el.getAttribute('id') || el.getAttribute('name') || '').trim();
          if (id && got === id) return true;
        } else if (sel.indexOf('role:') === 0) {
          const hash = sel.indexOf('#');
          if (hash < 0) continue;
          const role = sel.slice(5, hash);
          const idx = parseInt(sel.slice(hash + 1), 10);
          if (!(idx >= 0)) continue;
          let seen = -1, target = null;
          const root = document.body || document.documentElement;
          (function walk(node) {
            if (target || !node) return;
            if (roleOf(node) === role) { seen++; if (seen === idx) { target = node; return; } }
            for (const c of node.children) walk(c);
          })(root);
          if (target === el) return true;
        } else {
          try { if (el.matches && el.matches(sel)) return true; } catch (e) {}
        }
      }
      return false;
    };

    const interactive = (el, role) => {
      const tag = el.tagName.toLowerCase();
      if (['a', 'button', 'select'].includes(tag)) return true;
      // Text fields ARE actionable: the explorer drives them with a "type"
      // action. Without this, form-gated apps (login, search, TodoMVC new-todo)
      // map to a single dead state because their only control is undrivable.
      if (tag === 'input' || tag === 'textarea') return true;
      if (role === 'textfield') return true;
      if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role)) return true;
      if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
      return false;
    };

    // A link that navigates OFF the app-under-test's origin (a team member's
    // LinkedIn, a "View on GitHub" footer). Tapping it leaves the app, so the
    // explorer must not offer it as an action: the destination is a foreign
    // site, not a state of the app, and recording it produces phantom states +
    // spurious dead ends. mailto:/tel:/javascript: are not external navigation.
    const isExternalLink = (el) => {
      const a = el.closest && el.closest('a[href]');
      if (!a) return false;
      let href;
      try { href = new URL(a.getAttribute('href'), location.href); } catch (e) { return false; }
      if (href.protocol !== 'http:' && href.protocol !== 'https:') return false;
      return href.origin !== location.origin;
    };

    const nameOf = (el) => {
      const aria = el.getAttribute('aria-label');
      if (aria && aria.trim()) return aria.trim();
      const title = el.getAttribute('title');
      if (title && title.trim()) return title.trim();
      const alt = el.getAttribute('alt');
      if (alt && alt.trim()) return alt.trim();
      const text = (el.innerText || el.textContent || '').trim().split('\n')[0].trim();
      return text;
    };
    const visible = (el) => {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) return false;
      const st = getComputedStyle(el);
      return st.visibility !== 'hidden' && st.display !== 'none';
    };
    // REACHABLE: a real user can hit this element. Style-visible is NOT enough,
    // an offstage control (positioned outside the viewport) or one fully occluded
    // by another element is style-visible but un-tappable. The floor test is the
    // SAME hit-test used by the framebuffer probe (runFramebufferProbe ~L1052):
    // the element's center must lie inside the viewport AND a hit-test there must
    // resolve to the element or a descendant (so a button whose deepest painted
    // node is an inner <span> still counts). Used to gate tap candidacy AND the
    // role+index assignment so an unreachable control is neither offered as an
    // action nor given an index a replay could resolve to.
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
    const boundsOf = (el) => {
      try {
        const r = el.getBoundingClientRect();
        if (!r || r.width <= 0 || r.height <= 0) return null;
        return [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)];
      } catch (_) {
        return null;
      }
    };
    // Pointer-operable but OUTSIDE interactive()'s tappable grammar: a control a
    // pointer user can drive (cursor:pointer, or an ARIA-interactive role /
    // focusable tabindex delegation marker) that interactive() does not take.
    // The operability ground truth (EXPLORE:GROUNDTRUTH) already counts these as
    // operable; mirroring that predicate here lets the explorer actually TAP
    // them, so an SPA built from delegated-click <div role=option> elements no
    // longer maps to a single state. Kept deliberately conservative (and the
    // caller adds ONLY keyed elements) so it expands coverage without flooding
    // the candidate set with decorative cursor:pointer chrome.
    const ARIA_OPERABLE = {
      button: 1, link: 1, checkbox: 1, radio: 1, switch: 1, tab: 1,
      menuitem: 1, menuitemcheckbox: 1, menuitemradio: 1, option: 1, slider: 1,
    };
    const pointerOperable = (el) => {
      // cursor:pointer is INHERITED, so only count an element that INTRODUCES it
      // (its parent is not already pointer), matching the ground-truth guard so a
      // clickable parent does not paint every descendant as a candidate.
      const parentCursor = el.parentElement ? getComputedStyle(el.parentElement).cursor : '';
      if (getComputedStyle(el).cursor === 'pointer' && parentCursor !== 'pointer') return true;
      const ariaRole = (el.getAttribute('role') || '').toLowerCase();
      if (ARIA_OPERABLE[ariaRole]) return true;
      const ti = el.getAttribute('tabindex');
      if (ti !== null && parseInt(ti, 10) >= 0) return true;
      return false;
    };
    const fnvLbl = (name) => {
      let h = 0x811c9dc5;
      for (let i = 0; i < name.length; i++) { h ^= name.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; }
      return (h >>> 0).toString(16).padStart(8, '0');
    };
    const clipLabel = (name) => {
      if (name.length <= maxLen) return name;
      const suffix = '#' + fnvLbl(name);
      return name.slice(0, maxLen - suffix.length) + suffix;
    };

    // Build the canonical Node tree (role + id + type + icon + children). The
    // root is the screen; invisible wrappers are skipped but their visible
    // descendants are hoisted; transient subtrees carry transient:true so the
    // host-side normalizer drops them. We also collect labels + tappables for
    // the display/elements list along the way.
    const buildNode = (el, isRoot) => {
      const role = isRoot ? 'screen' : roleOf(el);
      // Value-state (Layer 2): a value-role element (by tag/aria), an aria-live
      // region, or a Layer-3 opt-in node is value-bearing. Value-bearing WINS
      // over the transient heuristic, so a role=status / aria-live counter that
      // the transient heuristic would otherwise drop is kept as a value node and
      // its keypresses produce DISTINCT value-states.
      const vrole = !isRoot ? valueRoleOf(el) : null;
      const optIn = !isRoot && matchesValueNode(el);
      const valueBearing = !isRoot && (!!vrole || optIn);
      const transient = !isRoot && !valueBearing && isTransientEl(el);
      const node = { role: role };
      const id = idOf(el); if (id != null) node.id = id;
      const type = typeOf(el, role); if (type != null) node.type = type;
      const icon = iconOf(el); if (icon != null) node.icon = icon;
      if (valueBearing) {
        node.value = valueOf(el);
        // The flag makes the canonical is_value_bearing accept the node even
        // when roleOf normalized its raw value-role (status/output/...) to node.
        node.value_node = true;
        // Layer-1 content fingerprint: a value node's stable key + its raw value.
        const fkey = id != null ? 'key:' + id : 'vrole:' + (vrole || 'opt');
        textNodes.push([fkey, node.value]);
      }
      if (transient) { node.transient = true; node.children = []; return node; }

      // Layer-1 content fingerprint over text-bearing nodes (runner-local, NOT
      // canonical): any keyed element's own (non-child) trimmed text contributes
      // (stable-key, text). This catches a display whose textContent changes
      // without any structural move (a calculator/counter), so the action is seen
      // as EFFECTIVE even when the value node itself was not detected as a
      // value-role. The raw text never enters the canonical key.
      if (!isRoot && id != null && !valueBearing) {
        let own = '';
        for (const c of el.childNodes) { if (c.nodeType === 3) own += c.textContent; }
        own = own.trim();
        if (own) textNodes.push(['text:' + id, own]);
      }

      // labels + tappables (display/elements list; never in the hash)
      if (!isRoot) {
        const name = nameOf(el);
        if (name) labels.push(clipLabel(name));
        // Tap candidacy requires REACHABILITY, not just interactivity: an
        // offstage / occluded control is interactive in the DOM but a user can't
        // reach it, so the explorer must not offer it as an action and ddmin must
        // not be able to minimize a repro through it. Gating here means such a
        // control also never consumes a role+index slot (the index is assigned
        // from rawTaps below), so no replay selector can resolve to it.
        if (interactive(el, role) && reachable(el)) {
          const ac = (el.getAttribute && el.getAttribute('autocomplete') || '').toLowerCase();
          const it = (el.getAttribute && el.getAttribute('type') || '').toLowerCase();
          const purpose = ac === 'one-time-code' ? 'otp'
            : (ac === 'current-password' || ac === 'new-password' || it === 'password') ? 'password'
            : ac === 'username' ? 'username'
            : (ac === 'email' || it === 'email') ? 'email'
            : (ac === 'tel' || ac === 'tel-national' || it === 'tel') ? 'phone'
            : null;
          rawTaps.push({
            role, key: keyOf(el),
            label: name ? clipLabel(name) : '',
            bounds: boundsOf(el),
            external: isExternalLink(el),
            grp: groupOf(el),
            cgrp: choiceContainerOf(el),
            selected: selectedState(el),
            purpose,
          });
        } else if (reachable(el) && pointerOperable(el)) {
          // Only KEYED extras: a stable `key:<id>` selector is reproducible and
          // does NOT consume a role+index slot, so existing role:<role>#<idx>
          // selectors and the canonical signature are untouched. A pointer-
          // operable element with no stable id is exactly one a repro could not
          // address anyway, so dropping it here loses nothing replayable.
          const k = keyOf(el);
          if (k) {
            extraTaps.push({
              role, key: k,
              label: name ? clipLabel(name) : '',
              bounds: boundsOf(el),
            });
          }
        }
      }

      node.children = [];
      collectChildren(el, node.children);
      return node;
    };
    const collectChildren = (el, out) => {
      for (const child of el.children) {
        if (!visible(child)) { collectChildren(child, out); continue; }
        out.push(buildNode(child, false));
      }
    };

    const root = document.body || document.documentElement;
    const tree = root ? buildNode(root, true) : { role: 'screen', children: [] };

    // Structural selectors for replay (key, else role+per-role index).
    const perRole = {};
    const tappables = rawTaps.map((tn) => {
      const idx = perRole[tn.role] || 0;
      perRole[tn.role] = idx + 1;
      const sel = tn.key ? 'key:' + tn.key : 'role:' + tn.role + '#' + idx;
      return { sel, role: tn.role, index: idx, key: tn.key, label: tn.label, bounds: tn.bounds || null, external: !!tn.external, grp: tn.grp, cgrp: tn.cgrp != null ? tn.cgrp : null, selected: !!tn.selected, purpose: tn.purpose || null };
    });
    // Append the keyed pointer-operable extras (keyed selector only; no role
    // index, so nothing above shifts). Dedup against selectors already present
    // so an element can never appear twice in the candidate set.
    const present = new Set(tappables.map((t) => t.sel));
    for (const tn of extraTaps) {
      const sel = 'key:' + tn.key;
      if (present.has(sel)) continue;
      present.add(sel);
      tappables.push({ sel, role: tn.role, index: -1, key: tn.key, label: tn.label, bounds: tn.bounds || null });
    }

    const texts = [];
    const seenTextBoxes = new Set();
    for (const el of Array.from(document.querySelectorAll('body *'))) {
      if (!visible(el)) continue;
      let text = '';
      for (const c of el.childNodes) {
        if (c.nodeType === 3) text += c.textContent || '';
      }
      text = text.replace(/\s+/g, ' ').trim();
      if (!text) continue;
      const bounds = boundsOf(el);
      if (!bounds) continue;
      const key = text + '|' + bounds.join(',');
      if (seenTextBoxes.has(key)) continue;
      seenTextBoxes.add(key);
      texts.push({ text: clipLabel(text), bounds });
      if (texts.length >= 48) break;
    }

    // Anchor: route of the current screen = path + SPA hash route, but NOT the
    // query string. Hash routers (the common SPA case) put the real route in
    // location.hash (#/a vs #/b on one pathname), so the hash MUST be in the
    // signature or distinct screens collapse into one state. The query string
    // (location.search, plus any ?... embedded in the hash) is deliberately
    // EXCLUDED: utm/session/token params are volatile and would explode the
    // state space, and a screen that genuinely differs by query still differs
    // structurally (the DOM tree), so it stays distinct on its own.
    let anchor = null;
    let path = null;
    try {
      if (location && location.pathname) {
        let pth = location.pathname;
        // Trailing-slash route normalization (mirrors host-side normalizePathname):
        // /docs/ and /docs are the SAME screen, so a 301 that toggles the slash is
        // not a distinct route (else the same screen double-counts / a benign
        // redirect reads as a broken route).
        if (pth.length > 1) pth = pth.replace(/\/+$/, '') || '/';
        path = pth;
        let hash = location.hash || '';
        const q = hash.indexOf('?');
        if (q >= 0) hash = hash.slice(0, q);
        anchor = pth + hash;
      }
    } catch (e) {}

    // Layer-1 content fingerprint source: sorted (stable-key, trimmed text) over
    // value + keyed-text nodes. Sorted here so it is order-independent.
    textNodes.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : (a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0)));

    // Return `tree` as a JSON STRING, not the live object. Playwright's evaluate
    // serializer caps object-graph DEPTH (~100 nested refs) and throws "object
    // reference chain is too long" on a deeply nested DOM (e.g. docs sites with
    // many wrapper divs) -- which killed observe()/the whole crawl before any
    // state-present oracle (choice-anomaly, overflow) could run. A string has no
    // object graph, so it serializes regardless of DOM depth; parsed back below.
    return { tree: JSON.stringify(tree), anchor, path, labels: [...new Set(labels)], tappables, texts, textNodes };
  }, { maxLen: MAX_LABEL_LEN, valueNodeSelectors: valueNodeSelectors || [] });
  // Reparse the canonical tree (stringified in-page to dodge the serializer's
  // depth cap) back into the object signatureOf/descriptorOf consume.
  snap.tree = JSON.parse(snap.tree);

  // Hash the canonical Node tree with the host-pure canonical signature, exactly
  // like the Rust oracle and the golden vectors. Text never contributes.
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
  // the structural sig OR this fingerprint changed (see observe/effect checks).
  // This carries raw localized text and is NEVER folded into the canonical key.
  snap.content = snap.sig + '|' + snap.textNodes.map((p) => p[0] + '=' + p[1]).join(';');
  return snap;
}

// Trailing-slash route normalization: `/docs/` and `/docs` are the SAME screen,
// so a 301 that adds or drops a trailing slash must not read as a distinct route
// (else a benign redirect reads as a broken route and the same screen
// double-counts). Root `/` is left intact. Applied to the in-page anchor AND the
// host-side navStatus / link keys so route identity is consistent end to end.
function normalizePathname(p) {
  if (typeof p !== 'string' || p.length <= 1) return p;
  return p.replace(/\/+$/, '') || '/';
}

// DOM QUIESCENCE settle before a STRUCTURAL-SIGNATURE capture. It waits for the
// page to STOP changing so that two independent renders of the same URL converge:
//   1. network idle (no in-flight requests for a settle window),
//   2. no DOM mutation for a stable window (a MutationObserver quiet period),
//   3. running CSS transitions / Web Animations settled, then two clean frames.
// The blank-screen oracle applies this before re-checking a candidate-blank state,
// so a still-hydrating mid-load frame is not mistaken for a white-screen-of-death.
// Every wait is HARD-CAPPED, so a page that never idles (an infinite spinner /
// poll) still returns. Best-effort: any failure is ignored and the caller falls
// back to whatever is on screen.
async function settleForSignature(page) {
  try { await page.waitForLoadState('networkidle', { timeout: 2500 }); } catch (_) {}
  try {
    await page.evaluate(async () => {
      const twoFrames = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
      // No DOM mutation for a 400ms stable window; hard cap 1.8s. The early-exit
      // (a quiet page resolves at 400ms) keeps well-behaved pages fast; the cap
      // bounds the cost on a page that keeps mutating (polling/analytics).
      await new Promise((resolve) => {
        let obs = null;
        let quiet = null;
        const finish = () => {
          if (quiet) clearTimeout(quiet);
          if (hard) clearTimeout(hard);
          if (obs) { try { obs.disconnect(); } catch (_) {} }
          resolve();
        };
        const arm = () => { if (quiet) clearTimeout(quiet); quiet = setTimeout(finish, 400); };
        const hard = setTimeout(finish, 1800);
        try {
          obs = new MutationObserver(arm);
          obs.observe(document.documentElement, { subtree: true, childList: true, attributes: true, characterData: true });
        } catch (_) {}
        arm();
      });
      // Running transitions / animations settled; hard cap 800ms (an infinite
      // animation never resolves its `finished`, so the race releases it).
      try {
        const running = (document.getAnimations ? document.getAnimations() : [])
          .filter((a) => a.playState === 'running');
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
      if (document.querySelector(
        '#challenge-running, #cf-challenge-running, #challenge-form, .cf-turnstile, [id^="cf-chl"], script[src*="challenge-platform"], iframe[src*="challenges.cloudflare.com"]'
      )) return { vendor: 'Cloudflare', marker: 'challenge-platform' };
      if (has(/just a moment/) || has(/checking your browser before/)
        || has(/performing (a )?security verification/)
        || has(/enable javascript and cookies to continue/)) {
        return { vendor: 'Cloudflare', marker: 'interstitial' };
      }
      if (has(/attention required/) && has(/cloudflare/)) return { vendor: 'Cloudflare', marker: 'attention-required' };
      if (document.querySelector('#px-captcha, .px-block, [class*="perimeterx"]')) return { vendor: 'PerimeterX', marker: 'px-captcha' };
      if (has(/verify you are (a )?human/)
        && document.querySelector('iframe[src*="captcha"], .g-recaptcha, .h-captcha')) {
        return { vendor: 'WAF', marker: 'human-verification' };
      }
      // A bare Cloudflare block page: dominated by a Ray ID with little else.
      if (/ray id:/.test(bodyText) && bodyText.length < 1200) return { vendor: 'Cloudflare', marker: 'ray-id-block' };
      return null;
    });
  } catch (_) { return null; }
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
      screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
      icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
      slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
    };
    const roleOf = (el) => {
      const tag = el.tagName.toLowerCase();
      const ariaRole = (el.getAttribute('role') || '').toLowerCase();
      if (ariaRole) {
        if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox') return 'textfield';
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
      if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role)) return true;
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
      'banner', 'complementary', 'contentinfo', 'form', 'main', 'navigation',
      'region', 'search',
      // document structure
      'article', 'definition', 'directory', 'document', 'feed', 'figure', 'group',
      'heading', 'img', 'list', 'listitem', 'math', 'none', 'note', 'presentation',
      'separator', 'table', 'term', 'toolbar', 'tooltip', 'caption', 'rowgroup',
      'row', 'cell', 'columnheader', 'rowheader',
      // containers + live regions / status
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
    for (const e of document.querySelectorAll('[data-reproit-gt]')) e.removeAttribute('data-reproit-gt');
    const out = [];
    // perRole counts ONLY tappable-walk elements, so role:<role>#<idx> selectors
    // match snapshot()/EXPLORE:STATE byte-for-byte. The ground truth also covers
    // a BROADER set: elements that are operable by pointer yet the tappable
    // grammar drops them (the <div role=option tabindex=-1> delegated case is
    // the motivating one). Such broader-only elements use the same explicit
    // author key or structural fallback as snapshot(), so they still join.
    const perRole = {};
    const root = document.body || document.documentElement;
    const walk = (el, isRoot) => {
      if (!isRoot && !visible(el)) { for (const c of el.children) walk(c, false); return; }
      if (!isRoot) {
        const role = roleOf(el);
        // The tappable walk takes only REACHABLE interactives, lockstep with
        // snapshot(), so role:<role>#<idx> indices match EXPLORE:STATE.
        const isReachable = reachable(el);
        const inTappableWalk = interactive(el, role) && isReachable;
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
            sel, role, native, cursor, deleg,
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
      const { listeners } = await cdp.send('DOMDebugger.getEventListeners', { objectId: result.objectId });
      if ((listeners || []).some((l) => l.type === 'click' || l.type === 'pointerdown' || l.type === 'mousedown')) return true;
    } catch (e) { /* CDP best-effort */ }
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
    const { listeners } = await cdp.send('DOMDebugger.getEventListeners', { objectId: result.objectId });
    const ls = listeners || [];
    return {
      pointer: ls.some((l) => l.type === 'click' || l.type === 'pointerdown' || l.type === 'mousedown'),
      key: ls.some((l) => l.type === 'keydown' || l.type === 'keypress' || l.type === 'keyup'),
    };
  } catch (e) { return { pointer: false, key: false }; }
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
  await page.evaluate(() => { try { if (document.activeElement) document.activeElement.blur(); document.body.focus(); } catch (e) {} });
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
    await page.evaluate(({ x, y }) => {
      try {
        if (document.activeElement) document.activeElement.blur();
        const body = document.body;
        if (body) {
          const old = body.getAttribute('tabindex');
          body.setAttribute('tabindex', '-1');
          body.focus({ preventScroll: true });
          if (old == null) body.removeAttribute('tabindex'); else body.setAttribute('tabindex', old);
        }
        window.scrollTo(x, y);
      } catch (_) {}
    }, scroll).catch(() => {});
    // Give focusout-driven portal teardown two presented frames to finish before
    // exploration observes or acts on the page again.
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))).catch(() => {});
  }
  // Focus trap: after focus first left body it never came back (no -2 after the
  // first real focus), yet focus kept moving. A page that lets you Tab back out
  // to the body/address bar is not trapped.
  let firstReal = visited.findIndex((v) => v >= 0 || v === -1);
  let returnedToBody = false;
  if (firstReal >= 0) {
    for (let k = firstReal + 1; k < visited.length; k++) if (visited[k] === -2) { returnedToBody = true; break; }
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
    try { cdp.off('Page.screencastFrame', onFrame); } catch (_) {}
    return null;
  }
  return {
    async stop() {
      try { await cdp.send('Page.stopScreencast'); } catch (_) {}
      try { cdp.off('Page.screencastFrame', onFrame); } catch (_) {}
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
  try { frames = await cap.stop(); } catch (_) { return; }
  if (!frames || frames.length < 3) return;
  let rgbas;
  try { rgbas = frames.map(pngToRgba); } catch (_) { return; }
  const final = rgbas[rgbas.length - 1];
  // Per-frame distance to the FINAL settled frame. Skip any frame whose
  // dimensions differ from the final (a resize, not a flash) rather than score
  // it as fully-different.
  const diffs = [];
  for (const f of rgbas) {
    if (f.width !== final.width || f.height !== final.height || f.data.length !== final.data.length) {
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
  try { vp = page.viewportSize() || { width: 1280, height: 800 }; } catch (_) { vp = { width: 1280, height: 800 }; }
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
    } catch (_) { a11yCovered = true; /* unknown -> don't flag */ }

    try {
      beforeBuf = await page.screenshot({ clip: clipAround(pt, vp), animations: 'disabled' });
      await page.mouse.click(pt.x, pt.y, { delay: 10 });
      await page.waitForTimeout(120);
      afterBuf = await page.screenshot({ clip: clipAround(pt, vp), animations: 'disabled' });
    } catch (_) { continue; }

    let changed = 0;
    try {
      const a = pngToRgba(beforeBuf);
      const b = pngToRgba(afterBuf);
      changed = changedFraction(a.data, b.data);
    } catch (_) { changed = 0; }
    probed.push({ x: pt.x, y: pt.y, changed, a11yCovered });
  }
  // The clicks may have navigated/mutated the page; restore the start screen so
  // the explorer's next snapshot reflects the real state, not a probe artifact.
  try { await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }); await page.waitForTimeout(300); } catch (_) {}
  const gaps = probeRegionsToGroundtruth(probed);
  if (gaps.length) log(`JOURNEY[a] step: framebuffer-probe found ${gaps.length} operable region(s) with no a11y control`);
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
  try { els = await gtCollect(page); } catch (e) { return; }
  // PIECE 2 floor: when opted in, the framebuffer probe contributes operable
  // regions that have NO a11y/DOM node (so gtCollect, which is DOM-based, can't
  // see them). Run it first; its results are appended to the records below.
  let probeEls = [];
  if (PROBE) {
    try { probeEls = await runFramebufferProbe(page); } catch (_) { probeEls = []; }
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
  let inTab = new Set(), focusTrap = false;
  try { ({ inTab, focusTrap } = await gtTabOrder(page, els.length, 60)); } catch (e) {}

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
  try { await page.evaluate(() => { for (const el of document.querySelectorAll('[data-reproit-gt]')) el.removeAttribute('data-reproit-gt'); }); } catch (e) {}

  // Append the framebuffer-probe floor's findings (operable regions with no DOM/
  // a11y node). These are addressed by spatial selector, so they never collide
  // with the DOM `sel` ids above.
  log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap, elements: records.concat(probeEls) }));
}

// STRUCTURAL tap: resolve a locale-invariant selector and click it. Returns
// true on success. Mirrors explorer.dart's tapSelector. No visible text is ever
// used to locate the element.
//   key:testid:<v> -> [data-testid="v"] (or data-test-id)
//   key:id:<v>     -> #<v>
//   key:name:<v>   -> [name="v"]
//   role:<role>#<idx> -> the idx-th visible tappable of that role, document order
async function tap(page, sel, opts) {
  const ok = await page.evaluate(({ s, mark, box, boxColor }) => {
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
    const cssEscape = (v) => (window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\]/g, '\\$&'));
    // On a recorded replay, tag the clicked element so a crash/jank/hang box can
    // point at exactly the control the user actuated (only the LAST one carries
    // the tag). Gated on `mark` so a normal fuzz walk never touches the DOM.
    const doClick = (el) => {
      if (mark) {
        try {
          for (const e of document.querySelectorAll('[data-reproit-trigger]')) e.removeAttribute('data-reproit-trigger');
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
          if (!inView) el.scrollIntoView({ behavior: 'smooth', block: 'center', inline: 'nearest' });
        } catch (_) {}
        const old = document.getElementById('__reproit_tapbox'); if (old) old.remove();
        const r = el.getBoundingClientRect();
        const layer = document.createElement('div');
        layer.id = '__reproit_tapbox';
        layer.style.cssText = 'position:absolute;top:0;left:0;width:0;height:0;z-index:2147483646;pointer-events:none';
        const b = document.createElement('div');
        const col = boxColor || '#2f6bff';
        b.style.cssText = [
          'position:absolute', 'top:' + (r.top + window.scrollY - 2) + 'px', 'left:' + (r.left + window.scrollX - 2) + 'px',
          'width:' + (r.width + 4) + 'px', 'height:' + (r.height + 4) + 'px',
          'border:3px solid ' + col, 'background:' + col + '20', 'border-radius:4px',
          'box-shadow:0 0 0 1px rgba(255,255,255,.5),0 4px 18px rgba(0,0,0,.35)',
        ].join(';');
        const tag = document.createElement('div');
        tag.textContent = box;
        tag.style.cssText = [
          'position:absolute', 'top:-22px', 'left:-3px', 'background:' + col, 'color:#fff',
          'font:600 12px/1 ui-monospace,SFMono-Regular,Menlo,monospace', 'padding:4px 7px',
          'border-radius:5px', 'white-space:nowrap', 'box-shadow:0 2px 8px rgba(0,0,0,.4)',
        ].join(';');
        b.appendChild(tag); layer.appendChild(b);
        (document.body || document.documentElement).appendChild(layer);
        return true;
      }
      // Stash the clicked element for the post-tap oracle probes (the
      // duplicate-submit eligibility check and the focus-loss guards read it
      // in-page). A window ref only, never a DOM mutation, so the signature/
      // content/mutation oracles are untouched.
      try {
        window.__reproitLastTap = el;
        // FOCUS-LOSS probe: a real user click gives the control keyboard focus
        // before activating it; el.click() alone does not. When the walk armed
        // the probe pre-tap (focusLossArm), focus first (no scroll, so the
        // viewport-dependent snapshot is untouched) so the oracle can observe
        // whether the app's re-render then drops focus back to <body>.
        if (window.__reproitFocusProbe) {
          try { el.focus({ preventScroll: true }); } catch (_) {}
          window.__reproitTapFocused = document.activeElement === el;
        }
      } catch (_) {}
      el.click();
      return true;
    };

    if (s.startsWith('key:')) {
      const body = s.slice(4);
      const ci = body.indexOf(':');
      if (ci < 0) return false;
      const kind = body.slice(0, ci);
      const val = body.slice(ci + 1);
      let el = null;
      if (kind === 'testid') {
        el = document.querySelector('[data-testid="' + cssEscape(val) + '"]')
          || document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
      } else if (kind === 'id') {
        el = document.getElementById(val);
      } else if (kind === 'name') {
        el = document.querySelector('[name="' + cssEscape(val) + '"]');
      }
      if (!el) return false;
      // A control that exists in the DOM but isn't REACHABLE (behind an auth
      // gate, offstage, or occluded) is not actionable: report it as a miss so a
      // journey that assumed it could reach this control is classified stale, not
      // a silent pass. Reachability (not just style-visibility) is the floor so a
      // keyed selector to an offstage control fails exactly like a user would.
      if (!reachable(el)) return false;
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
        screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
        icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
        slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
      };
      const roleOf = (el) => {
        const tag = el.tagName.toLowerCase();
        const ariaRole = (el.getAttribute('role') || '').toLowerCase();
        if (ariaRole) {
          if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox') return 'textfield';
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
        if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(r)) return true;
        if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
        return false;
      };
      let seen = -1, target = null;
      const walk = (el) => {
        if (target) return;
        if (!visible(el)) { for (const c of el.children) walk(c); return; }
        const r = roleOf(el);
        // Count only REACHABLE candidates so the per-role index matches the one
        // snapshot() assigned (which also gates on reachable). An offstage control
        // is walked into for its children but never consumes an index here.
        if (interactive(el, r) && r === role && reachable(el)) { seen++; if (seen === idx) { target = el; return; } }
        for (const c of el.children) walk(c);
      };
      const root = document.body || document.documentElement;
      if (root) walk(root);
      if (!target) return false;
      return doClick(target);
    }

    return false;
  }, { s: sel, mark: !!(opts && opts.mark), box: (opts && opts.box) || null, boxColor: (opts && opts.boxColor) || null }).catch(() => false);
  return !!ok;
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
  const found = await page.evaluate(({ s, mark }) => {
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
    const cssEscape = (v) => (window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\]/g, '\\$&'));

    let el = null;
    if (s.startsWith('key:')) {
      const body = s.slice(4);
      const ci = body.indexOf(':');
      if (ci < 0) return false;
      const kind = body.slice(0, ci);
      const val = body.slice(ci + 1);
      if (kind === 'testid') {
        el = document.querySelector('[data-testid="' + cssEscape(val) + '"]')
          || document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
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
        screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
        icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
        slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
      };
      const roleOf = (el) => {
        const tag = el.tagName.toLowerCase();
        const ariaRole = (el.getAttribute('role') || '').toLowerCase();
        if (ariaRole) {
          if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox') return 'textfield';
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
        if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(r)) return true;
        if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
        return false;
      };
      let seen = -1, target = null;
      const walk = (el) => {
        if (target) return;
        if (!visible(el)) { for (const c of el.children) walk(c); return; }
        const r = roleOf(el);
        // Count only REACHABLE candidates, lockstep with snapshot()'s index.
        if (interactive(el, r) && r === role && reachable(el)) { seen++; if (seen === idx) { target = el; return; } }
        for (const c of el.children) walk(c);
      };
      const root = document.body || document.documentElement;
      if (root) walk(root);
      el = target;
    }
    if (!el) return false;
    // A field that isn't REACHABLE (behind an auth gate, a collapsed panel, or
    // offstage) is not fillable: a miss, so the journey is stale rather than a
    // silent pass.
    if (!reachable(el)) return false;
    // Only type into things that hold text; a non-text target is a miss so the
    // caller treats it like a failed action rather than silently no-op'ing.
    const tag = el.tagName.toLowerCase();
    const isText = tag === 'textarea'
      || (el.getAttribute && (el.getAttribute('role') || '').toLowerCase().match(/textbox|searchbox|combobox/))
      || el.isContentEditable
      || (tag === 'input' && !['checkbox', 'radio', 'range', 'button', 'submit', 'reset', 'image']
        .includes((el.getAttribute('type') || 'text').toLowerCase()));
    if (!isText) return false;
    try { el.focus(); } catch (e) {}
    el.setAttribute('data-reproit-typed', '1');
    // Recorded replay: tag this field as the trigger so a crash/jank box (e.g. a
    // form that throws on submit) can point at it. Only the latest action's tag.
    if (mark) {
      try { for (const e of document.querySelectorAll('[data-reproit-trigger]')) e.removeAttribute('data-reproit-trigger'); el.setAttribute('data-reproit-trigger', '1'); } catch (_) {}
    }
    return true;
  }, { s: sel, mark: !!(opts && opts.mark) }).catch(() => false);
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
  } catch (e) { return false; }
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
      const ok = await page.evaluate((t) => !!(document.body && document.body.innerText.includes(t)), want).catch(() => false);
      log('FUZZ:ASSERT ' + (ok ? 'pass' : 'fail') + ' text=' + JSON.stringify(want) + ' actor=' + who);
    } else if (body.startsWith('count:')) {
      const rest = body.slice('count:'.length);
      const eq = rest.lastIndexOf('=');
      const finder = eq >= 0 ? rest.slice(0, eq) : rest;
      const want = eq >= 0 ? parseInt(rest.slice(eq + 1), 10) : 0;
      const got = await page.evaluate(countMatching, finder).catch(() => -1);
      log('FUZZ:ASSERT ' + (got === want ? 'pass' : 'fail') + ' count ' + finder + ' want=' + want + ' got=' + got + ' actor=' + who);
    } else {
      log('FUZZ:ASSERT fail unsupported ' + body + ' actor=' + who);
    }
    await page.waitForTimeout(300);
    return;
  }
  if (act === 'back') { await page.goBack().catch(() => {}); await page.waitForTimeout(400); return; }
  if (act.startsWith('type:')) {
    const b = act.slice('type:'.length);
    const eq = b.lastIndexOf('=');
    const sel = eq >= 0 ? b.slice(0, eq) : b;
    const valId = eq >= 0 ? b.slice(eq + 1) : 'normal';
    // PRECEDENCE: a property-matched fixture input for this field wins over the
    // adversarial-class token (same rule as the fuzz-replay path); else the
    // class token / env-expanded literal, unchanged.
    const fixtureVal = inputValueFor(sel, inputs);
    const value = fixtureVal != null
      ? fixtureVal
      : (ADVERSARIAL_BY_ID[valId] !== undefined ? ADVERSARIAL_BY_ID[valId] : expandEnv(valId));
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
    try { who = (await (await fetch(base + '/claim')).text()).trim(); } catch (_) { who = ''; }
    if (!who || who.startsWith('ERR')) who = 'a';
  }
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  page.on('pageerror', (err) => {
    const msg = String(err && err.message ? err.message : err);
    if (exceptionIsBenign(msg) || exceptionThrownInTracker(err && err.stack) || exceptionIsNonDeterministic(msg, err && err.stack) || !exceptionIsFirstParty(err && err.stack, APP_ORIGIN)) return;
    log('EXCEPTION CAUGHT BY WEB PAGE');
    log('actor ' + who + ': ' + msg);
    const stack = (err && err.stack) ? String(err.stack) : '';
    for (const line of stack.split('\n').slice(0, 8)) log(line);
    log('════════');
  });
  // Renderer/GPU/OOM crash (Playwright `crash`, not `pageerror`): emit the same
  // app-crash block so a process death isn't misattributed to the runner.
  page.on('crash', () => {
    log('EXCEPTION CAUGHT BY WEB PAGE');
    log('actor ' + who + ': the page crashed (renderer process gone -- GPU / out-of-memory / sad-tab)');
    log('════════');
  });
  await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
  log('JOURNEY claimed role=' + who);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  for (let guard = 0; guard < 100000; guard++) {
    let body = 'WAIT';
    try { body = (await (await fetch(base + '/next?device=' + who)).text()).trim(); }
    catch { await sleep(100); continue; }
    if (body === 'DONE') break;
    if (body === 'WAIT') { await sleep(40); continue; }
    const act = body.startsWith('ACT\t') ? body.slice(4) : body;
    await execScenarioAction(page, act, who, inputs);
    try { await fetch(base + '/done?device=' + who, { method: 'POST' }); } catch (_) {}
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
            'position:fixed', 'top:14px', 'left:50%', 'transform:translateX(-50%)',
            'z-index:2147483647', 'font:600 14px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace',
            'padding:10px 16px', 'border-radius:10px', 'pointer-events:none',
            'box-shadow:0 6px 24px rgba(0,0,0,.45)', 'max-width:92vw',
            'white-space:nowrap', 'overflow:hidden', 'text-overflow:ellipsis',
          ].join(';');
          (document.body || document.documentElement).appendChild(el);
        }
        el.style.background = isFail ? 'rgba(190,32,32,.96)' : 'rgba(18,20,26,.94)';
        el.style.color = '#fff';
        el.style.border = '1px solid ' + (isFail ? '#ff7a7a' : 'rgba(255,255,255,.14)');
        el.textContent = (isFail ? '✗  ' : '▸  ') + text;
      },
      { text, isFail }
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
  const drew = await page
    .evaluate(
      async ({ trigger, flickerKeys, oracle, linkHref }) => {
        try { clearInterval(window.__reproitBoxHeal); } catch (_) {}
        const old = document.getElementById('__reproit_boxes');
        if (old) old.remove();
        const visible = (el) => {
          const r = el.getBoundingClientRect();
          if (r.width === 0 || r.height === 0) return false;
          const st = getComputedStyle(el);
          return st.visibility !== 'hidden' && st.display !== 'none';
        };
        const sx = window.scrollX, sy = window.scrollY;
        // {prio,mag} orders findings by user-visible impact.
        const hits = [];
        const push = (el, label, prio, mag, cat, rect) => {
          // rect overrides the element box (a range-tightened text rect).
          const r = rect || el.getBoundingClientRect();
          hits.push({ top: r.top + sy, left: r.left + sx, w: r.width, h: r.height, label, prio, mag, el, cat });
        };
        const all = document.body ? document.body.querySelectorAll('*') : [];
        // Content-bug artifacts: the literal broken-stringify tokens, on the OWN
        // text of an element (mirrors detectContentBugs' reasonOf).
        const ownText = (el) => {
          let t = '';
          for (const c of el.childNodes) if (c.nodeType === 3) t += c.textContent;
          return t.replace(/\s+/g, ' ').trim();
        };
        const dominates = (s) => s.length <= 24 && !/[.!?]/.test(s);
        const reasonOf = (text) => {
          if (!text) return null;
          // Same prose guard as detectContentBugs for both artifact kinds.
          if (text.includes('[object Object]')) {
            const s = text.replace(/\[object Object\]/g, ' ').replace(/\s+/g, ' ').trim();
            if (dominates(s)) return '[object Object]';
          }
          if (/\{\{[^}]*\}\}/.test(text) || /\$\{[^}]*\}/.test(text)) {
            const s = text.replace(/\{\{[^}]*\}\}/g, ' ').replace(/\$\{[^}]*\}/g, ' ').replace(/\s+/g, ' ').trim();
            if (dominates(s)) return 'unrendered template';
          }
          return null;
        };
        // Skip a CODE context (mirrors detectContentBugs): template/markup syntax
        // shown as documentation is not a leaked binding.
        const CODE_TAGS = new Set(['code', 'pre', 'script', 'style', 'textarea']);
        const inCodeContext = (el) => {
          if (el.isContentEditable) return true;
          for (let n = el; n && n !== document.body; n = n.parentElement) {
            if (CODE_TAGS.has(n.tagName.toLowerCase())) return true;
          }
          return false;
        };
        const seenC = new Set();
        for (const el of all) {
          if (!visible(el)) continue;
          if (inCodeContext(el)) continue;
          const reason = reasonOf(ownText(el));
          if (!reason || seenC.has(el)) continue;
          seenC.add(el);
          push(el, 'content  ' + reason, 4, 1e6, 'content');
        }
        // TRIGGER element (crash / jank / hang): the control the failing action
        // targeted, tagged at click/focus time. Highest priority - it IS the bug
        // the user reproduces - so it sorts first and is the one scrolled to.
        if (trigger) {
          const t = document.querySelector('[data-reproit-trigger]');
          if (t && visible(t)) push(t, trigger, 5, 2e6, 'trigger');
        }
        // FLICKER: the persistent-chrome anchors that were rebuilt though their box
        // and text were unchanged. Resolve each key back to a live node by the same
        // id/testid/tag[role] grammar markAnchors used (first visible match).
        if (flickerKeys && flickerKeys.length) {
          const keyToEl = (key) => {
            const ci = key.indexOf(':');
            const kind = key.slice(0, ci), val = key.slice(ci + 1);
            if (kind === 'id') return document.getElementById(val);
            if (kind === 'testid') return document.querySelector('[data-testid="' + val + '"]') || document.querySelector('[data-test-id="' + val + '"]');
            if (kind === 'tag') {
              const m = val.match(/^([a-z0-9-]+)(?:\[([a-z]+)\])?$/i);
              if (!m) return null;
              const sel = m[2] ? m[1] + '[role="' + m[2] + '"]' : m[1];
              for (const el of document.querySelectorAll(sel)) if (visible(el)) return el;
            }
            return null;
          };
          const seenF = new Set();
          for (const k of flickerKeys) {
            const el = keyToEl(k);
            if (el && !seenF.has(el) && visible(el)) { seenF.add(el); push(el, 'flicker  rebuilt', 2, 5e5, 'flicker'); }
          }
        }
        // BROKEN-ROUTE: the source link whose navigation target is the dead route.
        // Box the <a> on THIS (source) page, captioned with its visible text +
        // href, so the bad link is locatable where a person would click it.
        if (linkHref) {
          for (const a of document.querySelectorAll('a[href]')) {
            if (!visible(a)) continue;
            const raw = a.getAttribute('href') || '';
            // A same-page fragment (#...) resolves to THIS page's pathname and
            // can never be the dead route; without this guard a "Skip to
            // Content" link matched the source path and the box landed on a
            // visually hidden element.
            if (raw.startsWith('#')) continue;
            let path = '';
            try { path = new URL(raw, location.href).pathname; } catch (e) { continue; }
            if (path !== linkHref) continue;
            const txt = (a.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 40);
            // A glyphless anchor (an image-overlay link) renders nothing of its
            // own, so a bare box reads as "a box around nothing". Caption it as
            // the image/overlay link it is, named by alt/aria-label when present.
            const img = a.querySelector('img') || (a.parentElement && a.parentElement.querySelector('img'));
            const label = txt || ((img && img.getAttribute('alt')) || a.getAttribute('aria-label') || '')
              .replace(/\s+/g, ' ').trim().slice(0, 40);
            const kind = txt ? 'broken link' : (img ? 'broken image link' : 'broken overlay link');
            // Tighten a block-level anchor's box to its rendered text so the box
            // hugs what a person sees instead of the full container width.
            let rect = null;
            if (txt) {
              try {
                const rg = document.createRange();
                rg.selectNodeContents(a);
                const rr = rg.getBoundingClientRect();
                if (rr.width > 0 && rr.height > 0) rect = rr;
              } catch (e) { /* keep the element rect */ }
            }
            push(a, kind + '  ' + (label ? '"' + label + '" → ' : '') + linkHref, 5, 3e6, 'link', rect);
            break;
          }
        }
        // SCOPE to the replayed finding's oracle: when this clip is one specific
        // repro (a gallery clip), box ONLY that finding's category and show a
        // SINGLE box, so each video is "just that issue", not every problem on the
        // page. The oracle name is the invariant the repro reproduces. Without a
        // hint (a generic record) keep the old behavior: all categories, up to 6.
        // Map the repro's oracle to a box category by keyword. Oracles with no on-screen element
        // Whole-process findings such as leak map to null and draw nothing.
        const catOf = (o) => {
          if (!o) return null;
          if (o.includes('broken-render') || o.includes('content')) return 'content';
          if (o.includes('flicker')) return 'flicker';
          if (o.includes('broken-route') || o.includes('not-found')) return 'link';
          if (o.includes('exception') || o.includes('crash') || o.includes('jank') || o.includes('hang') || o.includes('choice')) return 'trigger';
          return null;
        };
        let scoped;
        let cap;
        if (oracle) {
          const wantCat = catOf(oracle);
          // An oracle with no on-screen element (such as leak) draws nothing
          // rather than falling back to boxing unrelated issues.
          if (!wantCat) return false;
          scoped = hits.filter((h) => h.cat === wantCat);
          cap = 1; // a per-finding clip shows a SINGLE box: just that issue
        } else {
          scoped = hits;
          cap = 6;
        }
        if (!scoped.length) return false;
        // De-dupe nested hits (keep the outer), prioritize, cap.
        scoped.sort((a, b) => b.prio - a.prio || b.mag - a.mag);
        const chosen = [];
        for (const h of scoped) {
          // Skip a hit already covered by a higher-priority one: the same
          // element or an outer element that contains it.
          if (chosen.some((c) => c.el === h.el || c.el.contains(h.el))) continue;
          chosen.push(h);
          if (chosen.length >= cap) break;
        }
        // Bring the top offender into the recorded frame, HUMAN-PACED: a smooth
        // eased scroll, then WAIT FOR IT TO SETTLE before drawing. A fixed delay
        // is too short on a long page (the smooth scroll outlasts it), so the box
        // anchored to a mid-glide viewport and ended up off-screen once the scroll
        // finished -- the "clip shows no box" bug. Poll scrollY until it stops.
        try {
          const fr = chosen[0].el.getBoundingClientRect();
          const fvh = window.innerHeight || document.documentElement.clientHeight;
          const fvw = window.innerWidth || document.documentElement.clientWidth;
          const fInView = fr.top >= 0 && fr.left >= 0 && fr.bottom <= fvh && fr.right <= fvw;
          if (!fInView) chosen[0].el.scrollIntoView({ behavior: 'smooth', block: 'center', inline: 'center' });
        } catch (_) {}
        {
          let lastY = -1, stable = 0;
          for (let i = 0; i < 50; i++) {
            await new Promise((r) => setTimeout(r, 50));
            const y = window.scrollY;
            if (y === lastY) { if (++stable >= 3) break; } else { stable = 0; lastY = y; }
          }
        }
        const vx = window.scrollX, vy2 = window.scrollY;
        const vw = window.innerWidth || document.documentElement.clientWidth;
        const vh = window.innerHeight || document.documentElement.clientHeight;
        const layer = document.createElement('div');
        layer.id = '__reproit_boxes';
        layer.style.cssText = 'position:absolute;top:0;left:0;width:0;height:0;z-index:2147483646;pointer-events:none';
        for (const h of chosen) {
          const box = document.createElement('div');
          // CLAMP the box to the visible viewport (with an inset): an element bigger
          // than the viewport (a horizontally-overflowing carousel, a full-bleed
          // banner) drew its true bounds entirely off-frame, so nothing showed.
          // A fully-visible element is unchanged (the clamps are no-ops).
          const ins = 8;
          // Clamp the box fully INSIDE the viewport on BOTH axes. The old clamp
          // only pulled a box's NEAR edge in, so an element entirely off to the
          // right (a horizontal marquee/carousel whose box left > viewport right)
          // kept its off-screen left and drew nothing on camera -- the "overflow
          // clip shows no box" bug on dynamic sites. Pin the near edge into
          // [inset, viewport - inset - 8] so a box always lands on screen, at the
          // edge nearest the offender. A fully-visible element is unchanged.
          const bl = Math.min(Math.max(h.left - 2, vx + ins), vx + vw - ins - 8);
          const bt = Math.min(Math.max(h.top - 2, vy2 + ins), vy2 + vh - ins - 8);
          const br = Math.min(Math.max(h.left + h.w + 2, bl + 8), vx + vw - ins);
          const bb = Math.min(Math.max(h.top + h.h + 2, bt + 8), vy2 + vh - ins);
          const bw = Math.max(8, br - bl);
          const bh = Math.max(8, bb - bt);
          box.style.cssText = [
            'position:absolute', 'top:' + bt + 'px', 'left:' + bl + 'px',
            'width:' + bw + 'px', 'height:' + bh + 'px',
            'border:3px solid #e21f1f', 'background:rgba(226,31,31,.10)', 'border-radius:4px',
            'box-shadow:0 0 0 1px rgba(255,255,255,.5),0 4px 18px rgba(0,0,0,.35)', 'pointer-events:none',
          ].join(';');
          const tag = document.createElement('div');
          tag.textContent = h.label;
          // Sit the label above the box, but flip it just inside the top edge when
          // the box hugs the viewport top (a clamped/banner box) so it stays on-screen.
          const labelTop = (bt - vy2) < 24 ? 3 : -22;
          tag.style.cssText = [
            'position:absolute', 'top:' + labelTop + 'px', 'left:-3px', 'background:#e21f1f', 'color:#fff',
            'font:600 12px/1 ui-monospace,SFMono-Regular,Menlo,monospace', 'padding:4px 7px',
            'border-radius:5px', 'white-space:nowrap', 'box-shadow:0 2px 8px rgba(0,0,0,.4)',
          ].join(';');
          box.appendChild(tag);
          layer.appendChild(box);
        }
        (document.body || document.documentElement).appendChild(layer);
        // Self-heal: some sites (a React/Next route-transition re-render) detach
        // injected nodes on their next reconcile, so the box flashed once then
        // vanished mid-clip. Re-attach it for a bounded window so it stays on
        // camera through the hold. Auto-stops; the box-removal sites clear it.
        try { clearInterval(window.__reproitBoxHeal); } catch (_) {}
        let heals = 0;
        window.__reproitBoxHeal = setInterval(() => {
          if (!document.getElementById('__reproit_boxes')) {
            (document.body || document.documentElement).appendChild(layer);
          }
          if (++heals >= 24) { clearInterval(window.__reproitBoxHeal); window.__reproitBoxHeal = null; }
        }, 150);
        return chosen.length > 0;
      },
      {
        trigger: hints.triggerLabel || null,
        flickerKeys: hints.flickerKeys || null,
        oracle: hints.oracle || null,
        linkHref: hints.linkHref || null,
      }
    )
    .catch(() => false);
  // TRUST GATE: tell the Rust side whether the box actually drew, so a clip that
  // did not reproduce the finding on this load is dropped rather than shipped
  // with a misleading caption.
  log('FINDING:BOXED ' + JSON.stringify({ oracle: hints.oracle || null, drew: !!drew }));
  return !!drew;
}

// ---- COMPONENT-CHOICE differential fuzzing ----
// A multi-choice component (language tabs, a radio group) where EVERY choice has
// a similar effect (the common, expected behavior) but ONE choice deviates is a
// real bug. We exhaustively select each option and flag the one whose effect on
// the GLOBAL layout (the page OUTSIDE the component) is an OUTLIER vs its
// siblings - differential, not an absolute floor. If all choices behave alike
// (every language merely resizes the code block), NOTHING is flagged. This is
// what catches "only Go shifts the whole page" without the false positives an
// absolute layout-shift threshold produced.
// CHOICE_OUTLIER_RATIO / CHOICE_MIN_MAGNITUDE come from ./choice-oracle.mjs (the
// single source of truth shared with the electron + tauri ports); only the role
// SET is local here (detectChoiceGroups wants O(1) membership).
const CHOICE_ROLES = new Set(['tab', 'radio', 'menuitemradio']);

// Group the snapshot's choice-role tappables into mutually-exclusive option sets
// (>= 2 options). Scoped by the OWNING choice container (cgrp), so two separate
// tablists/radiogroups on one page are distinct components, not one merged group
// (comparing across independent components produced false outliers). When no
// container owns the options, the role alone is the key (the prior v1 behavior).
function detectChoiceGroups(tappables) {
  const groups = [];
  const claimed = new Set();
  // 1) ARIA choice roles: a set of tab/radio/menuitemradio options, partitioned
  // by `role|owning-container` so independent groups never merge.
  const byRole = new Map();
  for (const t of tappables) {
    if (CHOICE_ROLES.has(t.role)) {
      const key = t.role + '|' + (t.cgrp != null ? t.cgrp : 'role');
      if (!byRole.has(key)) byRole.set(key, []);
      byRole.get(key).push(t);
    }
  }
  for (const opts of byRole.values()) {
    if (opts.length >= 2) {
      groups.push({ role: opts[0].role, opts });
      for (const o of opts) claimed.add(o.sel);
    }
  }
  // 2) Button-cluster pickers (no ARIA choice role, e.g. a code-block language
  // switcher rendered as plain buttons): a set of >=3 same-parent, same-role
  // tappables where EXACTLY ONE is selected. The one-of-N selected state is what
  // separates a mutually-exclusive choice group from a row of action buttons
  // (Save/Delete, none selected), so we never blindly tap a Delete.
  const byGrp = new Map();
  for (const t of tappables) {
    // Only plain BUTTONS (links navigate, they are not a choice picker), with a
    // label (a real picker labels every option).
    if (claimed.has(t.sel) || t.role !== 'button' || !t.label || t.grp == null || t.grp < 0) continue;
    if (!byGrp.has(t.grp)) byGrp.set(t.grp, []);
    byGrp.get(t.grp).push(t);
  }
  for (const opts of byGrp.values()) {
    if (opts.length >= 3 && opts.filter((o) => o.selected).length === 1) {
      groups.push({ role: 'button-cluster', opts });
    }
  }
  return groups;
}

// FEATURE 1: native <select> as a multi-choice component. The snapshot maps a
// <select> to a `textfield` role, so detectChoiceGroups (which keys off ARIA
// choice roles / button clusters) never sees it -- the most common real-world
// picker. Here we query the page for visible <select>s with >= 3 enabled
// <option>s and return a choice group per select, keyed by a stable structural
// selector (data-testid > name) so the same picker re-resolves across the
// option-by-option exercise even as the framework re-renders. Each option carries
// its raw `value` (the thing we set on the element), exercised below by setting
// select.value + dispatching change/input so a bound framework reacts. The group
// shape mirrors the ARIA/button groups so exerciseChoiceGroup difffs it with the
// SAME global-layout measurement and the SAME outlier rule; the only difference
// is how an option is selected (set value vs click), branched on group.role.
async function detectSelectGroups(page) {
  const raw = await page
    .evaluate(() => {
      const visible = (el) => {
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0) return false;
        const st = getComputedStyle(el);
        return st.visibility !== 'hidden' && st.display !== 'none';
      };
      const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
      const keyOf = (el) => {
        const tid = (el.getAttribute('data-testid') || el.getAttribute('data-test-id') || '').trim();
        if (tid) return 'testid:' + tid;
        const name = (el.getAttribute('name') || '').trim();
        if (name) return 'name:' + name;
        return null;
      };
      const out = [];
      let nth = -1;
      for (const sel of document.querySelectorAll('select')) {
        nth++;
        if (!visible(sel)) continue;
        const opts = Array.from(sel.options || []).filter((o) => !o.disabled);
        if (opts.length < 3) continue;
        const key = keyOf(sel);
        // Structural selector for replay/exercise: stable key, else document-order
        // index among <select>s (never the visible text), matching the runner's
        // selector grammar.
        const ssel = key ? 'key:' + key : 'tag:select#' + nth;
        out.push({
          ssel,
          orig: sel.value,
          opts: opts.map((o) => ({ value: o.value, label: norm(o.label || o.textContent) || o.value })),
        });
      }
      return out;
    })
    .catch(() => []);
  // One choice group per select. opts carry the option `value` + `label`; `sel`
  // is the option's addressable identity (selectSelector=optionValue) so a
  // recorded clip / dedup key is stable and locale-invariant.
  return raw.map((s) => ({
    role: 'select',
    selectSel: s.ssel,
    orig: s.orig,
    opts: s.opts.map((o) => ({ sel: s.ssel + '=' + o.value, value: o.value, label: o.label })),
  }));
}

// Set a native <select>'s value by structural selector (key:<...> or
// tag:select#<idx>) and dispatch input+change so frameworks bound to it react.
// Returns true when the select was found and set. Non-destructive aside from the
// value change (restored by exerciseChoiceGroup after the pass).
async function setSelectValue(page, selectSel, value) {
  return await page
    .evaluate(
      ({ selectSel, value }) => {
        const cssEscape = (v) => (window.CSS && CSS.escape ? CSS.escape(v) : String(v).replace(/["\\]/g, '\\$&'));
        let el = null;
        if (selectSel.startsWith('key:')) {
          const body = selectSel.slice(4);
          const ci = body.indexOf(':');
          const kind = ci >= 0 ? body.slice(0, ci) : '';
          const val = ci >= 0 ? body.slice(ci + 1) : body;
          if (kind === 'testid') {
            el = document.querySelector('[data-testid="' + cssEscape(val) + '"]')
              || document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
          } else if (kind === 'id') el = document.getElementById(val);
          else if (kind === 'name') el = document.querySelector('select[name="' + cssEscape(val) + '"]');
        } else if (selectSel.startsWith('tag:select#')) {
          const idx = parseInt(selectSel.slice('tag:select#'.length), 10);
          const all = document.querySelectorAll('select');
          el = idx >= 0 && idx < all.length ? all[idx] : null;
        }
        if (!el || el.tagName.toLowerCase() !== 'select') return false;
        el.scrollIntoView({ block: 'center', inline: 'center' });
        el.value = value;
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        return true;
      },
      { selectSel, value }
    )
    .catch(() => false);
}

// Capture a GLOBAL-layout fingerprint: page horizontal overflow + the positions
// of PERSISTENT (fixed/sticky) chrome anchors. The point is to measure a choice's
// effect that BREAKS the shared page geometry, not a choice that legitimately
// swaps content of a different height. A content-switching picker (a category /
// preview tab set) makes each choice grow the page to a DIFFERENT height, which
// pushes flow content (an h1/h2/footer below the fold) by different amounts --
// that is EXPECTED, not a bug, and was the choice-anomaly false positive. So the
// anchors are restricted to FIXED/STICKY chrome (a header/nav bar that must not
// move regardless of which choice is active); ordinary flow content is not an
// anchor. The horizontal-overflow term stays: a choice that shoves the page into
// horizontal overflow (a real layout break) is still caught (and is what the
// unit-test fixture's "Broken" option trips).
async function measureGlobalLayout(page) {
  return await page
    .evaluate(() => {
      // Measure from a FIXED scroll (top): the choice exercise scrolls each option
      // into view, and on a lazy-loading page different scroll depths load different
      // amounts of content, drifting far-down anchors by thousands of px between
      // options (a progressive-load artifact, not a reflow). Pinning scroll to 0
      // gives every option the same lazy-load state; only the above-the-fold hero
      // (where a taller pane's shift actually shows) is anchored below. Force an
      // INSTANT jump: many sites set CSS `scroll-behavior:smooth`, under which a
      // plain scrollTo animates and the rects below are read MID-SCROLL, which
      // shifts the "above-fold" set and injects huge phantom deltas.
      try { window.scrollTo({ top: 0, left: 0, behavior: 'instant' }); } catch (_) { window.scrollTo(0, 0); }
      document.documentElement.scrollTop = 0;
      const de = document.documentElement;
      const anchors = [];
      // PINNED chrome only (fixed, or sticky while actually stuck), in VIEWPORT
      // coords: pinned chrome must not move in the viewport regardless of the
      // active choice, and viewport position is scroll-invariant. Page-absolute
      // coords (the previous fingerprint) are scroll-DEPENDENT for fixed chrome
      // (`rect.top + scrollY`), so a synced code-language picker that changed
      // content height above the component moved scrollY and read as "the header
      // moved 33px", blaming an innocent option (measured on a docs quickstart
      // page). Unpinned sticky is ordinary flow content: skipped. Anchors are
      // keyed by tag + query index so the stuck-state filter cannot misalign the
      // key-matched delta.
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
      // FLOW-content landmarks in DOCUMENT-absolute coords (scroll-invariant). THE
      // signal document.scrollHeight misses: when one option's pane is taller (the
      // code-language case -- Go's sample ~60px taller than siblings), the total
      // scrollHeight may barely grow (trailing whitespace / a height-coupled hero
      // row absorbs it) yet every heading BELOW the picker visibly shifts down.
      // Keyed by tag + clipped text (stable across a language switch: CODE text
      // changes, headings do not), so the by-key delta compares the same element.
      // Summed displacement over many shifted headings makes the outlier tower over
      // its ~0-shift siblings. Bounded for determinism; pinned chrome is measured
      // above in VIEWPORT coords so a scroll change injects no phantom delta. Kept
      // byte-identical to choice-oracle.mjs measureGlobalLayoutInPage.
      const seen = {};
      const vh = window.innerHeight || 800;
      const marks = document.querySelectorAll('h1,h2,h3,h4,h5,h6,[role=heading]');
      for (let i = 0; i < marks.length && anchors.length < 40; i++) {
        const el = marks[i];
        const cs = getComputedStyle(el);
        if (cs.position === 'fixed' || cs.position === 'sticky') continue;
        const r = el.getBoundingClientRect();
        if (r.width <= 0 || r.height <= 0) continue;
        // Above-the-fold only (scroll is pinned to 0): a taller pane pushes these
        // hero headings down; below-fold headings are lazy/accumulating, so excluded.
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
    })
    .catch(() => null);
}

// layoutDelta (global-layout move between two fingerprints) and medianOf are
// imported from ./choice-oracle.mjs so the web reference and the electron/tauri
// ports difference identically.

// Exhaustively select each option of a choice group, measure its effect on the
// global layout, and emit at most one EXPLORE:CHOICEBUG for the outlier (a choice
// whose effect is >= CHOICE_OUTLIER_RATIO x the median of its siblings AND at
// least CHOICE_MIN_MAGNITUDE px). Needs >= 3 options so >= 2 siblings define the
// norm. The caller re-observes afterward (the last option is left selected).
// Select one option by its accessible label (scroll into view + click), robust
// to below-fold pickers and to the positional selectors going stale as the
// picker re-renders between choices. Returns true if an element was clicked.
// Click a choice option by its ACCESSIBLE LABEL, scrolling it into view first
// (below-fold pickers must be exercised). Used as the fallback when the precise
// selector click can't resolve/reach the option (tap's reachability gate rejects
// an off-screen control before it is scrolled in).
async function clickOptionByLabel(page, role, label) {
  if (!label) return false;
  return await page
    .evaluate(
      ({ label }) => {
        const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
        for (const el of document.querySelectorAll('button, [role=button], [role=tab], [role=radio]')) {
          const ll = el.getAttribute('aria-labelledby');
          let name = norm(el.getAttribute('aria-label'));
          if (!name && ll) {
            const ref = document.getElementById(ll.split(/\s+/)[0]);
            if (ref) name = norm(ref.textContent);
          }
          if (!name) name = norm(el.textContent);
          if (name === label) {
            el.scrollIntoView({ block: 'center', inline: 'center' });
            el.click();
            return true;
          }
        }
        return false;
      },
      { label }
    )
    .catch(() => false);
}

// Measure the global layout AFTER IT SETTLES: sample until two consecutive
// fingerprints match (or the cap hits). A choice whose layout effect lands
// asynchronously (lazy-loaded content, fonts, a CSS transition) settles PAST any
// fixed wait; with the old fixed 600ms wait the late shift landed in the NEXT
// option's measurement window and the oracle blamed the wrong sibling (measured:
// a docs code-language picker whose real offender was the option BEFORE the one
// reported). Sampling to stability pins each option's effect to the option that
// caused it, at the same 600ms cost in the common already-stable case.
async function measureSettledLayout(page) {
  await page.waitForTimeout(300);
  let prev = await measureGlobalLayout(page);
  for (let waited = 300; waited < 2400; waited += 300) {
    await page.waitForTimeout(300);
    const cur = await measureGlobalLayout(page);
    if (cur && prev && layoutDelta(prev, cur) === 0) return cur;
    prev = cur;
  }
  return prev;
}

// Select one option of a choice group. A native <select> (FEATURE 1) is driven
// by setting its .value + dispatching change/input (no element to click); every
// other group kind clicks the option element. Prefer the EXACT option by its
// structural selector (so two groups sharing an option label don't
// cross-exercise each other's components), but fall back to the label click when
// the precise selector can't be reached -- tap()'s reachability gate rejects a
// below-fold picker before clickOptionByLabel scrolls it into view.
async function pickChoiceOption(page, group, opt) {
  return group.role === 'select'
    ? await setSelectValue(page, group.selectSel, opt.value)
    : ((await tap(page, opt.sel)) || (await clickOptionByLabel(page, group.role, opt.label)));
}

async function exerciseChoiceGroup(page, group, fromSig, keepBox = false) {
  // FIRST PASS: select each option in turn and capture its SETTLED ABSOLUTE
  // layout fingerprint (sampled to stability). Absolute per-option states, not
  // deltas: a late-settling shift lands inside its own option's settled
  // fingerprint, and no baseline choice can hide or misattribute anything.
  const results = [];
  for (const opt of group.opts) {
    const ok = await pickChoiceOption(page, group, opt);
    results.push({ opt, fp: ok ? await measureSettledLayout(page) : null });
  }
  const valid = results.filter((r) => r.fp);
  if (valid.length < 3) {
    if (group.role === 'select' && group.selectSel) {
      await setSelectValue(page, group.selectSel, group.orig);
    }
    return false; // need >= 2 siblings to call one an outlier
  }
  // NORM: the MEDOID fingerprint (the option whose layout is most like the
  // others) is the group's typical page geometry; each option's magnitude is
  // its distance from that norm. The pack of ordinary options defines the
  // median deviation, so a picker whose panes all differ by comparable amounts
  // stays quiet, while a genuine odd-one-out (one language whose selection
  // reflows the whole page while its siblings sit within px of each other)
  // towers over the median and fires.
  let medoid = valid[0];
  let bestSum = Infinity;
  for (const r of valid) {
    let s = 0;
    for (const o of valid) if (o !== r) s += layoutDelta(r.fp, o.fp);
    if (s < bestSum) {
      bestSum = s;
      medoid = r;
    }
  }
  for (const r of valid) r.mag = layoutDelta(medoid.fp, r.fp);
  const siblingMedFor = (cand) =>
    medianOf(valid.filter((o) => o !== cand).map((o) => o.mag));
  const candidates = valid
    .filter((r) => {
      if (r === medoid || r.mag < CHOICE_MIN_MAGNITUDE) return false;
      return r.mag >= CHOICE_OUTLIER_RATIO * Math.max(siblingMedFor(r), 1);
    })
    .sort((a, b) => b.mag - a.mag);
  // CAUSAL CONFIRMATION: the first pass attributes; only a controlled A/B
  // re-toggle PROVES ownership. For EACH candidate: park the group on the
  // medoid (the typical layout), settle, then select the candidate, settle --
  // the candidate owns a bug only if the deviation FOLLOWS it in this isolated
  // pair. Every candidate that confirms is reported (a picker can have more
  // than one odd-one-out option, each with its own real magnitude); the clip
  // boxes the largest. This is what stops a slow async shift from convicting
  // an innocent neighbor, and it doubles as the reproducibility check the
  // recorded clip relies on.
  const confirmed = [];
  for (const cand of candidates) {
    if (!(await pickChoiceOption(page, group, medoid.opt))) continue;
    const a = await measureSettledLayout(page);
    if (!(await pickChoiceOption(page, group, cand.opt))) continue;
    const b = await measureSettledLayout(page);
    const mag = a && b ? layoutDelta(a, b) : null;
    const med = siblingMedFor(cand);
    if (mag !== null && mag >= CHOICE_MIN_MAGNITUDE && mag >= CHOICE_OUTLIER_RATIO * Math.max(med, 1)) {
      confirmed.push({ opt: cand.opt, mag, med });
    }
  }
  confirmed.sort((a, b) => b.mag - a.mag);
  const max = confirmed[0] || null;
  // FEATURE 1 restore: a native <select> is left on the last exercised option
  // above, so put it back to its original value (non-destructive, like the rest
  // of the oracle). ARIA/button groups are left selected by design (the caller
  // re-observes the resulting state); a hidden form value is not a navigable
  // state, so it is restored instead.
  if (group.role === 'select' && group.selectSel) {
    await setSelectValue(page, group.selectSel, group.orig);
  }
  const isOutlier = !!max;
  if (isOutlier) {
    for (const c of confirmed) {
      log(
        'EXPLORE:CHOICEBUG ' +
          JSON.stringify({
            from: fromSig,
            role: group.role,
            outlier: c.opt.label || c.opt.sel,
            sel: c.opt.sel,
            magnitude: Math.round(c.mag),
            siblingMedian: Math.round(c.med),
          })
      );
    }
    // Recorded fuzz walk (`fuzz --record`): re-select the outlier and box it so
    // the clip shows WHICH choice shifts the page - the differential finding made
    // visible. Unlike the other oracles this fires during the fuzz walk (the
    // exercise is fuzz-only), so it draws here, holds, then cleans up so the rest
    // of the walk is untouched. Reuses the trigger path of drawFindingBoxes (the
    // boxed outlier, plus any overflow the shift causes).
    if (VIDEO_DIR) {
      // A native <select> outlier: re-set the select to the outlier value and tag
      // the SELECT element so the box lands on the picker that shifted the page.
      let tapped = false;
      if (group.role === 'select' && group.selectSel) {
        await setSelectValue(page, group.selectSel, max.opt.value);
        tapped = await page
          .evaluate(({ selectSel }) => {
            const cssEscape = (v) => (window.CSS && CSS.escape ? CSS.escape(v) : String(v).replace(/["\\]/g, '\\$&'));
            let el = null;
            if (selectSel.startsWith('key:')) {
              const body = selectSel.slice(4);
              const ci = body.indexOf(':');
              const kind = ci >= 0 ? body.slice(0, ci) : '';
              const val = ci >= 0 ? body.slice(ci + 1) : body;
              if (kind === 'testid') el = document.querySelector('[data-testid="' + cssEscape(val) + '"]') || document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
              else if (kind === 'id') el = document.getElementById(val);
              else if (kind === 'name') el = document.querySelector('select[name="' + cssEscape(val) + '"]');
            } else if (selectSel.startsWith('tag:select#')) {
              const idx = parseInt(selectSel.slice('tag:select#'.length), 10);
              const all = document.querySelectorAll('select');
              el = idx >= 0 && idx < all.length ? all[idx] : null;
            }
            if (!el) return false;
            for (const e of document.querySelectorAll('[data-reproit-trigger]')) e.removeAttribute('data-reproit-trigger');
            el.setAttribute('data-reproit-trigger', '1');
            return true;
          }, { selectSel: group.selectSel })
          .catch(() => false);
      } else {
        // Re-select the EXACT outlier by selector and tag it (mark) so the box lands
        // on the choice that shifted the page, not a same-label sibling. Fall back
        // to the label click + a manual trigger tag when the selector can't be
        // reached (below-fold), so the clip still boxes the right control.
        tapped = await tap(page, max.opt.sel, { mark: true });
      }
      if (!tapped) {
        const label = max.opt.label || max.opt.sel;
        await clickOptionByLabel(page, group.role, label);
        await page
          .evaluate(({ label }) => {
            const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
            for (const e of document.querySelectorAll('[data-reproit-trigger]')) e.removeAttribute('data-reproit-trigger');
            for (const el of document.querySelectorAll('button, [role=button], [role=tab], [role=radio]')) {
              const ll = el.getAttribute('aria-labelledby');
              let name = norm(el.getAttribute('aria-label'));
              if (!name && ll) { const ref = document.getElementById(ll.split(/\s+/)[0]); if (ref) name = norm(ref.textContent); }
              if (!name) name = norm(el.textContent);
              if (name === label) { el.setAttribute('data-reproit-trigger', '1'); break; }
            }
          }, { label })
          .catch(() => {});
      }
      await page.waitForTimeout(500);
      await drawFindingBoxes(page, {
        triggerLabel: 'layout shift +' + Math.round(max.mag) + 'px',
        oracle: 'no-choice-anomaly',
      }).catch(() => {});
      await page.waitForTimeout(2200);
      // A scan clip (`keepBox`) ends on the boxed outlier, so the cleanup that a
      // mid-walk exercise does is skipped; the caller holds + finishes the clip.
      if (!keepBox) {
        await page
          .evaluate(() => {
            try { clearInterval(window.__reproitBoxHeal); } catch (_) {}
            const b = document.getElementById('__reproit_boxes'); if (b) b.remove();
            for (const e of document.querySelectorAll('[data-reproit-trigger]')) e.removeAttribute('data-reproit-trigger');
          })
          .catch(() => {});
      }
      return true;
    }
    return false;
  }
}

async function main() {
  console.log(`JOURNEY[a] step: engine=${ENGINE}`);
  const browser = await launchBrowser({ headless: HEADLESS });
  // Multi-actor scenario: this process plays one actor, pulling from the conductor.
  if (process.env.REPROIT_SCENARIO_BARRIER) {
    log('JOURNEY[a] step: scenario actor=' + (process.env.REPROIT_DEVICE || 'a'));
    await runScenarioActor(browser);
    await browser.close();
    return;
  }
  // Build the context options: video (optional) plus the run locale (optional).
  // Setting `locale` makes Playwright override navigator.language/languages AND
  // send a matching Accept-Language header, so both client-side i18n and
  // server-side content negotiation render the page in the requested language.
  // Scoped to this context (and so to this run).
  // Pin the viewport, device scale, and locale so the layout-sensitive oracles
  // (overflow, content) and rendered text metrics are STABLE across machines,
  // CI runners, and Playwright-default changes: a repro captured here must not
  // appear or vanish on a customer's CI. The sibling runners
  // (differential/jank/annotate) already pin these; the main runner was the
  // outlier. Defaults match Playwright's current default viewport (so no golden
  // drift today) and a canonical en-US locale; both are env-overridable for
  // responsive / i18n runs.
  const VW = Number(process.env.REPROIT_VIEWPORT_W) || 1280;
  const VH = Number(process.env.REPROIT_VIEWPORT_H) || 720;
  const effectiveLocale = LOCALE || 'en-US';
  // Identifiable scanner UA: the real browser UA (read from a throwaway context so
  // it is never hardcoded) plus the reproit token, unless the caller overrode it
  // via --header "User-Agent: ...".
  let scannerUA = UA_OVERRIDE;
  if (!scannerUA) {
    try {
      const uaCtx = await browser.newContext();
      const uaPage = await uaCtx.newPage();
      const baseUA = await uaPage.evaluate(() => navigator.userAgent).catch(() => '');
      await uaCtx.close();
      if (baseUA) scannerUA = baseUA + ' ' + REPROIT_UA_TOKEN;
    } catch (_) {}
  }
  const contextOpts = {
    viewport: { width: VW, height: VH },
    deviceScaleFactor: 1,
    locale: effectiveLocale,
    // Accept-Language first, then any --header passthrough (which may override it
    // or add clearance/auth headers). Header names are sent as given.
    extraHTTPHeaders: {
      'Accept-Language': `${effectiveLocale},${effectiveLocale.split('-')[0]};q=0.9`,
      ...EXTRA_HEADERS,
    },
    // Capture determinism: emulate prefers-reduced-motion: reduce for the whole
    // context, pinning animation-dependent layout so snapshots/pixels are stable
    // across runs for the other oracles.
    reducedMotion: 'reduce',
  };
  if (scannerUA) contextOpts.userAgent = scannerUA;
  if (VIDEO_DIR) contextOpts.recordVideo = { dir: VIDEO_DIR, size: { width: VW, height: VH } };
  if (LOCALE) console.log(`JOURNEY[a] step: locale=${LOCALE}`);
  const context = await browser.newContext(contextOpts);
  await installCapsuleReplay(context);
  await installWebSocketCausal(context);
  const page = await context.newPage();
  // CDP session for ground-truth operability (DOMDebugger.getEventListeners):
  // detects real click/pointer listeners on elements and the document/body
  // delegation pattern. Chromium-only; firefox/webkit have no CDP, so the
  // ground-truth falls back to native + cursor + delegation-marker signals.
  let gtCdp = null;
  if (ENGINE === 'chromium') {
    try { gtCdp = await context.newCDPSession(page); } catch (e) { gtCdp = null; }
    // JANK hardening: enable the CDP Performance domain so we can read
    // LayoutCount/RecalcStyleCount. The DELTA of forced synchronous layouts
    // around an action is a machine-INVARIANT jank signal (300 forced layouts is
    // 300 on any runner), unlike the wall-clock stall. Chromium-only; best-effort.
    if (gtCdp) { try { await gtCdp.send('Performance.enable'); } catch (_) {} }
  }

  // Exception oracle: uncaught page errors (a throw in an onclick, an
  // unhandled rejection) become the same EXCEPTION block the Flutter
  // pipeline emits, so the fuzz oracle and exceptions.jsonl pick them up.
  // `replayErrorCount` lets a recorded replay know a (kept) crash fired so the
  // finding box labels the triggering element "crash".
  let replayErrorCount = 0;
  const emitError = (err) => {
    const msg = String(err && err.message ? err.message : err);
    // Skip third-party-script throws and known-benign browser-policy errors.
    if (exceptionIsBenign(msg) || exceptionThrownInTracker(err && err.stack) || exceptionIsNonDeterministic(msg, err && err.stack) || !exceptionIsFirstParty(err && err.stack, APP_ORIGIN)) return;
    replayErrorCount++;
    log('EXCEPTION CAUGHT BY WEB PAGE');
    log('The following error was thrown:');
    log(msg);
    const stack = (err && err.stack) ? String(err.stack) : '';
    for (const line of stack.split('\n').slice(0, 8)) log(line);
    log('\u2550\u2550\u2550\u2550\u2550\u2550\u2550\u2550');
  };
  page.on('pageerror', emitError);
  // A renderer/GPU/OOM crash raises Playwright's `crash` event, NOT `pageerror`.
  // Without this the next action throws inside the runner and is misattributed to
  // the runner ("EXCEPTION CAUGHT BY WEB RUNNER") instead of the app. Emit the
  // same app-crash block and bump the counter so a recorded replay boxes it.
  page.on('crash', () => {
    replayErrorCount++;
    log('EXCEPTION CAUGHT BY WEB PAGE');
    log('The following error was thrown:');
    log('the page crashed (renderer process gone -- GPU / out-of-memory / sad-tab)');
    log('════════');
  });

  // BROKEN-ROUTE oracle: record the HTTP status of main-frame DOCUMENT
  // navigations, keyed by URL pathname. A state whose document came back >= 400
  // is a dead route the app linked to (a 404/5xx). The status is structural and
  // locale-invariant, and a 4xx/5xx is never an intended screen, so this is
  // false-positive-free. Same-origin only (off-site links are handled elsewhere).
  const navStatus = {};
  const causalRequests = new WeakMap();
  page.on('response', async (resp) => {
    try {
      const req = resp.request();
      const causal = causalRequests.get(req);
      if (causal && NETWORK_FILE) {
        const headers = await resp.allHeaders().catch(() => ({}));
        const contentType = headers['content-type'] || '';
        let body;
        if (/text\/event-stream/i.test(contentType)) {
          const raw = await resp.text().catch(() => '');
          const sse = redactSse(raw);
          body = sse.body;
          if (!sse.supported) log('REPROIT:CAPABILITIES {"sse":{"status":"unsupported","detail":"non-JSON event cannot be safely persisted"},"sse_replay":{"status":"unsupported","detail":"non-JSON event cannot be safely persisted"}}');
        } else if (/json/i.test(contentType)) {
          const raw = await resp.text().catch(() => '');
          body = parseNetworkBody(raw, contentType);
        } else {
          const len = headers['content-length'];
          body = len ? `<reproit:body:length=${len}>` : undefined;
        }
        appendNetworkFact({
          version: 1, type: 'exchange', id: causal.id, actor: NETWORK_ACTOR,
          actionIndex: Math.max(causal.actionIndex, 0), ordinal: causal.ordinal,
          protocol: /text\/event-stream/i.test(contentType) ? 'sse' : new URL(resp.url()).protocol.replace(':', ''), method: req.method(), url: resp.url(),
          requestHeaders: causal.headers, requestBody: causal.body,
          status: resp.status(), responseHeaders: redactNetworkHeaders(headers), responseBody: body,
          required: true,
        });
      }
      if (req.frame() !== page.mainFrame() || req.resourceType() !== 'document') return;
      const u = new URL(resp.url());
      if (u.origin !== APP_ORIGIN) return;
      navStatus[normalizePathname(u.pathname)] = resp.status();
    } catch (e) { /* ignore */ }
  });

  // DUPLICATE-SUBMIT probe support, OPT-IN per run via REPROIT_DUPSUBMIT=1:
  // double-firing real submit actions during a walk changes exploration
  // semantics (an order really is placed twice), so the probe never runs
  // unless the operator asked for it. While a tap probe is armed (dupReqLog
  // non-null, set in the tap branch), every first-party non-GET request in the
  // window between the first click and the settle is recorded as "METHOD url";
  // the tap branch groups them and reports a pair that fired twice. A
  // page-level listener (not in-page patching) so plain form submissions count
  // exactly like fetch/XHR. null = disarmed, zero overhead on a normal walk.
  const DUPSUBMIT = process.env.REPROIT_DUPSUBMIT === '1';
  // LISTENER-LEAK probe support, OPT-IN per run via REPROIT_LISTENERLEAK=1:
  // driving repeated route revisits (history back/forward loops) changes
  // exploration semantics and adds navigation cost, so like the duplicate-submit
  // probe it never runs unless the operator asked for it. When on, an init script
  // wraps add/removeEventListener at page load so the live listener count is
  // available for the revisit samples.
  const LISTENERLEAK = process.env.REPROIT_LISTENERLEAK === '1';
  let dupReqLog = null;
  page.on('request', (req) => {
    if (NETWORK_FILE) {
      try {
        if (['xhr', 'fetch', 'eventsource'].includes(req.resourceType())) {
          const headers = req.headers();
          const ordinal = causalOrdinal++;
          causalRequests.set(req, {
            id: `${NETWORK_ACTOR}-${causalActionIndex}-${ordinal}`,
            actionIndex: causalActionIndex,
            ordinal,
            headers: redactNetworkHeaders(headers),
            body: parseNetworkBody(req.postData(), headers['content-type'] || ''),
          });
        }
      } catch (_) {}
    }
    if (!dupReqLog) return;
    try {
      const method = req.method();
      if (method === 'GET') return;
      if (new URL(req.url()).origin !== APP_ORIGIN) return;
      dupReqLog.push(method + ' ' + req.url());
    } catch (e) { /* ignore */ }
  });

  // Install the Long Tasks observer (jank/hang watchdog) BEFORE the first
  // navigation so it is live for every action. addInitScript re-runs it on every
  // document, so it survives in-app navigations and reloads.
  await installLongTaskObserver(page);
  // Install the cross-engine rAF frame-interval recorder too. On firefox/webkit
  // (no Long Tasks API) it is the ONLY jank/hang signal; on chromium it is unused
  // (the precise Long Tasks path is kept), but installing it everywhere keeps the
  // page setup uniform.
  await installFrameObserver(page);
  // LISTENER-LEAK counter (opt-in): wrap add/removeEventListener as an INIT
  // script so it is installed before any page script on every document and its
  // tally survives client-side navigations (the leak surface). Must precede the
  // first goto below so the initial load is instrumented too.
  if (LISTENERLEAK) await page.addInitScript(installListenerLeakCounter);

  // Ready marker so the orchestrator starts its clock; matches the Dart
  // explorer's claim line.
  log('JOURNEY claimed role=a');
  // A `scan --record` clip pins the START url so it lands directly on the
  // finding's screen (a faithful, hand-followable "open this URL"), instead of
  // replaying drifty positional taps. Same-origin as APP_URL, so the off-origin
  // guards still hold. Absent for a normal run -> the app's start URL.
  const START_URL = loadFuzz().gotoUrl || APP_URL;
  const startResponse = await page.goto(START_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => null);
  await page.waitForTimeout(800);

  // BOT-WALL guard: if the landing page is a WAF challenge interstitial, reproit
  // never reached the app. Report the scan UNSCANNABLE with a clear remediation
  // and emit NO oracle findings (the completion markers still fire so the run
  // reads as a clean, complete pass with zero findings, not a cut-short crawl).
  const wall = await detectBotWall(page);
  if (wall) {
    const diag = `target is behind a ${wall.vendor} bot-challenge (${wall.marker}); reproit could not reach the app. `
      + `Allowlist the reproit User-Agent ("${REPROIT_UA_TOKEN}") in your WAF, run reproit against your dev/staging build, `
      + `or pass --header "Cookie: cf_clearance=..." to inject a clearance token.`;
    log('EXPLORE:UNSCANNABLE ' + JSON.stringify({ reason: 'bot-wall', vendor: wall.vendor, marker: wall.marker, diagnostic: diag }));
    log('JOURNEY[a] step: UNSCANNABLE - ' + diag);
    log('JOURNEY DONE');
    log('All tests passed');
    try { await browser.close(); } catch (_) {}
    return;
  }

  // Layer-3 opt-in value-node selectors from reproit.yaml (empty if none).
  const valueNodeSelectors = loadValueNodes();
  if (valueNodeSelectors.length) log(`JOURNEY[a] step: value_nodes=${valueNodeSelectors.length}`);

  // Layer-1 hard cap (docs/signature.md "Value-state"): per structural node,
  // track the DISTINCT value-class combinations seen. Once a node exceeds
  // VALUE_CLASS_CAP, fall back to its structural-only signature for the rest of
  // the run so an adversarial value generator cannot explode the graph. The cap
  // is SESSION-wide (every seed): an adversarial value generator cannot evade it
  // by resetting between seeds, matching the other runners' contract.
  const valueCombos = new Map();   // structuralSig -> Set of V: sections
  const cappedNodes = new Set();   // structuralSig that hit the cap
  // The EFFECTIVE signature for a snapshot, applying the runner-local cap: the
  // full value-folded sig unless this structural node is capped, then structural.
  function effectiveSig(snap) {
    if (cappedNodes.has(snap.structuralSig)) return snap.structuralSig;
    if (snap.vsection) {
      let set = valueCombos.get(snap.structuralSig);
      if (!set) { set = new Set(); valueCombos.set(snap.structuralSig, set); }
      set.add(snap.vsection);
      if (set.size > VALUE_CLASS_CAP) {
        cappedNodes.add(snap.structuralSig);
        log(`JOURNEY[a] step: value-cap hit (${snap.structuralSig})`);
        return snap.structuralSig;
      }
    }
    return snap.sig;
  }

  // If an action navigated the browser off the app-under-test's origin (a
  // footer "View on GitHub", a social/outbound link), that destination is NOT
  // a state of the app: recording it would make the whole map + every fuzz
  // finding about the foreign site. Recover by going back; if that fails to
  // return us on-origin, re-goto the app URL. Mirrors the back-path recovery.
  // Returns true if a recovery was performed (caller should not record state).
  async function recoverIfOffOrigin() {
    let url = '';
    try { url = page.url(); } catch (e) {}
    let off = false;
    try { off = new URL(url).origin !== APP_ORIGIN; } catch (e) { off = true; }
    if (!off) return false;
    await page.goBack({ timeout: 3000 }).catch(() => {});
    await page.waitForTimeout(400);
    let back = '';
    try { back = page.url(); } catch (e) {}
    let stillOff = true;
    try { stillOff = new URL(back).origin !== APP_ORIGIN; } catch (e) { stillOff = true; }
    if (stillOff) {
      await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
      await page.waitForTimeout(400);
    }
    return true;
  }

  // Re-pump a fresh starting screen between seeds. The Flutter explorer rebuilds
  // a clean widget tree per seed; the web analogue is to navigate back to the
  // app start URL so each seed begins from the same clean state. Session-wide
  // (browser/context/page + the value cap) survives; per-seed state does not.
  async function resetToRoot() {
    // Re-navigating alone does NOT reset a state-persisting app: a TodoMVC-style
    // list kept in localStorage (or sessionStorage / IndexedDB) survives the
    // reload, so a later seed inherits an earlier seed's state and a kept repro
    // diverges on its own re-check. Land on the app origin first, CLEAR the
    // client-side stores, then re-load so the app boots from a clean slate. An
    // app that exposes window.__reproitReset() (a server-backed / custom reset)
    // gets it called too, so that convention stays compatible.
    await page.goto(APP_URL, { waitUntil: 'domcontentloaded', timeout: 8000 }).catch(() => {});
    await clearClientStorage(page);
    await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
    await page.waitForTimeout(500);
  }

  // Explore/replay ONE seed, emitting the same EXPLORE:STATE / EXPLORE:EDGE /
  // FUZZ:ACT / FUZZ:MISS markers as a single-seed run. Seen states + tried edges
  // are LOCAL to the seed so per-seed coverage is independent, matching the other
  // runners' per-seed contract (runners/rn, runners/linux-atspi.py run_seed).
  async function runSeed(fuzz) {
    const seenStates = new Set();
    // ZOOM-REFLOW (WCAG 1.4.10): anchors (routes) already re-rendered at 200%
    // zoom, so each distinct route is checked once. See zoomReflowCheck below.
    const zoomChecked = new Set();
    // ROTATION / BACKGROUND-RESTORE (lifecycle-metamorphic): each distinct state
    // sig is transform-tested once. See rotationCheck / backgroundCheck below.
    const rotChecked = new Set();
    const bgChecked = new Set();
    // LISTENER-LEAK: anchors (routes) already revisit-probed, so each distinct
    // route is checked once. See listenerLeakCheck below (opt-in).
    const leakChecked = new Set();
    const triedEdges = new Set();
    // DUPLICATE-SUBMIT probe: (from sig, action) pairs already double-
    // dispatched this seed, so each submit-like control is probed (and
    // reported) at most once.
    const dupProbed = new Set();
    const actionsByState = new Map();
    const graph = new Map();
    let launchSig = null;
    // Same-origin link targets SEEN during the crawl (pathname -> source state
    // sig), HEAD-probed for dead links at the end. Coverage is bounded, so a dead
    // link the walk never tapped (a footer /download 404) was missed when
    // broken-route relied only on actual navigations.
    const seenLinks = new Map();
    const exercisedGroups = new Set(); // choice-groups already differential-tested this seed
    const pick = rng(fuzz.seed || 0);
    const replay = fuzz.replay || null;
    // Finding-highlight hints for a recorded replay: the most recent action's
    // transition-level signals, so the end-of-replay box can point at what broke.
    const recording = !!(replay && VIDEO_DIR);
    const crashAtStart = replayErrorCount;
    let lastTriggerLabel = null; // 'jank' / 'froze' from the latest action (crash overrides)
    let lastFlickerKeys = null;  // churned persistent-chrome anchor keys, latest action
    // Property-matched fixture inputs for this seed (field -> concrete value).
    // Empty unless the config carries `inputs`; when present, a matching `type:`
    // action types the provided value instead of the adversarial-class token.
    const inputs = loadInputs(fuzz);
    if (fuzz.seed) log(`JOURNEY[a] step: fuzz seed=${fuzz.seed}`);

    // The state + action that triggered the CURRENT navigation, so a broken-route
    // landed on by tapping a link is attributed to the exact SOURCE page and link
    // (not reverse-matched by destination, which is arbitrary when several pages
    // link to the same dead route). Set right before each navigating tap; null for
    // the initial load (the start URL has no in-app source).
    let lastNav = null;

    async function observe() {
      const snap = await snapshot(page, valueNodeSelectors);
      snap.sig = effectiveSig(snap);
      // In replay, emit the current state after every action so a journey's
      // `expect: state` can verify the path positionally (explore dedups
      // EXPLORE:STATE, which loses revisited / per-step states).
      if (replay) log('FUZZ:STATE ' + snap.sig);
      if (!seenStates.has(snap.sig)) {
        seenStates.add(snap.sig);
        // sig: STRUCTURAL (roles + tree shape + stable developer keys),
        //      locale-invariant.
        // labels: DISPLAY-ONLY visible text (map show), never in the sig.
        // elements: structural selectors for replay; `nokey` flags a tappable
        //           with no explicit author key (data-testid/name) so the map layer can
        //           warn the developer to add one.
        log('EXPLORE:STATE ' + JSON.stringify({
          sig: snap.sig,
          // route: the URL path, so the candidate map can reconcile by route
          // (the reliable, framework-neutral join key) and not just by name.
          ...(snap.anchor ? { route: snap.anchor } : {}),
          labels: snap.labels.slice(0, 24),
          elements: snap.tappables.slice(0, 24).map((e) => {
            const o = { sel: e.sel, role: e.role, label: e.label };
            if (e.purpose) o.inputPurpose = e.purpose;
            if (e.bounds) o.bounds = e.bounds;
            if (!e.key) o.nokey = true;
            return o;
          }),
          texts: (snap.texts || []).slice(0, 48),
        }));
        // Evidence recording is not another audit. The scan already found and
        // classified the bug; this run exists only to film that reproduction.
        // Skip state-audit probes here because some are intentionally invasive:
        // the 60-step Tab traversal walks focus through the whole document and
        // scrollRoundTripScan drives a scroller away and back. Filming those made
        // a choice-anomaly clip visit the footer before touching its picker.
        if (recording) return snap;
        // The structural oracle scans run on the SAME (un-mutated) DOM the
        // snapshot captured, and crucially BEFORE emitGroundtruth -- whose
        // keyboard-activation probe mutates the DOM and whose framebuffer probe
        // (REPROIT_PROBE=1) RELOADS the page to the start URL. Running them after
        // would scan the reloaded/mutated page yet attribute findings to THIS
        // sig, so a probe run mis-keyed every overflow/content-bug to the wrong
        // state. Order is therefore: scans first, ground-truth (mutating) last.
        //
        // CONTENT-BUG, keyed by the SAME sig. Pure DOM/label scan (no pixels, no
        // timing), so it reproduces on replay. Silent when nothing is broken.
        const cbug = await page.evaluate(detectContentBugs, [...INJECTED_VALUES]).catch(() => null);
        if (cbug && cbug.length) {
          log('EXPLORE:CONTENTBUG ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: cbug }));
        }
        // OCCLUSION: an interactive element that is presented as usable (visible,
        // in the viewport, not aria-hidden/inert) but whose CENTER is covered by a
        // foreign element -- a click there hits the overlay, not the control. The
        // classic case is an invisible leftover backdrop or a z-index accident
        // blocking the UI. Pure hit-test (document.elementFromPoint), deterministic
        // given a fixed viewport, so it re-confirms on replay. FP guards: when a
        // modal is open the background is LEGITIMATELY covered, so we only check
        // elements inside the modal; and we skip hidden/zero-opacity/off-screen
        // controls (not presented as clickable). RE-CONFIRMED: a second scan a
        // beat later must agree (same target+cover), so a transient overlap from
        // an animating menu / mid-scroll dropdown drops out; only a stably buried
        // control survives.
        const occ1 = await page.evaluate(occlusionScan).catch(() => null);
        let occ = occ1;
        if (occ1 && occ1.length) {
          await page.waitForTimeout(300);
          const occ2 = await page.evaluate(occlusionScan).catch(() => null);
          occ = confirmOcclusions(occ1, occ2 || []);
        }
        if (occ && occ.length) {
          log('EXPLORE:OCCLUSION ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: occ }));
        }
        // SECURITY hygiene: pure DOM/URL predicates, deterministic and FP-free.
        //   - tabnabbing: a cross-origin target=_blank link with no rel=noopener
        //     (the opened page can rewrite window.opener.location -- a phishing
        //     vector). Fires on any page.
        //   - insecure-form / mixed-content: an HTTPS document with an http: form
        //     action or http: subresource. Gated on https so an http dev page
        //     never false-positives.
        const sec = await page.evaluate(securityScan).catch(() => null);
        if (sec && sec.length) {
          log('EXPLORE:SECURITY ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: sec }));
        }
        // BLANK-SCREEN: the state rendered NOTHING -- zero visible text nodes,
        // zero tappable controls, zero visible media -- in a non-empty viewport
        // (the white-screen-of-death: an SPA mount that threw before render).
        // observe() runs after the action's settle wait like every scan here,
        // and the scan itself requires a laid-out document.body, so a page
        // still loading never fires. Structural DOM emptiness, no pixels, so it
        // reproduces on replay. Silent when the state shows any content.
        let blank = await page.evaluate(blankScreenScan).catch(() => null);
        // A candidate-blank state may just be a MID-LOAD blank frame (the JS has
        // not populated the DOM yet), which is a transient loading state, NOT a
        // white-screen-of-death. Settle for content (network idle + DOM quiescence)
        // and re-check: only a state STILL blank AFTER settle fires. The settle is
        // paid ONLY on the rare candidate-blank state, so a normal state is unaffected.
        if (blank && blank.length) {
          await settleForSignature(page);
          blank = await page.evaluate(blankScreenScan).catch(() => null);
        }
        if (blank && blank.length) {
          log('EXPLORE:BLANKSCREEN ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: blank }));
        }
        // APP-INVARIANT: the app's OWN predicates, registered via the SDK
        // (ReproIt.invariant("id", fn), which pushes to the stable global
        // window.__reproit_invariants). Evaluate each on this settled state; a
        // predicate that returns falsy, throws, or an { ok:false, message }
        // object is a violation. The app owns this ground truth, so a reported
        // violation is real (FP-free). Silent when the app registered none or
        // all held. Each test is isolated so one throwing predicate cannot
        // suppress the others.
        const invViolations = await page.evaluate(() => {
          const reg = window.__reproit_invariants || [];
          const out = [];
          for (let i = 0; i < reg.length; i++) {
            const it = reg[i];
            if (!it || typeof it.test !== 'function') continue;
            let ok = true, message = '';
            try {
              const r = it.test();
              if (r && typeof r === 'object') { ok = !!r.ok; message = r.message ? String(r.message) : ''; }
              else { ok = !!r; }
            } catch (e) { ok = false; message = (e && e.message) ? String(e.message) : String(e); }
            if (!ok) out.push({ id: String(it.id), message: message });
          }
          return out;
        }).catch(() => null);
        if (invViolations && invViolations.length) {
          log('EXPLORE:INVARIANT ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: invViolations }));
        }
        // BROKEN-ASSET: dead subresources rendered in this state -- an img that
        // completed with no pixels, a FontFace whose load errored, rendered
        // tofu (a visible U+FFFD). Pure DOM/resource status facts; running
        // after the settle wait means loads have resolved, so a still-loading
        // asset never false-positives. Silent when every asset is healthy.
        const assets = await page.evaluate(brokenAssetScan, [...INJECTED_VALUES]).catch(() => null);
        if (assets && assets.length) {
          log('EXPLORE:BROKENASSET ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: assets }));
        }
        if (!PROBE) {
          // SCROLL ROUND-TRIP: scroll the primary list away and back and flag
          // content that differs at a pinned offset (a list-recycling /
          // virtualization bug rebinds a different row to the same position).
          // Self-restoring; value-state normalized out, so it reproduces on
          // replay. Silent when the list is stable or there is no scroller.
          const srt = await page.evaluate(scrollRoundTripScan).catch(() => null);
          if (srt && srt.length) {
            log('EXPLORE:SCROLLROUNDTRIP ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: srt }));
          }
        }
        // BROKEN-ROUTE: the document for this URL came back with a status that
        // means the resource is GENUINELY GONE -- 404 (not found) or 410 (gone).
        // ONLY those. Not 401/403 (intentional auth gates), 429 (rate limit),
        // 3xx (redirect), 405/501 (method semantics), or 5xx (a transient server
        // error is not a broken LINK) -- flagging any of those was a false
        // positive. Looked up by bare PATHNAME (snap.path), not the signature
        // anchor: a document status is a SERVER concern keyed on the path the
        // request hit, while the SPA hash (#/route) and query string never reach
        // the server. Two URLs that differ only by query thus share one status
        // entry -- a per-query dead route is a known limitation, not distinguished.
        const status = snap.path ? navStatus[snap.path] : undefined;
        if (typeof status === 'number' && isDeadRouteStatus(status)) {
          // SPA SOFT-404 guard: a static host (naiveui, GitHub Pages, Netlify) can
          // answer a deep path with HTTP 404 yet still serve index.html, and the
          // client router renders the CORRECT screen. The runner is standing ON that
          // rendered screen right now, so if it is a real app view (filled mount,
          // real interactive content, no not-found heading) the 404 status is not a
          // broken route. A genuine error page still fails the check and fires.
          const view = await page.evaluate(soft404View).catch(() => null);
          if (!isSoftHandled(view)) {
            log('EXPLORE:BROKENROUTE ' + JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              status,
              // Exact source attribution: the page + link that led here.
              ...(lastNav ? { from: lastNav.from, action: lastNav.action } : {}),
            }));
          }
        }
        // Operability/accessibility ground truth LAST: its keyboard-activation
        // probe mutates the DOM and its framebuffer probe reloads the page, so it
        // must run after the snapshot, the state record, AND the scans above. The
        // next action then drives the live (possibly mutated/reloaded) DOM.
        await emitGroundtruth(page, gtCdp, snap.sig);
      }
      // Record same-origin APP link targets on this page (dedup by pathname, first
      // source state wins) for the end-of-crawl broken-route link check. Exclude
      // non-app links the probe should never fetch: a `download` link (a file
      // download, not a navigable route) and an href whose path ends in a file /
      // asset extension (.zip/.pdf/.dmg/.exe/... plus static web assets). A 404 on
      // an asset is a broken-asset concern, not a broken-route, and many assets
      // legitimately answer non-200 to a bare fetch.
      try {
        const links = await page.evaluate(collectRouteLinks, ASSET_EXT_SOURCE);
        for (const p of links) if (!seenLinks.has(p)) seenLinks.set(p, snap.sig);
      } catch (_) {}
      return snap;
    }

    // ZOOM-REFLOW (WCAG 1.4.10 Reflow, EAA-mandatory): re-render the CURRENT
    // route at 200% zoom by halving the viewport's CSS size (1280x720 -> the
    // reflow-equivalent 640x360), then flag content that breaks: the document
    // now requires TWO-DIMENSIONAL scrolling (fixed-width content grew a
    // horizontal scrollbar by >16px), or a previously visible tappable's hit
    // rect collapsed below 1px while still rendered (a responsively HIDDEN
    // control is intentional adaptation and never fires -- see
    // zoomReflowScan). Once per distinct route (the caller dedupes via
    // zoomChecked) and never in replay or probe mode (guarded at the call
    // sites). Self-restoring: the original viewport is always put back so the
    // walk continues undisturbed.
    async function zoomReflowCheck(sig, route) {
      try {
        const preKeys = await page.evaluate(zoomTappableKeys);
        await page.setViewportSize({ width: Math.round(VW / 2), height: Math.round(VH / 2) });
        await page.waitForTimeout(350);
        const items = await page.evaluate(zoomReflowScan, preKeys).catch(() => null);
        if (items && items.length) {
          log('EXPLORE:ZOOMREFLOW ' + JSON.stringify({ sig, ...(route ? { route } : {}), items }));
        }
      } catch (_) {
      } finally {
        // Restore the pinned viewport (layout-sensitive oracles depend on it).
        try {
          await page.setViewportSize({ width: VW, height: VH });
          await page.waitForTimeout(350);
        } catch (_) {}
      }
    }

    // ROTATION-stability (lifecycle-metamorphic): rotate the viewport by
    // swapping width/height (the orientation change a device rotation /
    // split-screen triggers), let it reflow, then rotate BACK to the original
    // orientation and re-observe. A correct screen reflows but rebuilds the SAME
    // structure once the original orientation is restored; an app that mishandles
    // the resize/orientationchange lifecycle -- dropping content or state that
    // never comes back -- regresses the STRUCTURAL signature (value-state
    // excluded, so a re-fetched timestamp never trips it). Round-trip identity
    // (same orientation in and out) makes it false-positive-free: a legit
    // responsive breakpoint swap is symmetric and restores, so it never fires;
    // only a permanent loss does. Guarded on the pre-transform state having
    // content, so an already-empty screen is not asserted about. Self-restoring
    // (viewport put back); never in replay/probe. Returns the re-observed state.
    async function rotationCheck(snap) {
      const expected = snap.structuralSig;
      // Do not attribute ordinary async settling to rotation. The source must
      // still be structurally identical after a quiet beat before we transform.
      await page.waitForTimeout(300);
      const pre = await snapshot(page, valueNodeSelectors).catch(() => null);
      if (!pre || pre.structuralSig !== expected) return pre || snap;
      try {
        await page.setViewportSize({ width: VH, height: VW });
        await page.waitForTimeout(350);
      } catch (_) {}
      try {
        await page.setViewportSize({ width: VW, height: VH });
        await page.waitForTimeout(350);
      } catch (_) {}
      const after = await observe();
      if (snap.tappables && snap.tappables.length > 0 && after.structuralSig !== expected) {
        // Reconfirm the destination after another quiet beat. A lazy/virtualized
        // view often mounts in phases after resize; only a stable permanent loss
        // is a lifecycle defect.
        await page.waitForTimeout(700);
        const confirmed = await snapshot(page, valueNodeSelectors).catch(() => null);
        if (confirmed && confirmed.structuralSig === after.structuralSig) {
          log('EXPLORE:ROTATION ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), expected, got: after.structuralSig }));
        }
      }
      return after;
    }

    // BACKGROUND-RESTORE-stability (lifecycle-metamorphic): send the page to the
    // background (visibilitychange -> hidden, pagehide, blur) then restore it
    // (visibilitychange -> visible, pageshow, focus) and re-observe. A correct
    // app returns to the SAME screen with its state intact; one that drops you on
    // a different screen or loses state across the lifecycle regresses the
    // STRUCTURAL signature. No size change, so it is a direct before/after
    // comparison (value-state excluded); guarded on the pre-transform state
    // having content. Self-restoring (the page ends visible); never in
    // replay/probe. Returns the re-observed state.
    async function backgroundCheck(snap) {
      const expected = snap.structuralSig;
      await page.waitForTimeout(300);
      const pre = await snapshot(page, valueNodeSelectors).catch(() => null);
      if (!pre || pre.structuralSig !== expected) return pre || snap;
      try {
        await page.evaluate(() => {
          try { Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' }); } catch (_) {}
          try { Object.defineProperty(document, 'hidden', { configurable: true, get: () => true }); } catch (_) {}
          document.dispatchEvent(new Event('visibilitychange'));
          window.dispatchEvent(new Event('pagehide'));
          window.dispatchEvent(new Event('blur'));
        });
        await page.waitForTimeout(300);
        await page.evaluate(() => {
          try { Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'visible' }); } catch (_) {}
          try { Object.defineProperty(document, 'hidden', { configurable: true, get: () => false }); } catch (_) {}
          document.dispatchEvent(new Event('visibilitychange'));
          window.dispatchEvent(new Event('pageshow'));
          window.dispatchEvent(new Event('focus'));
        });
        await page.waitForTimeout(300);
      } catch (_) {}
      const after = await observe();
      if (snap.tappables && snap.tappables.length > 0 && after.structuralSig !== expected) {
        await page.waitForTimeout(700);
        const confirmed = await snapshot(page, valueNodeSelectors).catch(() => null);
        if (confirmed && confirmed.structuralSig === after.structuralSig) {
          log('EXPLORE:BGRESTORE ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), expected, got: after.structuralSig }));
        }
      }
      return after;
    }
    // LISTENER-LEAK (opt-in, REPROIT_LISTENERLEAK=1): drive N revisits of `route`
    // with history back/forward (client-side, NO reload -- the init-script
    // listener tally survives) and watch the live event-listener count (adds -
    // removes) and the attached DOM-node count. A route that mounts
    // listeners/nodes it never releases on unmount climbs MONOTONICALLY across
    // revisits; a stable route is flat after warmup. The first sample is taken
    // AFTER one warmup revisit so a route's one-time persistent listeners are not
    // mistaken for a leak. Fires only when a metric strictly increases on EVERY
    // revisit and rises past the floor. Once per route (the caller dedupes via
    // leakChecked), never in replay/probe mode. Self-restoring: back/forward net
    // to the entry we started on, so the walk continues undisturbed.
    async function listenerLeakCheck(route) {
      const CYCLES = 5;    // revisit samples compared for a monotonic climb
      const MIN_RISE = 5;  // net climb (last - first) a metric must show to count
      const samples = [];
      try {
        for (let i = 0; i < CYCLES; i++) {
          await page.goBack({ timeout: 3000 }).catch(() => {});
          await page.waitForTimeout(250);
          await page.goForward({ timeout: 3000 }).catch(() => {});
          await page.waitForTimeout(250);
          // Confirm the forward step landed back on the SAME route; if history
          // drifted (a redirect, an off-route back), abort so we never compare
          // samples from different screens.
          const snap = await snapshot(page, valueNodeSelectors).catch(() => null);
          if (!snap || snap.anchor !== route) return;
          const s = await page.evaluate(listenerLeakSample).catch(() => null);
          if (!s) return;
          samples.push(s);
        }
      } catch (_) { return; }
      if (samples.length < 3) return;
      const items = [];
      const consider = (kind, series) => {
        for (let i = 1; i < series.length; i++) if (!(series[i] > series[i - 1])) return;
        const rise = series[series.length - 1] - series[0];
        if (rise >= MIN_RISE) items.push({ kind, first: series[0], last: series[series.length - 1] });
      };
      // Drop the first sample as warmup (the route's initial persistent mount),
      // then require a strict monotonic climb across the remaining revisits.
      const post = samples.slice(1);
      consider('listeners', post.map((s) => s.live));
      consider('nodes', post.map((s) => s.nodes));
      if (items.length) {
        log('EXPLORE:LISTENERLEAK ' + JSON.stringify({ route, visits: post.length, items }));
      }
    }

    let current = await observe();
    launchSig = current.sig;
    // ZOOM-REFLOW for the start route: the walk's tap-edge check only covers
    // routes NAVIGATED to, so the launch screen gets its zoomed re-render here.
    if (!replay && !PROBE && current.anchor && !zoomChecked.has(current.anchor)) {
      zoomChecked.add(current.anchor);
      await zoomReflowCheck(current.sig, current.anchor);
    }
    let stuck = 0;
    const prefix = fuzz.prefix || null;
    const prefixLen = prefix ? prefix.length : 0;
    const mapMode = !replay && !prefix && !fuzz.seed;
    const budget = replay
      ? replay.length
      : (((mapMode && !FUZZ_CONFIGURED) ? MAP_ACTION_BUDGET : (fuzz.budget || ACTION_BUDGET)) + prefixLen);

    // LEAK sampler: in REPLAY mode (the `--soak` tier writes {"replay":[...]}),
    // sample the web heap once at the start and after every action, so the Rust
    // soak oracle gets a heap-vs-time series to read the slope from. Off outside
    // replay (a plain fuzz walk is not a soak). t0 anchors t_ms to walk start.
    const t0 = Date.now();
    if (replay) await sampleHeap(page, gtCdp, 0);

    let actions = 0;
    for (; actions < budget && stuck < 3; actions++) {
    // LEAK sampler: in replay mode, sample the heap once per action (this fires
    // BEFORE acting, so action k's sample reflects the heap after the previous
    // action settled; together with the start + final samples it forms the
    // monotonic series the soak slope is read from). No-op outside replay.
    if (replay && actions > 0) await sampleHeap(page, gtCdp, Date.now() - t0);
    // LIFECYCLE-metamorphic oracles (rotation, background-restore): once per
    // distinct state, apply a device-lifecycle transform and assert the
    // structural signature survives it. Self-restoring, so `current` is refreshed
    // to the (restored) reality afterwards; never in replay/probe (a recorded
    // clip must not jump viewport or fire lifecycle events). Runs before action
    // selection so the walk continues from the re-observed state.
    if (!replay && !PROBE) {
      if (!rotChecked.has(current.sig)) { rotChecked.add(current.sig); current = await rotationCheck(current); }
      if (!bgChecked.has(current.sig)) { bgChecked.add(current.sig); current = await backgroundCheck(current); }
    }
    // COMPONENT-CHOICE differential (fuzz only, not replay): when the current
    // state exposes a multi-choice component not yet exercised this seed,
    // exhaustively select each choice and flag a global-layout outlier. Each
    // group is its own bounded sub-traversal, consuming one action slot.
    if (!replay) {
      let exercised = false;
      // ARIA / button-cluster groups (from the snapshot tappables) plus native
      // <select> components (FEATURE 1; queried live since the snapshot maps a
      // <select> to a text field and so never surfaces its options).
      const groups = detectChoiceGroups(current.tappables)
        .concat(await detectSelectGroups(page));
      for (const group of groups) {
        const gkey =
          current.sig + '|' + group.role + '|' + group.opts.map((o) => o.sel).join(',');
        if (exercisedGroups.has(gkey)) continue;
        exercisedGroups.add(gkey);
        await exerciseChoiceGroup(page, group, current.sig);
        current = await observe();
        exercised = true;
        break;
      }
      if (exercised) continue;
    }
    let act;
    if (replay) act = replay[actions];
    else if (prefix && actions < prefixLen) act = prefix[actions];
    else if (fuzz.seed) {
      // Inverse-visit-count weighted pick: weight each candidate edge by
      // 1/(1+globalVisits) from the edgeWeights snapshot, plus 'back'.
      // Seeded + deterministic, so replays reproduce exactly. Candidates are
      // addressed by STRUCTURAL selector (key, else role+index), never by
      // visible text, so the seeded pick and any replay are locale-invariant.
      // Candidate edges: tap every tappable; for text fields ALSO offer a type
      // edge whose adversarial value is chosen deterministically from the seed
      // (the option string carries the value id so a replay reconstructs it).
      // Exclude cross-origin links from the action set: tapping one leaves the
      // app (see isExternalLink). They stay in `tappables` so role:<role>#<idx>
      // indices are unchanged; they are just never chosen as an edge.
      const actable = current.tappables.filter((e) => !e.external);
      const taps = actable.map((e) => e.sel).sort();
      const textSels = actable.filter((e) => e.role === 'textfield').map((e) => e.sel).sort();
      const typeOpts = textSels.map((s) => {
        // Derive the adversarial id from seed + selector so the same field on
        // the same seed always types the same value (reproducible), but
        // different fields can get different values.
        const idx = pick(ADVERSARIAL.length === 0 ? 1 : ADVERSARIAL.length);
        return 'type:' + s + '=' + adversarialFor(idx).id;
      });
      const ew = (fuzz.edgeWeights && fuzz.edgeWeights[current.sig]) || {};
      const options = taps.map((s) => 'tap:' + s).concat(typeOpts).concat(['back']);
      const weights = options.map((o) => 1 / (1 + (ew[o] || 0)));
      const total = weights.reduce((a, b) => a + b, 0);
      let r = (pick(1 << 20) / (1 << 20)) * total;
      act = options[options.length - 1];
      for (let k = 0; k < options.length; k++) { r -= weights[k]; if (r <= 0) { act = options[k]; break; } }
    } else {
      const actions = [];
      for (const el of current.tappables) {
        if (el.external) continue; // never leave the app-under-test's origin
        actions.push(el.role === 'textfield' ? 'type:' + el.sel + '=normal' : 'tap:' + el.sel);
      }
      actions.sort();
      actions.push('back');
      rememberActions(actionsByState, current.sig, actions);
      act = firstUntriedAction(actionsByState, triedEdges, current.sig);
      if (!act) {
        const path = pathToFrontier(graph, actionsByState, triedEdges, current.sig);
        act = path && path.length ? path[0] : null;
      }
      if (!act && hasFrontier(actionsByState, triedEdges) && current.sig !== launchSig) break;
      if (!act) break;
    }

    log('FUZZ:ACT ' + act);
    // Record/review HUD: when recording a REPLAY (`check --record`), draw a
    // paced on-screen caption of each action so a human can actually follow the
    // repro - the video analogue of the cloud "path to the bug". Only when
    // replaying AND recording, so a normal fuzz hunt is never slowed.
    if (replay && VIDEO_DIR && !act.startsWith('assert:') && !act.startsWith('shoot:')) {
      const isLast = actions >= replay.length - 1;
      const o = String(fuzz.highlight || '');
      // The final action of a sequence-bug clip is the one that breaks the app.
      const trigger = isLast && /hang|jank|exception|crash/.test(o);
      if (act.startsWith('tap:')) {
        // Highlight the element reproit is ABOUT to tap, with its human-readable
        // name (not `role:link#7`), drawn while the page is still live. For the
        // final trigger of a sequence-bug clip (hang/crash/jank) it is the bug
        // itself, so box it RED with the outcome; other taps are BLUE "here's what
        // I clicked". Drawing pre-tap (a PREVIEW box, no click) means a tap that
        // navigates/freezes still shows the right element (a frozen page can't be
        // annotated afterward), and lets the clip LINGER on the doomed control
        // before it is actually tapped.
        const sel = act.slice('tap:'.length);
        const target = current.tappables.find((e) => e.sel === sel);
        let name = (target && target.label && String(target.label).trim()) || sel;
        if (name.length > 36) name = name.slice(0, 35) + '…';
        const outcome = /hang/.test(o) ? '  → froze' : /jank/.test(o) ? '  → janked' : /exception|crash/.test(o) ? '  → crashed' : /dead/.test(o) ? '  → no effect' : '';
        // Highlight the element in RED before acting on it -- every clicked control
        // in a clip is boxed red the beat BEFORE the click, so the viewer always
        // sees what is about to be actuated (the trigger also carries its outcome).
        await tap(page, sel, {
          box: 'about to tap  ' + name + (trigger ? outcome : ''),
          boxColor: '#e21f1f',
        }).catch(() => {});
      } else {
        await showActionHud(page, act, actions, replay.length).catch(() => {});
      }
      // Hold before performing the action. Linger LONGEST on the control that is
      // about to break (the crash/jank/hang trigger) so the recorded clip clearly
      // shows the doomed element for a beat -- highlighted, pausable -- and THEN
      // breaks. Other final steps get a shorter beat; mid-sequence steps are quick.
      await page.waitForTimeout(trigger ? 2600 : isLast ? 1600 : 950);
    }
    if (act.startsWith('shoot:')) {
      // Screenshot point (e.g. a `do: shoot:<name>` journey/tour step): capture
      // the current screen to REPROIT_SHOTS_DIR and emit the SHOOT marker. Like
      // an assertion, it does not move the known state (no observe/stuck change).
      await shoot(page, act.slice('shoot:'.length));
      continue;
    }
    if (act.startsWith('auth:')) {
      // Session bypass: restore a pre-authenticated session for the account so a
      // journey can exercise a feature without re-driving the login UI each run.
      // The orchestrator injects REPROIT_SECRET_<ACCT>_STORAGE (a JSON map of
      // localStorage entries) from the vault; we seed it and reload so the app
      // boots authenticated. Absent/garbage => FUZZ:MISS (the journey is stale,
      // not a pass: it never reached the authenticated state it assumed).
      const acct = act.slice('auth:'.length);
      const envName = 'REPROIT_SECRET_' + acct.replace(/[^A-Za-z0-9]/g, '_').toUpperCase() + '_STORAGE';
      const raw = process.env[envName];
      if (!raw) { log('FUZZ:MISS ' + act + ' (no ' + envName + ')'); stuck++; continue; }
      let store;
      try { store = JSON.parse(raw); } catch { log('FUZZ:MISS ' + act + ' (bad JSON in ' + envName + ')'); stuck++; continue; }
      await page.addInitScript((entries) => {
        try { for (const [k, v] of Object.entries(entries)) localStorage.setItem(k, v); } catch (_) {}
      }, store);
      await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
      await page.waitForTimeout(replay ? 700 : 400);
      current = await observe(); // observe() emits FUZZ:STATE in replay mode
      continue;
    }
    if (act.startsWith('assert:')) {
      // Journey assertions: evaluated against the live screen at this point in
      // the replay. They never move state (no observe/stuck change); the verdict
      // is reported via FUZZ:ASSERT and the CLI maps a fail to a stale run.
      const body = act.slice('assert:'.length);
      if (body.startsWith('state=')) {
        const want = body.slice('state='.length);
        const got = current.sig; // current is the state after the previous action
        log('FUZZ:ASSERT ' + (got === want ? 'pass' : 'fail') + ' state want=' + want + ' got=' + got);
      } else if (body.startsWith('text=')) {
        const want = body.slice('text='.length);
        const ok = await page.evaluate((t) => !!(document.body && document.body.innerText.includes(t)), want).catch(() => false);
        log('FUZZ:ASSERT ' + (ok ? 'pass' : 'fail') + ' text=' + JSON.stringify(want));
      } else if (body.startsWith('count:')) {
        const rest = body.slice('count:'.length);
        const eq = rest.lastIndexOf('=');
        const finder = eq >= 0 ? rest.slice(0, eq) : rest;
        const want = eq >= 0 ? parseInt(rest.slice(eq + 1), 10) : 0;
        const got = await page.evaluate(countMatching, finder).catch(() => -1);
        log('FUZZ:ASSERT ' + (got === want ? 'pass' : 'fail') + ' count ' + finder + ' want=' + want + ' got=' + got);
      } else {
        log('FUZZ:ASSERT fail unknown-assertion ' + body);
      }
      continue;
    }
    if (act === 'back') {
      const before = current.sig;
      triedEdges.add(edgeKey(before, 'back'));
      const beforeContent = current.content;
      const origin = new URL(APP_URL).origin;
      await page.goBack({ timeout: 3000 }).catch(() => {});
      await page.waitForTimeout(600);
      // Stepping off the app (about:blank) is not a real state: go forward.
      if (!page.url().startsWith(origin)) {
        await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
        await page.waitForTimeout(400);
        stuck++;
        current = await observe();
        continue;
      }
      const next = await observe();
      if (next.sig !== before) {
        log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'back', to: next.sig }));
        rememberEdge(graph, before, 'back', next.sig);
        stuck = 0;
      } else if (next.content !== beforeContent) {
        // Layer-1: the action changed on-screen content without moving the
        // structural sig (a value-state change on a capped node). It is
        // EFFECTIVE, so do not count it as stuck, but no graph edge is added.
        stuck = 0;
      } else stuck++;
      current = next;
      continue;
    }
    if (act.startsWith('type:')) {
      // type:<sel>=<valueId> -> focus the field and type the value.
      const body = act.slice('type:'.length);
      const eq = body.lastIndexOf('=');
      const sel = eq >= 0 ? body.slice(0, eq) : body;
      const valId = eq >= 0 ? body.slice(eq + 1) : 'normal';
      // PRECEDENCE: an explicit property-matched fixture input for this field
      // wins over the adversarial-class token. The class token still picks the
      // value when no input matches (the existing path, unchanged). Both are
      // deterministic, so the replay reproduces the same text either way.
      const fixtureVal = inputValueFor(sel, inputs);
      const value = fixtureVal != null
        ? fixtureVal
        : (ADVERSARIAL_BY_ID[valId] !== undefined ? ADVERSARIAL_BY_ID[valId] : expandEnv(valId));
      triedEdges.add(edgeKey(current.sig, act));
      const before = current.sig;
      const beforeContent = current.content;
      await page.evaluate(() => { window.__reproitLongTasks = []; window.__reproitFrameIntervals = []; }).catch(() => {}); // jank/hang: drop pre-action longtasks + frame intervals
      const perfBeforeType = await readLayoutCounters(gtCdp); // jank: machine-invariant forced-layout baseline
      const typePix = await startScreencastCapture(gtCdp); // Tier-2 (gated): record presented frames
      const ok = await typeInto(page, sel, value, { mark: recording });
      if (!ok) { if (typePix) await typePix.stop(); log('FUZZ:MISS ' + act); stuck++; continue; }
      const perfAfterType = await readLayoutCounters(gtCdp); // jank: read before settle -> synchronous reflow only
      // Replays settle longer than the fuzz walk: under recording/CI load the
      // app's handler (and any uncaught throw it triggers) needs more wall-clock
      // to run and for `pageerror` to fire, so a deterministic crash isn't
      // missed. The fuzz walk stays fast.
      await page.waitForTimeout(replay ? 1100 : 700);
      // Typing + Enter can navigate (e.g. a search form submitting to another
      // origin). Stay on the app-under-test: drop off-origin destinations.
      if (await recoverIfOffOrigin()) { if (typePix) await typePix.stop(); stuck++; current = await observe(); continue; }
      await finishScreencastCapture(typePix, before, 'type:' + sel + '=' + valId);
      const typeJank = await drainJankForEngine(page);
      const typeThrash = layoutThrash(perfBeforeType, perfAfterType);
      if (typeThrash && (!typeJank || typeJank.kind !== 'hang')) {
        log('EXPLORE:JANK ' + JSON.stringify({ from: before, action: 'type:' + sel + '=' + valId, bucket: typeThrash.count, unit: 'layouts', count: typeThrash.count }));
      } else if (typeJank) {
        log('EXPLORE:' + (typeJank.kind === 'hang' ? 'HANG' : 'JANK') + ' ' +
          JSON.stringify({ from: before, action: 'type:' + sel + '=' + valId, bucket: typeJank.bucket, count: typeJank.count }));
      }
      if (recording) {
        lastTriggerLabel = (typeJank || typeThrash) ? ((typeJank && typeJank.kind === 'hang') ? 'froze' : 'jank') : null;
        lastFlickerKeys = (typeChurn && typeChurn.length) ? typeChurn : null;
      }
      const next = await observe();
      if (next.sig !== before) {
        log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'type:' + sel + '=' + valId, to: next.sig }));
        rememberEdge(graph, before, 'type:' + sel + '=' + valId, next.sig);
        stuck = 0;
      } else if (next.content !== beforeContent) {
        stuck = 0; // Layer-1: content changed without a structural move; effective.
      } else stuck++;
      current = next;
      continue;
    }
    const sel = act.slice('tap:'.length);
    // Key MUST match the picker's edge form (`tap:<sel>`, line ~3337); recording
    // the bare `<sel>` left every tap looking perpetually untried, so the
    // deterministic walk kept re-tapping the first control and under-explored.
    triedEdges.add(edgeKey(current.sig, 'tap:' + sel));
    const before = current.sig;
    const beforeContent = current.content;
    const beforeAnchor = current.anchor;
    // Remember the source page + link before this (possibly navigating) tap, so a
    // broken-route landed on next is attributed to exactly here, not reverse-matched.
    lastNav = { from: before, action: 'tap:' + sel };
    await page.evaluate(() => { window.__reproitLongTasks = []; window.__reproitFrameIntervals = []; }).catch(() => {}); // jank/hang: drop pre-action longtasks + frame intervals
    // FOCUS-LOSS: record the pre-tap activeElement + open dialog count and arm
    // the probe (tap()'s doClick then focuses the control before clicking, the
    // way a real user click does). Checked after the settle below.
    await page.evaluate(focusLossArm).catch(() => {});
    // DUPLICATE-SUBMIT probe (opt-in, REPROIT_DUPSUBMIT=1): when this tap
    // targets a button, dispatch a SECOND click ~120ms after the first and
    // record every first-party non-GET request over the window, so a submit
    // handler with no double-activation guard is caught firing the same
    // (method, url) twice. Armed BEFORE the first click so its request counts;
    // the in-page eligibility check between the clicks confirms the control is
    // actually submit-like. Once per (from, action); never on a recorded clip.
    const dupTapTarget = DUPSUBMIT ? current.tappables.find((e) => e.sel === sel) : null;
    const dupProbe = DUPSUBMIT && !recording
      && !!dupTapTarget && dupTapTarget.role === 'button'
      && !dupProbed.has(edgeKey(before, 'tap:' + sel));
    let dupUrlBefore = null;
    if (dupProbe) {
      dupProbed.add(edgeKey(before, 'tap:' + sel));
      dupUrlBefore = page.url();
      dupReqLog = [];
    }
    const perfBefore = await readLayoutCounters(gtCdp); // jank: machine-invariant forced-layout baseline
    const tapPix = await startScreencastCapture(gtCdp); // Tier-2 (gated): record presented frames
    const ok = await tap(page, sel, { mark: recording });
    if (!ok) { if (tapPix) await tapPix.stop(); dupReqLog = null; log('FUZZ:MISS ' + act); stuck++; continue; }
    // JANK: read the forced-layout counter NOW, right after the synchronous
    // handler returned and BEFORE the settle wait, so the delta counts only the
    // handler's own reflows -- not animation frames over the settle (which would
    // be machine-dependent and reintroduce flake).
    const perfAfterTap = await readLayoutCounters(gtCdp);
    // DUPLICATE-SUBMIT double dispatch: the second click, ~120ms after the
    // first -- the probe's rapid double activation IN PLACE OF the walk's usual
    // single click. Skipped when the first click already changed the URL (the
    // navigation legitimately swallows a second click: no probe, no finding) or
    // when the resolved element is not submit-like in-page (a submit-type
    // control inside a form qualifies even without a matching accessible name).
    let dupDispatched = false;
    if (dupProbe && dupReqLog) {
      await page.waitForTimeout(120);
      const eligible = await page.evaluate(dupSubmitEligible).catch(() => false);
      if (eligible && page.url() === dupUrlBefore) {
        dupDispatched = await tap(page, sel).catch(() => false);
        // RECORD the second dispatch into the action sequence (FUZZ:ACT) only when
        // it actually fired: the walk continues from the post-double-click state, so
        // a kept repro must replay both clicks or it diverges (the probe otherwise
        // mutated state invisibly).
        if (dupDispatched) log('FUZZ:ACT tap:' + sel);
      }
      if (!dupDispatched) dupReqLog = null;
    }
    // Replays settle longer than the fuzz walk (see the type branch): a
    // deterministic crash must have time to throw + flush `pageerror` under load.
    await page.waitForTimeout(replay ? 1100 : 700);
    // DUPLICATE-SUBMIT verdict: group the captured window's first-party non-GET
    // requests by (method, url); the same pair firing twice or more while the
    // URL never changed is the bug (the handler has no double-activation
    // guard). Reported once per (from, action); the map layer dedupes again.
    if (dupProbe && dupReqLog) {
      const captured = dupReqLog;
      dupReqLog = null;
      if (dupDispatched && page.url() === dupUrlBefore) {
        const counts = new Map();
        for (const r of captured) counts.set(r, (counts.get(r) || 0) + 1);
        for (const [key, n] of counts) {
          if (n < 2) continue;
          const sp = key.indexOf(' ');
          log('EXPLORE:DUPSUBMIT ' + JSON.stringify({
            from: before, action: 'tap:' + sel,
            method: key.slice(0, sp), url: key.slice(sp + 1), count: n,
          }));
          break;
        }
      }
    }
    // SEQUENCE-BUG clip (hang/crash/jank), FINAL action: this tap IS the trigger
    // and the page may now be frozen/busy. The churn + observe below each do a
    // page.evaluate, which BLOCKS on a busy main thread for ~30s -- that is what
    // made a hang clip ~80s long. So for a clip we skip them and detect the bug by
    // RESPONSIVENESS (a hang's own definition: the page stops responding), which
    // is fast AND faithful (it really re-fired), not a timeout that gives up.
    if (recording && replay && actions >= replay.length - 1
        && /hang|jank|exception|crash/.test(String(fuzz.highlight || ''))) {
      if (tapPix) await tapPix.stop();
      if (/hang|jank/.test(String(fuzz.highlight))) {
        const responsive = await Promise.race([
          page.evaluate(() => true).then(() => true, () => true),
          new Promise((r) => setTimeout(() => r(false), 2500)),
        ]);
        if (!responsive) lastTriggerLabel = 'froze'; // unresponsive = the hang re-fired
      }
      // crash: the pageerror handler already bumped replayErrorCount.
      break; // end the replay; the end-of-replay block emits FINDING:BOXED + holds
    }
    // ORIGIN GUARD: a tap on an outbound link (footer "View on GitHub", a
    // social link) navigates off the app-under-test's origin. That page is NOT
    // a state of the app; recording it would make the whole map about the
    // foreign site. Recover (go back / re-goto) and do NOT record the state.
    if (await recoverIfOffOrigin()) { if (tapPix) await tapPix.stop(); stuck++; current = await observe(); continue; }
    await finishScreencastCapture(tapPix, before, 'tap:' + sel);
    // JANK/HANG watchdog: did this action block the main thread past the
    // jank/hang floor? Keyed by (from, action) like the flicker oracle, so the
    // Rust side attributes it to this transition and `check` re-confirms it.
    const tapJank = await drainJankForEngine(page);
    // Deterministic layout-thrash jank (machine-invariant forced-layout count).
    // Preferred over the wall-clock jank bucket when it fires: the count
    // reproduces on any runner, so `check` re-confirms it without depending on
    // machine speed. A HANG (freeze) still reports from the timing watchdog (a 2s
    // freeze is robust and may be pure-compute with no layouts).
    const tapThrash = layoutThrash(perfBefore, perfAfterTap);
    if (tapThrash && (!tapJank || tapJank.kind !== 'hang')) {
      log('EXPLORE:JANK ' + JSON.stringify({ from: before, action: 'tap:' + sel, bucket: tapThrash.count, unit: 'layouts', count: tapThrash.count }));
    } else if (tapJank) {
      log('EXPLORE:' + (tapJank.kind === 'hang' ? 'HANG' : 'JANK') + ' ' +
        JSON.stringify({ from: before, action: 'tap:' + sel, bucket: tapJank.bucket, count: tapJank.count }));
    }
    if (recording) {
      lastTriggerLabel = (tapJank || tapThrash) ? ((tapJank && tapJank.kind === 'hang') ? 'froze' : 'jank') : null;
      lastFlickerKeys = (tapChurn && tapChurn.length) ? tapChurn : null;
    }
    // FOCUS-LOSS: read the in-page verdict BEFORE observe() -- a new state's
    // ground-truth probe mutates the DOM and can move focus, which would
    // corrupt the reading. Whether the tap actually navigated is only known
    // after observe(), so the emit decision is just below.
    const focusLost = await page.evaluate(focusLossCheck).catch(() => false);
    const next = await observe();
    // FOCUS-LOSS: only a NON-navigating tap counts (same structural sig, or
    // the same route after settle: an in-place re-render). A navigation is
    // expected to move focus, so it never fires; the in-page check already
    // applied the dialog / removed-control / link guards.
    if (focusLost && (next.sig === before || (next.anchor && next.anchor === beforeAnchor))) {
      log('EXPLORE:FOCUSLOSS ' + JSON.stringify({ from: before, action: 'tap:' + sel }));
    }
    if (next.sig !== before) {
      log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'tap:' + sel, to: next.sig }));
      rememberEdge(graph, before, 'tap:' + sel, next.sig);
      stuck = 0;
      // ZOOM-REFLOW: this tap navigated to a route not yet zoom-tested; run the
      // 200% zoom re-render BEFORE the metamorphic reload below (the check
      // restores the viewport, so the reload still sees the pinned size). Never
      // in replay (a recorded clip must not jump viewports) or probe mode.
      if (!replay && !PROBE && next.anchor && !zoomChecked.has(next.anchor)) {
        zoomChecked.add(next.anchor);
        await zoomReflowCheck(next.sig, next.anchor);
      }
      // LISTENER-LEAK (opt-in): this tap navigated to a new route with a real
      // history entry (the anchor CHANGED). Probe it for a revisit leak via the
      // back/forward loop. Once per route; guarded off in replay/probe mode like
      // the other route checks.
      if (LISTENERLEAK && !replay && !PROBE && next.anchor && next.anchor !== beforeAnchor && !leakChecked.has(next.anchor)) {
        leakChecked.add(next.anchor);
        await listenerLeakCheck(next.anchor);
      }
    } else if (next.content !== beforeContent) {
      // Layer-1 effect detection: the tap changed displayed content (a calculator
      // keypress on a capped display) without a structural move. EFFECTIVE, so
      // reset stuck and keep driving; no self-edge is recorded.
      stuck = 0;
    }
    current = next;
  }

    if (mapMode && actions >= budget && hasFrontier(actionsByState, triedEdges)) {
      log('EXPLORE:TRUNCATED ' + JSON.stringify({
        reason: 'action-budget', budget: MAP_ACTION_BUDGET,
        states: actionsByState.size,
      }));
    }

    // LEAK sampler: a final heap sample after the last action, so the series
    // spans the whole soak (start ... last action). No-op outside replay.
    if (replay) await sampleHeap(page, gtCdp, Date.now() - t0);
    // FINDING HIGHLIGHT: on a recorded replay, draw a red box around what broke
    // on this final state and hold it so the clip ends on the bug itself - the
    // visual companion to the action HUD. State oracles (overflow/content) are
    // re-detected inside; crash/jank/hang/flicker come from the latest action's
    // captured signals (crash overrides a jank label on the same action). Replay+
    // record only, so a normal fuzz hunt is untouched.
    if (recording) {
      if (fuzz.highlight && fuzz.highlight.includes('choice')) {
        // CHOICE-ANOMALY clip: a CALM, minimal reproduction. The scan already named
        // the outlier (fuzz.choiceOutlier) and confirmed the anomaly, so the clip
        // does NOT re-run the differential (clicking every option + an A/B re-toggle
        // on camera made an unwatchable, jumpy clip). It just: find the outlier
        // option, bring it into view ONLY if it is off-screen (a slow scroll to the
        // one control the action touches -- never a full-page scroll-through), select
        // it once so the page visibly shifts, and box it. If the host did not pass an
        // outlier (older map), fall back to one in-page detection pass to name it.
        let drew = false;
        try {
          let label = fuzz.choiceOutlier || null;
          let mag = Number(fuzz.choiceMag) || 0;
          if (!label) {
            const found = await page.evaluate(choiceAnomalyInPage, {
              settleMs: 600, ratio: CHOICE_OUTLIER_RATIO, minMag: CHOICE_MIN_MAGNITUDE,
              choiceRoles: CHOICE_ROLE_LIST,
            }).catch(() => []);
            const top = (found || []).sort((a, b) => (b.magnitude || 0) - (a.magnitude || 0))[0];
            if (top && top.outlier) { label = top.outlier; mag = top.magnitude || 0; }
          }
          if (label) {
            const replayed = await page.evaluate(replayChoiceComponentInPage, {
              label, settleMs: 450,
            }).catch(() => ({ ok: false, choices: [] }));
            if (replayed && replayed.ok) {
              await page.waitForTimeout(800); // let the page settle into the shifted layout
              await drawFindingBoxes(page, {
                triggerLabel: mag ? 'layout shift +' + Math.round(mag) + 'px' : 'layout shift',
                oracle: 'no-choice-anomaly',
              }).catch(() => {});
              await page.waitForTimeout(2000); // hold the boxed shift for a beat
              drew = true;
            }
          }
        } catch (_) { /* ignore */ }
        log('FINDING:BOXED ' + JSON.stringify({ oracle: fuzz.highlight, drew }));
      } else if (/hang|jank|exception|crash/.test(String(fuzz.highlight || ''))) {
        // SEQUENCE-BUG clip (hang/crash/jank): the trigger was already boxed RED
        // PRE-tap (while the page was live), so we do NOT draw on the now-frozen/
        // broken page -- that is what made the clip wait ~80s for the freeze to
        // release. The trust gate is whether the bug ACTUALLY RE-FIRED on replay
        // (a real re-hang / re-crash), not whether a box drew. Faithful or dropped.
        const fired = lastTriggerLabel === 'froze' || lastTriggerLabel === 'jank'
          || replayErrorCount > crashAtStart;
        log('FINDING:BOXED ' + JSON.stringify({ oracle: fuzz.highlight, drew: !!fired }));
      } else if (fuzz.brokenRouteStatus) {
        // A broken document reached during the original walk has no source
        // anchor to box. Revalidate the actual navigation response and apply
        // the same SPA soft-404 guard used during discovery. This makes the
        // trust marker deterministic without substituting an unrelated link.
        const expected = Number(fuzz.brokenRouteStatus);
        const actual = startResponse ? startResponse.status() : 0;
        const view = await page.evaluate(soft404View)
          .catch(() => ({ controls: 0, mountFilled: false, notFound: false }));
        const fired = actual === expected && isDeadRouteStatus(actual) && !isSoftHandled(view);
        log('FINDING:BOXED ' + JSON.stringify({ oracle: fuzz.highlight, drew: fired }));
      } else {
        // STATE-PRESENT (overflow/content) + broken-route: re-detect on the live
        // page and box it (the page is not frozen here).
        await drawFindingBoxes(page, {
          triggerLabel: lastTriggerLabel,
          flickerKeys: lastFlickerKeys,
          oracle: fuzz.highlight || null,
          linkHref: fuzz.linkHref || null,
        }).catch(() => {});
      }
      await page.waitForTimeout(2200);
    }
    // BROKEN-ROUTE link check: catch a dead link the bounded crawl never tapped
    // (a footer /download 404). Skip in a replay (a clip re-walk). TWO stages,
    // because a raw fetch does NOT match a real browser navigation: an SPA serves a
    // client route on navigation but 404s a bare fetch, so fetch alone false-flags
    // working links (e.g. a jobs board's /jobs/role/* client routes).
    //   1) a GET filter over every un-visited same-origin link (8-way). GET, NOT
    //      HEAD: a CDN/server answers HEAD with 405/501 ("method not implemented")
    //      while GET is 200, so a HEAD probe manufactured a false dead route (the
    //      AdminLTE /index2.html 501 FP). GET is what the user's navigation issues.
    //   2) VERIFY each flagged candidate with a real page.goto (also a GET) -- only
    //      a link that truly returns 404/410 ON NAVIGATION is reported.
    if (!replay) {
      const FETCH_CAP = 400, VERIFY_CAP = 20;
      const toProbe = [...seenLinks.entries()].filter(([p]) => navStatus[p] === undefined);
      const batch = toProbe.slice(0, FETCH_CAP);
      let statuses = {};
      if (batch.length) {
        try {
          statuses = await page.evaluate(async (paths) => {
            const origin = location.origin, out = {};
            let i = 0;
            const worker = async () => {
              while (i < paths.length) {
                const p = paths[i++];
                try { const r = await fetch(origin + p, { method: 'GET', redirect: 'manual' }); out[p] = r.status; }
                catch (e) { out[p] = 0; }
              }
            };
            await Promise.all(Array.from({ length: 8 }, worker));
            return out;
          }, batch.map(([p]) => p));
        } catch (_) {}
      }
      // DEAD only when the resource is GENUINELY GONE: 404 or 410 (isDeadRouteStatus).
      // Never 405/501 (method), 3xx (redirect), or 5xx (transient) -- not broken links.
      const isDead = (s) => isDeadRouteStatus(s);
      const candidates = batch.filter(([p]) => isDead(statuses[p] || 0));
      let verified = 0;
      for (const [path, fromSig] of candidates) {
        navStatus[path] = statuses[path] || 0; // remember the fetch verdict
        if (verified >= VERIFY_CAP) continue;
        verified++;
        let navStat = 0;
        try {
          const r = await page.goto(APP_ORIGIN + path, { waitUntil: 'load', timeout: 7000 });
          navStat = r ? r.status() : 0;
        } catch (_) {}
        navStatus[path] = navStat;
        if (!isDead(navStat)) continue;
        // SPA SOFT-404 guard: a static host (GitHub Pages / Netlify / Vercel) can
        // answer a deep path with HTTP 404 yet still serve index.html, and the
        // client router then hydrates the CORRECT app view. Only a GENUINE error /
        // empty page is a broken route, so after the 404 nav we let the SPA settle
        // and inspect the rendered document: a populated app mount with real
        // interactive content and no dominant not-found message means the router
        // handled it (naiveui, form.io) -> NOT dead. A bare error page (few
        // controls, no app mount, or a prominent "page not found") stays dead.
        // A LIGHT wait (not the full settle): the page already fired 'load', and a
        // client router hydrates its shell within a short window -- the full settle
        // here cost seconds per candidate on a never-idle site (the Ace editor / a
        // large 404 set), so it is bounded to a short fixed wait.
        try { await page.waitForTimeout(500); } catch (_) {}
        const view = await page.evaluate(soft404View)
          .catch(() => ({ controls: 0, mountFilled: false, notFound: false }));
        if (isSoftHandled(view)) {
          navStatus[path] = 200; // the app served a real view; not a broken route.
          continue;
        }
        log('EXPLORE:BROKENROUTE ' + JSON.stringify({ sig: fromSig, route: path, status: navStat, from: fromSig }));
      }
      const unverified = candidates.length - Math.min(candidates.length, VERIFY_CAP);
      if (unverified) log(`JOURNEY[a] step: broken-route: ${unverified} candidate link(s) not verified (capped)`);
    }
    log(`JOURNEY[a] step: explored ${seenStates.size} states`);
  }

  // Run every seed in this session in sequence. For a multi-seed batch
  // ({"batch":[...]}) wrap EACH seed's walk in SEED:BEGIN <seed> ... SEED:END
  // <seed> so the Rust side (fuzz.rs split_seed_segments) attributes coverage,
  // trace, and findings to the right seed; between seeds re-pump a fresh start
  // screen so each seed begins clean. A single-seed {"seed":..} run emits NO
  // SEED markers.
  const { seeds, isBatch } = loadBatch();
  for (let i = 0; i < seeds.length; i++) {
    const fuzz = seeds[i];
    if (isBatch) {
      if (i > 0) await resetToRoot();
      log(`SEED:BEGIN ${Number(fuzz.seed || 0)}`);
    }
    await runSeed(fuzz);
    if (isBatch) log(`SEED:END ${Number(fuzz.seed || 0)}`);
  }

  // Flush: a `pageerror` from the final action is delivered asynchronously, so
  // give it a beat to reach `emitError` (and the EXCEPTION block) before we tear
  // the page down. Without this, a crash on the very last replay step can race
  // the close and be lost under load.
  await page.waitForTimeout(500);
  log('JOURNEY DONE');
  log('All tests passed');
  await context.close();
  await browser.close();
}

// Standard ESM main guard: drive the browser only when executed directly
// (`node runner.mjs`, how the orchestrator launches it), NOT when this module is
// imported. Keeps snapshot()/signatureOf importable from tests without launching
// a browser on import.
if (import.meta.url === pathToFileURL(process.argv[1] || '').href) {
  main().catch((e) => {
    log('EXCEPTION CAUGHT BY WEB RUNNER');
    log(String(e && e.stack ? e.stack : e));
    log('Some tests failed');
    process.exit(0); // evidence already emitted; orchestrator judges by markers
  });
}
