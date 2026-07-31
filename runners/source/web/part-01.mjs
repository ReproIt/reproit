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
import { createHash } from 'node:crypto';
// The canonical signature model is SHARED, not copied. This runner carried a
// duplicate of shared/signature.mjs that differed only in an import alias and
// two blank lines, and the parity gate did not cover web until this change, so
// a drift between the copies could not have been caught here. esbuild inlines
// this for the bundled web target, so the shipped runner stays self contained.
import {
  signatureOf,
  descriptorOf,
  valueClass,
  fnv1a,
  loadValueNodes,
} from '../shared/signature.mjs';
// The in-page DOM predicates and the selector resolver are SHARED with the
// Electron and Tauri runners. `role:<role>#<idx>` is an identity, so the walk
// that assigns the index and the walk that resolves it must be the same code,
// on every DOM runner. See shared/dom-walk.mjs for why there is one copy.
import { detectContentBugs, resolveStructuralTarget } from '../shared/dom-walk.mjs';
import {
  gridPoints,
  changedFraction,
  classifyPoint,
  probeRegionsToGroundtruth,
  DEFAULT_GRID,
} from './probe.mjs';
import { transientDivergence } from './flicker-oracle.mjs';
import {
  inspectReplayFinished,
  inspectReplayStep,
  inspectStepModel,
} from './inspect.mjs';
import { deadInputProbe } from './dead-input-oracle.mjs';
import { zeroContrastScan } from './zero-contrast-oracle.mjs';
import { scanAccessibilityStateParity } from './accessibility-state-oracle.mjs';
import { layoutOverflowScan, confirmLayoutOverflow } from './overflow-oracle.mjs';
import {
  occlusionScan,
  confirmOcclusions,
  securityScan,
  indicatorRelationshipScan,
  confirmRelationshipViolations,
  dupSubmitEligible,
  focusLossArm,
  focusLossCheck,
  blankScreenScan,
  brokenAssetScan,
  installCriticalResourceObserver,
  criticalResourceScan,
  zoomTappableKeys,
  zoomReflowScan,
  scrollRoundTripScan,
  installListenerLeakCounter,
  listenerLeakSample,
} from './hygiene-oracles.mjs';
import {
  CHOICE_OUTLIER_RATIO,
  CHOICE_MIN_MAGNITUDE,
  CHOICE_ROLES as CHOICE_ROLE_LIST,
  layoutDelta,
  medianOf,
  choiceAnomalyInPage,
  replayChoiceComponentInPage,
} from './choice-oracle.mjs';
import {
  ASSET_EXT_SOURCE,
  collectRouteLinks,
  inspectLinkedRoutes,
  isDeadRouteStatus,
  isSoftHandled,
  publicRouteKey,
  requestRouteKey,
  soft404View,
} from './route-inspection.mjs';

export {
  ASSET_EXT_SOURCE,
  collectRouteLinks,
  inspectLinkedRoutes,
  isAssetPath,
  isDeadRouteStatus,
  isSoftHandled,
  normalizePathname,
  publicRouteKey,
  requestRouteKey,
  soft404View,
} from './route-inspection.mjs';

const APP_URL = process.env.REPROIT_URL || 'http://localhost:8080';
const APP_ORIGIN = (() => {
  try {
    return new URL(APP_URL).origin;
  } catch (e) {
    return '';
  }
})();
const VIDEO_DIR = process.env.REPROIT_VIDEO_DIR || undefined;
const NETWORK_FILE = process.env.REPROIT_NETWORK_FILE || undefined;
const NETWORK_ACTOR = process.env.REPROIT_DEVICE || 'a';
const BACKEND_ENABLED = process.env.REPROIT_BACKEND === '1';
const BACKEND_BUILD = String(process.env.REPROIT_BUILD || '').slice(0, 128);
const BACKEND_CONFIG_CONTRACT = String(process.env.REPROIT_CONFIG_CONTRACT || '').slice(0, 128);
const BACKEND_ORIGINS = (() => {
  try {
    const values = JSON.parse(process.env.REPROIT_BACKEND_ORIGINS || '[]');
    const normalized = [APP_ORIGIN, ...(Array.isArray(values) ? values : [])]
      .map((value) => {
        try {
          const url = new URL(value);
          return /^https?:$/.test(url.protocol) ? url.origin : null;
        } catch (_) {
          return null;
        }
      })
      .filter(Boolean);
    return new Set(normalized);
  } catch (_) {
    return new Set([APP_ORIGIN].filter(Boolean));
  }
})();
// 0 is the immutable bootstrap phase; user actions are 1-based. This keeps
// initial API/config traffic hermetic without conflating it with the first tap.
let causalActionIndex = 0;
let causalOrdinal = 0;
// Set by installCapsuleReplay; called at each batch-run boundary so hermetic
// exchange matching starts every run with the full exchange budget.
let capsuleReplayReset = null;
let backendRequestOrdinal = 0;

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
    try {
      origin = new URL(u).origin;
    } catch (e) {
      continue;
    }
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
const TRACKER_SCRIPT_RE = new RegExp(
  's_code\\.js|adobedtm|\\bat\\.js\\b|fbevents\\.js|connect\\.facebook\\.' +
    'net|googletagmanager|\\/gtag(\\/|\\.js)|gtm\\.js|google-analytics\\.com|\\/' +
    'ga\\.js|\\/analytics\\.js|ima3\\.js|doubleclick\\.net|adsbygoogle|hotjar\\.' +
    'com|static\\.hotjar|cdn\\.mixpanel|cdn\\.segment\\.com|clarity\\.ms|\\/' +
    'clarity\\.js|cdn\\.optimizely|amplitude\\.com|fullstory\\.' +
    'com|quantserve|scorecardresearch|chartbeat|js-agent\\.newrelic\\.' +
    'com|nr-data\\.net|browser\\.sentry-cdn\\.com|bugsnag',
  'i',
);
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
const NONDET_ERROR_RE = new RegExp(
  'is not valid JSON|Unexpected end of JSON input|Failed to ' +
    'fetch|NetworkError when attempting to fetch|Load failed',
  'i',
);
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
const BENIGN_ERROR_RE = new RegExp(
  'Blocked a frame with origin|accessing a cross-origin frame|Permission ' +
    'denied to access property .* on cross-origin|ResizeObserver loop',
  'i',
);
export function exceptionIsBenign(message) {
  return BENIGN_ERROR_RE.test(String(message || ''));
}

// A blank DOM is not authority. Correlate it only with a newly observed,
// independently filtered application failure on the exact same URL. This is
// pure so the fail-closed boundary can be tested without launching a browser.
export function blankScreenAuthority(lastFailure, failureFloor, currentUrl) {
  if (!lastFailure || !Number.isInteger(lastFailure.sequence)) return null;
  if (lastFailure.sequence <= failureFloor || lastFailure.url !== currentUrl) return null;
  if (!['first-party-exception', 'renderer-crash'].includes(lastFailure.kind)) return null;
  return lastFailure.kind;
}
const HEADLESS = process.env.REPROIT_HEADLESS !== '0';
const INSPECT = process.env.REPROIT_INSPECT === '1';
const INSPECT_WAIT_MS = process.env.REPROIT_INSPECT_WAIT_MS;
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
// Universal framebuffer-probe floor. OPT-IN
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
    return o && typeof o === 'object' && !Array.isArray(o) ? o : {};
  } catch (_) {
    return {};
  }
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
    if (kind === 'testid')
      sel = '[data-testid="' + esc(val) + '"],[data-test-id="' + esc(val) + '"]';
    else if (kind === 'id') sel = '#' + esc(val);
    else if (kind === 'name') sel = '[name="' + esc(val) + '"]';
  }
  let els;
  try {
    els = document.querySelectorAll(sel);
  } catch (_) {
    return -1;
  }
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
  await page
    .evaluate(async () => {
      try {
        if (typeof window.__reproitReset === 'function') await window.__reproitReset();
      } catch (_) {}
      try {
        localStorage.clear();
      } catch (_) {}
      try {
        sessionStorage.clear();
      } catch (_) {}
      try {
        if (window.indexedDB && typeof indexedDB.databases === 'function') {
          const dbs = await indexedDB.databases();
          await Promise.all(
            (dbs || []).map((d) =>
              d && d.name
                ? new Promise((res) => {
                    let done = false;
                    const fin = () => {
                      if (!done) {
                        done = true;
                        res();
                      }
                    };
                    const req = indexedDB.deleteDatabase(d.name);
                    req.onsuccess = fin;
                    req.onerror = fin;
                    req.onblocked = fin;
                    setTimeout(fin, 500); // never hang the reset on a blocked delete
                  })
                : Promise.resolve(),
            ),
          );
        }
      } catch (_) {}
    })
    .catch(() => {});
}

export function validRouteAccessPath(route) {
  return (
    typeof route === 'string' &&
    route.startsWith('/') &&
    !route.startsWith('//') &&
    !route.includes('?') &&
    !route.includes('#') &&
    route.length <= 256 &&
    !/\s/.test(route)
  );
}

export async function visitRoute(page, requested, appOrigin) {
  if (!validRouteAccessPath(requested)) return null;
  const target = new URL(requested, appOrigin).href;
  let response = null;
  let navigationFailed = false;
  try {
    response = await page.goto(target, {
      waitUntil: 'domcontentloaded',
      timeout: 8000,
    });
  } catch (_) {
    navigationFailed = true;
  }
  // Client-side guards commonly redirect after a session probe. Give that
  // bounded work time to finish, then require a stable URL sample.
  await page.waitForTimeout(750);
  let previous = '';
  let stableSamples = 0;
  for (let sample = 0; sample < 12 && stableSamples < 3; sample++) {
    const observed = page.url();
    stableSamples = observed === previous ? stableSamples + 1 : 0;
    previous = observed;
    if (stableSamples < 3) await page.waitForTimeout(150);
  }
  let finalRoute = '';
  let sameOrigin = false;
  try {
    const finalUrl = new URL(page.url());
    sameOrigin = finalUrl.origin === appOrigin;
    if (sameOrigin) finalRoute = publicRouteKey(finalUrl.pathname);
  } catch (_) {}
  return {
    requested,
    finalRoute,
    status: response ? response.status() : null,
    settled: !navigationFailed && sameOrigin && stableSamples >= 3,
  };
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
      key: keyOf(el),
      node: el,
      text: (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 256),
      x: Math.round(r.x),
      y: Math.round(r.y),
      w: Math.round(r.width),
      h: Math.round(r.height),
    });
  }
  window.__reproitAnchors = anchors;
  window.__reproitAnchorDoc = document;
  return anchors.length;
}

function churnedAnchors(sel) {
  const old = window.__reproitAnchors;
  // No mark, or the document was replaced (navigation): not a flicker candidate.
  if (!old || window.__reproitAnchorDoc !== document) {
    window.__reproitAnchors = null;
    return null;
  }
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
    if (cur.has(k)) {
      dup.add(k);
      continue;
    }
    cur.set(k, el);
  }
  const churned = [];
  for (const a of old) {
    if (dup.has(a.key)) continue; // ambiguous key -> skip
    const now = cur.get(a.key);
    if (!now) continue; // gone in the new state -> a real removal, not flicker
    if (now === a.node) continue; // same node survived -> reconciled, no churn (good)
    const r = now.getBoundingClientRect();
    const sameBox =
      Math.round(r.x) === a.x &&
      Math.round(r.y) === a.y &&
      Math.round(r.width) === a.w &&
      Math.round(r.height) === a.h;
    const sameText = (now.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 256) === a.text;
    if (sameBox && sameText) churned.push(a.key); // unchanged yet rebuilt = flicker
  }
  window.__reproitAnchors = null;
  return churned;
}

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
// Drain the longtask buffer and return the classification for the action that
// just ran, or null when nothing crossed the jank floor. `kind` is 'hang' or
// 'jank'; `bucket` is the coarse blocked-time floor (deterministic detail).
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

// Read the cumulative forced-layout / style-recalc counters from the CDP
// Performance domain. Returns { layout, recalc } or null (non-chromium / no CDP).
async function readLayoutCounters(cdp) {
  if (!cdp) return null;
  try {
    const { metrics } = await cdp.send('Performance.getMetrics');
    const g = (n) => {
      const m = metrics.find((x) => x.name === n);
      return m ? m.value : 0;
    };
    return { layout: g('LayoutCount'), recalc: g('RecalcStyleCount') };
  } catch (_) {
    return null;
  }
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
const RAF_FRAME_MS = 100; // an inter-frame interval this long is a "long frame"
const RAF_JANK_RUN_MIN = 2; // a sustained jank run needs >= this many long frames
// One frame this long is jank on its own (> GC noise, < the 600ms fixture).
const RAF_JANK_LONE_MS = 350;

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
    if (intervals[i] < RAF_FRAME_MS) {
      i++;
      continue;
    }
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
  await page
    .addInitScript(() => {
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
      } catch (_) {
        /* no rAF: cross-engine jank/hang silent (never a false positive) */
      }
    })
    .catch(() => {});
}
