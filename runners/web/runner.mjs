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
//                 selector = "key:<kind>:<v>" (data-testid/id/name) or
//                 "role:<role>#<idx>" (aria role + structural index), never text.
//
// Invoked by the orchestrator's web-playwright runner with env:
//   REPROIT_URL          the app URL to explore
//   REPROIT_VIDEO_DIR    where to save the run video (optional)
//   REPROIT_FUZZ_CONFIG  path to fuzz config json (seed/budget/replay/prefix)
//   REPROIT_HEADLESS     "0" to show the browser (default headless)
//
// stdout is the marker stream; the orchestrator captures it like a drive log.

import { chromium, firefox, webkit } from 'playwright';
import { readFileSync, existsSync, mkdirSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { PNG } from 'pngjs';
import {
  gridPoints, changedFraction, classifyPoint, probeRegionsToGroundtruth, DEFAULT_GRID,
} from './probe.mjs';
import { transientDivergence } from './flicker-oracle.mjs';

const APP_URL = process.env.REPROIT_URL || "http://localhost:8080";
const VIDEO_DIR = process.env.REPROIT_VIDEO_DIR || undefined;
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
const ENGINES = { chromium, firefox, webkit };
const BROWSER = ENGINES[ENGINE] || chromium;
// Universal framebuffer-probe floor (PIECE 2, docs/operability-graph.md). OPT-IN
// because it is SIDE-EFFECTING + coarse: it synthesizes clicks at a small grid
// and diffs screenshots to find operable regions with no a11y control (e.g. a
// canvas/WebGL hit area). Off unless REPROIT_PROBE=1. See probe.mjs.
const PROBE = process.env.REPROIT_PROBE === '1';

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
const ACTION_BUDGET = 36;
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
// ./reproit.yaml in the cwd. A missing/unparseable file yields an empty list
// (value-less behavior, fully backward-compatible).
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

function log(line) { process.stdout.write(line + '\n'); }

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
// ({seed, budget, edgeWeights, prefix, replay, ...}). A single-seed (legacy)
// run writes the bare {"seed":..} object with no "batch" key. Returns
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

// FNV-1a over an arbitrary descriptor string. Used for the STRUCTURAL signature
// (fed a structure descriptor, never localized text) and for hashing long
// labels in clipLabel. Matches explorer.dart's fnv1a so seeds/hashes line up.
function fnv1a(s) {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return (h >>> 0).toString(16).padStart(8, '0');
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
  pairs.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  return '\nV:' + pairs.map((p) => p[0] + '=' + p[1]).join(';');
}

function descriptorOf(anchor, root) {
  const tokens = [];
  const norm = normalizeNode(root);
  if (norm) serializeNode(norm, 0, false, tokens);
  return 'A:' + (anchor == null ? '' : anchor) + '\n' + tokens.join(';') + valueSection(root);
}
function signatureOf(anchor, root) { return fnv1a(descriptorOf(anchor, root)); }

export { signatureOf, descriptorOf, valueClass, snapshot };

// Snapshot the DOM: a STRUCTURAL, locale-invariant signature plus display-only
// labels and the structural selectors for each tappable. Mirrors
// templates/explorer.dart: the signature is a hash of the tag/role tree shape +
// stable developer identifiers (data-testid, id, name, aria role, input type) +
// structural position, with ALL user-facing text excluded. Visible text is kept
// only as a display label for `map --show`, never folded into the hash or into a
// selector. Elements are addressed by stable selector preference
// (data-testid > id > name > aria-role + structural index); a tappable lacking
// any stable id falls back to role+index and is flagged `nokey`.
async function snapshot(page, valueNodeSelectors) {
  const snap = await page.evaluate(({ maxLen, valueNodeSelectors }) => {
    const labels = [];          // DISPLAY-ONLY visible text
    const rawTaps = [];         // tappable nodes in document order
    const extraTaps = [];       // keyed pointer-operable nodes interactive() drops
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

    // Stable developer id: data-testid > id > name (for the descriptor token).
    const idOf = (el) => {
      const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
      if (testid && testid.trim()) return testid.trim();
      const id = el.getAttribute('id');
      if (id && id.trim()) return id.trim();
      const name = el.getAttribute('name');
      if (name && name.trim()) return name.trim();
      return null;
    };

    // Selector KEY (for replay): kind-tagged so tap() can resolve it.
    const keyOf = (el) => {
      const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
      if (testid && testid.trim()) return 'testid:' + testid.trim();
      const id = el.getAttribute('id');
      if (id && id.trim()) return 'id:' + id.trim();
      const name = el.getAttribute('name');
      if (name && name.trim()) return 'name:' + name.trim();
      return null;
    };

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
    const accessibleName = (el) => {
      const aria = el.getAttribute('aria-label');
      if (aria && aria.trim()) return true;
      const title = el.getAttribute('title');
      if (title && title.trim()) return true;
      const alt = el.getAttribute('alt');
      if (alt && alt.trim()) return true;
      const text = (el.innerText || el.textContent || '').trim();
      return text.length > 0;
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
          rawTaps.push({
            role, key: keyOf(el),
            label: name ? clipLabel(name) : '',
            unlabeled: !accessibleName(el),
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
              unlabeled: !accessibleName(el),
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
    let unlabeled = 0;
    const tappables = rawTaps.map((tn) => {
      const idx = perRole[tn.role] || 0;
      perRole[tn.role] = idx + 1;
      if (tn.unlabeled) unlabeled++;
      const sel = tn.key ? 'key:' + tn.key : 'role:' + tn.role + '#' + idx;
      return { sel, role: tn.role, index: idx, key: tn.key, label: tn.label };
    });
    // Append the keyed pointer-operable extras (keyed selector only; no role
    // index, so nothing above shifts). Dedup against selectors already present
    // so an element can never appear twice in the candidate set.
    const present = new Set(tappables.map((t) => t.sel));
    for (const tn of extraTaps) {
      const sel = 'key:' + tn.key;
      if (present.has(sel)) continue;
      present.add(sel);
      if (tn.unlabeled) unlabeled++;
      tappables.push({ sel, role: tn.role, index: -1, key: tn.key, label: tn.label });
    }

    // Anchor: route/path of the current screen.
    let anchor = null;
    try { if (location && location.pathname) anchor = location.pathname; } catch (e) {}

    // Layer-1 content fingerprint source: sorted (stable-key, trimmed text) over
    // value + keyed-text nodes. Sorted here so it is order-independent.
    textNodes.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : (a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0)));

    return { tree, anchor, labels: [...new Set(labels)], tappables, unlabeled, textNodes };
  }, { maxLen: MAX_LABEL_LEN, valueNodeSelectors: valueNodeSelectors || [] });

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
    // the motivating one). Such broader-only elements are keyed by their stable
    // id when they have one (key:id:...), so they still join to a real element.
    const perRole = {};
    const root = document.body || document.documentElement;
    const walk = (el, isRoot) => {
      if (!isRoot && !visible(el)) { for (const c of el.children) walk(c, false); return; }
      if (!isRoot) {
        const role = roleOf(el);
        // The tappable walk takes only REACHABLE interactives, lockstep with
        // snapshot(), so role:<role>#<idx> indices match EXPLORE:STATE.
        const inTappableWalk = interactive(el, role) && reachable(el);
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
            sel, role, native, cursor, deleg,
            // reachable: a real user can hit this (on-screen + hit-testable). The
            // keyboard-activation probe must NOT focus+Enter an UNreachable control
            // (offstage / occluded), doing so fires its handler and lets reproit
            // reach a control a user can't, e.g. an offstage submit that throws.
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
  // Start from a clean baseline: blur whatever is focused onto body.
  await page.evaluate(() => { try { if (document.activeElement) document.activeElement.blur(); document.body.focus(); } catch (e) {} });
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
    for (let k = firstReal + 1; k < visited.length; k++) if (visited[k] === -2) { returnedToBody = true; break; }
  }
  const focusTrap = firstReal >= 0 && !returnedToBody && inTab.size > 0 && inTab.size < count;
  return { inTab, focusTrap };
}

// Decode a Playwright PNG screenshot Buffer into a flat RGBA pixel array. Pure
// wrapper over pngjs so the diff (probe.mjs changedFraction) stays host-pure.
function pngToRgba(buf) {
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
  try { await page.goto(APP_URL, { waitUntil: 'networkidle' }); await page.waitForTimeout(300); } catch (_) {}
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
async function tap(page, sel) {
  const ok = await page.evaluate(({ s }) => {
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
      el.click();
      return true;
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
      target.click();
      return true;
    }

    // Label selector: an explicit `label:` prefix or a bare string, resolved by
    // visible text / aria-label. An ACTION selector only needs to be stable
    // within the run's locale; the state signature stays structural. Parity with
    // typing-by-label and Playwright/Appium addressing by visible name. Prefer an
    // exact accessible-name match on an interactive element, then a contains.
    {
      const want = (s.startsWith('label:') ? s.slice('label:'.length) : s).trim().toLowerCase();
      if (want) {
        const els = Array.from(
          document.querySelectorAll('a,button,[role],input,select,textarea,[onclick],[tabindex]')
        ).filter(visible);
        const nameOf = (el) =>
          (el.getAttribute('aria-label') || el.value || el.textContent || '').trim().toLowerCase();
        const el = els.find((e) => nameOf(e) === want) || els.find((e) => nameOf(e).includes(want));
        if (el) { el.click(); return true; }
      }
    }

    return false;
  }, { s: sel }).catch(() => false);
  return !!ok;
}

// STRUCTURAL type: resolve the SAME locale-invariant selector as tap() and type
// `value` into the field, then press Enter (many apps, e.g. TodoMVC's new-todo,
// commit on Enter). Focuses the element, sets its value, and dispatches the
// input/change events frameworks listen for. Returns true on success. The
// selector resolution mirrors tap() exactly so role+index addressing lines up.
async function typeInto(page, sel, value) {
  const found = await page.evaluate(({ s }) => {
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
    return true;
  }, { s: sel }).catch(() => false);
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
async function execScenarioAction(page, act, who) {
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
    const value = ADVERSARIAL_BY_ID[valId] !== undefined ? ADVERSARIAL_BY_ID[valId] : expandEnv(valId);
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
    log('EXCEPTION CAUGHT BY WEB PAGE');
    log('actor ' + who + ': ' + String(err && err.message ? err.message : err));
    const stack = (err && err.stack) ? String(err.stack) : '';
    for (const line of stack.split('\n').slice(0, 8)) log(line);
    log('════════');
  });
  await page.goto(APP_URL, { waitUntil: 'networkidle' }).catch(() => {});
  log('JOURNEY claimed role=' + who);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  for (let guard = 0; guard < 100000; guard++) {
    let body = 'WAIT';
    try { body = (await (await fetch(base + '/next?device=' + who)).text()).trim(); }
    catch { await sleep(100); continue; }
    if (body === 'DONE') break;
    if (body === 'WAIT') { await sleep(40); continue; }
    const act = body.startsWith('ACT\t') ? body.slice(4) : body;
    await execScenarioAction(page, act, who);
    try { await fetch(base + '/done?device=' + who, { method: 'POST' }); } catch (_) {}
  }
  await page.waitForTimeout(500); // flush a trailing pageerror before teardown
  log('JOURNEY DONE');
  log('All tests passed');
  await ctx.close().catch(() => {});
}

async function main() {
  console.log(`JOURNEY[a] step: engine=${ENGINE}`);
  const browser = await BROWSER.launch({ headless: HEADLESS });
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
  const contextOpts = {};
  if (VIDEO_DIR) contextOpts.recordVideo = { dir: VIDEO_DIR, size: { width: 1280, height: 800 } };
  if (LOCALE) {
    contextOpts.locale = LOCALE;
    contextOpts.extraHTTPHeaders = { 'Accept-Language': `${LOCALE},${LOCALE.split('-')[0]};q=0.9` };
    console.log(`JOURNEY[a] step: locale=${LOCALE}`);
  }
  const context = await browser.newContext(contextOpts);
  const page = await context.newPage();
  // CDP session for ground-truth operability (DOMDebugger.getEventListeners):
  // detects real click/pointer listeners on elements and the document/body
  // delegation pattern. Chromium-only; firefox/webkit have no CDP, so the
  // ground-truth falls back to native + cursor + delegation-marker signals.
  let gtCdp = null;
  if (ENGINE === 'chromium') {
    try { gtCdp = await context.newCDPSession(page); } catch (e) { gtCdp = null; }
  }

  // Exception oracle: uncaught page errors (a throw in an onclick, an
  // unhandled rejection) become the same EXCEPTION block the Flutter
  // pipeline emits, so the fuzz oracle and exceptions.jsonl pick them up.
  const emitError = (err) => {
    log('EXCEPTION CAUGHT BY WEB PAGE');
    log('The following error was thrown:');
    log(String(err && err.message ? err.message : err));
    const stack = (err && err.stack) ? String(err.stack) : '';
    for (const line of stack.split('\n').slice(0, 8)) log(line);
    log('\u2550\u2550\u2550\u2550\u2550\u2550\u2550\u2550');
  };
  page.on('pageerror', emitError);

  // Ready marker so the orchestrator starts its clock; matches the Dart
  // explorer's claim line.
  log('JOURNEY claimed role=a');
  await page.goto(APP_URL, { waitUntil: 'networkidle' }).catch(() => {});
  await page.waitForTimeout(800);

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

  const APP_ORIGIN = new URL(APP_URL).origin;
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
      await page.goto(APP_URL, { waitUntil: 'networkidle' }).catch(() => {});
      await page.waitForTimeout(400);
    }
    return true;
  }

  // Re-pump a fresh starting screen between seeds. The Flutter explorer rebuilds
  // a clean widget tree per seed; the web analogue is to navigate back to the
  // app start URL so each seed begins from the same clean state. Session-wide
  // (browser/context/page + the value cap) survives; per-seed state does not.
  async function resetToRoot() {
    await page.goto(APP_URL, { waitUntil: 'networkidle' }).catch(() => {});
    await page.waitForTimeout(500);
  }

  // Explore/replay ONE seed, emitting the same EXPLORE:STATE / EXPLORE:EDGE /
  // FUZZ:ACT / FUZZ:MISS markers as a single-seed run. Seen states + tried edges
  // are LOCAL to the seed so per-seed coverage is independent, matching the other
  // runners' per-seed contract (runners/rn, runners/linux-atspi.py run_seed).
  async function runSeed(fuzz) {
    const seenStates = new Set();
    const triedEdges = new Set();
    const pick = rng(fuzz.seed || 0);
    const replay = fuzz.replay || null;
    if (fuzz.seed) log(`JOURNEY[a] step: fuzz seed=${fuzz.seed}`);

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
        // labels: DISPLAY-ONLY visible text (map --show), never in the sig.
        // elements: structural selectors for replay; `nokey` flags a tappable
        //           with no stable id (data-testid/id/name) so the map layer can
        //           warn the developer to add one.
        log('EXPLORE:STATE ' + JSON.stringify({
          sig: snap.sig,
          // route: the URL path, so the candidate map can reconcile by route
          // (the reliable, framework-neutral join key) and not just by name.
          ...(snap.anchor ? { route: snap.anchor } : {}),
          labels: snap.labels.slice(0, 24),
          elements: snap.tappables.slice(0, 24).map((e) => {
            const o = { sel: e.sel, role: e.role, label: e.label };
            if (!e.key) o.nokey = true;
            return o;
          }),
          unlabeled: snap.unlabeled,
        }));
        // Operability/accessibility ground truth for this newly-seen state,
        // keyed by the SAME sig. Emitted once per state (alongside the
        // EXPLORE:STATE line). The keyboard-activation probe inside can mutate
        // the DOM, so it runs AFTER the snapshot was captured and the state was
        // recorded; the next action then drives the live (possibly mutated) DOM.
        await emitGroundtruth(page, gtCdp, snap.sig);
      }
      return snap;
    }

    let current = await observe();
    let stuck = 0;
    const prefix = fuzz.prefix || null;
    const prefixLen = prefix ? prefix.length : 0;
    const budget = replay ? replay.length : ((fuzz.budget || ACTION_BUDGET) + prefixLen);

    for (let actions = 0; actions < budget && stuck < 3; actions++) {
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
      const taps = current.tappables.map((e) => e.sel).sort();
      const textSels = current.tappables.filter((e) => e.role === 'textfield').map((e) => e.sel).sort();
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
      act = null;
      for (const el of current.tappables) {
        // Prefer an untried type edge for text fields (use the plain value in
        // the non-seeded walk; the seeded walk explores the adversarial set).
        const edge = el.role === 'textfield' ? 'type:' + el.sel + '=normal' : 'tap:' + el.sel;
        if (!triedEdges.has(current.sig + '|' + edge)) { act = edge; break; }
      }
      act = act || 'back';
    }

    log('FUZZ:ACT ' + act);
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
      await page.goto(APP_URL, { waitUntil: 'networkidle' }).catch(() => {});
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
      const beforeContent = current.content;
      const origin = new URL(APP_URL).origin;
      await page.goBack({ timeout: 3000 }).catch(() => {});
      await page.waitForTimeout(600);
      // Stepping off the app (about:blank) is not a real state: go forward.
      if (!page.url().startsWith(origin)) {
        await page.goto(APP_URL, { waitUntil: 'networkidle' }).catch(() => {});
        await page.waitForTimeout(400);
        stuck++;
        current = await observe();
        continue;
      }
      const next = await observe();
      if (next.sig !== before) {
        log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'back', to: next.sig }));
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
      // type:<sel>=<valueId> -> focus the field and type the adversarial value.
      const body = act.slice('type:'.length);
      const eq = body.lastIndexOf('=');
      const sel = eq >= 0 ? body.slice(0, eq) : body;
      const valId = eq >= 0 ? body.slice(eq + 1) : 'normal';
      const value = ADVERSARIAL_BY_ID[valId] !== undefined ? ADVERSARIAL_BY_ID[valId] : expandEnv(valId);
      triedEdges.add(current.sig + '|' + act);
      const before = current.sig;
      const beforeContent = current.content;
      await page.evaluate(markAnchors, ANCHOR_SEL).catch(() => {}); // flicker oracle: tag persistent chrome
      const typePix = await startScreencastCapture(gtCdp); // Tier-2 (gated): record presented frames
      const ok = await typeInto(page, sel, value);
      if (!ok) { if (typePix) await typePix.stop(); log('FUZZ:MISS ' + act); stuck++; continue; }
      // Replays settle longer than the fuzz walk: under recording/CI load the
      // app's handler (and any uncaught throw it triggers) needs more wall-clock
      // to run and for `pageerror` to fire, so a deterministic crash isn't
      // missed. The fuzz walk stays fast.
      await page.waitForTimeout(replay ? 1100 : 700);
      // Typing + Enter can navigate (e.g. a search form submitting to another
      // origin). Stay on the app-under-test: drop off-origin destinations.
      if (await recoverIfOffOrigin()) { if (typePix) await typePix.stop(); stuck++; current = await observe(); continue; }
      await finishScreencastCapture(typePix, before, 'type:' + sel + '=' + valId);
      const typeChurn = await page.evaluate(churnedAnchors, ANCHOR_SEL).catch(() => null);
      if (typeChurn && typeChurn.length) {
        log('EXPLORE:RERENDER ' + JSON.stringify({ from: before, action: 'type:' + sel + '=' + valId, churned: typeChurn }));
      }
      const next = await observe();
      if (next.sig !== before) {
        log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'type:' + sel + '=' + valId, to: next.sig }));
        stuck = 0;
      } else if (next.content !== beforeContent) {
        stuck = 0; // Layer-1: content changed without a structural move; effective.
      } else stuck++;
      current = next;
      continue;
    }
    const sel = act.slice('tap:'.length);
    triedEdges.add(current.sig + '|' + sel);
    const before = current.sig;
    const beforeContent = current.content;
    await page.evaluate(markAnchors, ANCHOR_SEL).catch(() => {}); // flicker oracle: tag persistent chrome
    const tapPix = await startScreencastCapture(gtCdp); // Tier-2 (gated): record presented frames
    const ok = await tap(page, sel);
    if (!ok) { if (tapPix) await tapPix.stop(); log('FUZZ:MISS ' + act); stuck++; continue; }
    // Replays settle longer than the fuzz walk (see the type branch): a
    // deterministic crash must have time to throw + flush `pageerror` under load.
    await page.waitForTimeout(replay ? 1100 : 700);
    // ORIGIN GUARD: a tap on an outbound link (footer "View on GitHub", a
    // social link) navigates off the app-under-test's origin. That page is NOT
    // a state of the app; recording it would make the whole map about the
    // foreign site. Recover (go back / re-goto) and do NOT record the state.
    if (await recoverIfOffOrigin()) { if (tapPix) await tapPix.stop(); stuck++; current = await observe(); continue; }
    await finishScreencastCapture(tapPix, before, 'tap:' + sel);
    // Tier-1 flicker oracle: did this transition rebuild persistent chrome that
    // did not change? (DOM node-identity churn; settled either way, so invisible
    // to the visual oracle.) Reported per transition, independent of whether the
    // structural sig moved.
    const tapChurn = await page.evaluate(churnedAnchors, ANCHOR_SEL).catch(() => null);
    if (tapChurn && tapChurn.length) {
      log('EXPLORE:RERENDER ' + JSON.stringify({ from: before, action: 'tap:' + sel, churned: tapChurn }));
    }
    const next = await observe();
    if (next.sig !== before) {
      log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'tap:' + sel, to: next.sig }));
      stuck = 0;
    } else if (next.content !== beforeContent) {
      // Layer-1 effect detection: the tap changed displayed content (a calculator
      // keypress on a capped display) without a structural move. EFFECTIVE, so
      // reset stuck and keep driving; no self-edge is recorded.
      stuck = 0;
    }
    current = next;
  }

    log(`JOURNEY[a] step: explored ${seenStates.size} states`);
  }

  // Run every seed in this session in sequence. For a multi-seed batch
  // ({"batch":[...]}) wrap EACH seed's walk in SEED:BEGIN <seed> ... SEED:END
  // <seed> so the Rust side (fuzz.rs split_seed_segments) attributes coverage,
  // trace, and findings to the right seed; between seeds re-pump a fresh start
  // screen so each seed begins clean. A single-seed (legacy {"seed":..}) run
  // emits NO SEED markers, preserving the byte-for-byte single-seed path.
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
