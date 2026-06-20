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
import { readFileSync, existsSync } from 'node:fs';
import { resolve } from 'node:path';

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

function loadFuzz() {
  const p = process.env.REPROIT_FUZZ_CONFIG;
  if (!p) return {};
  try { return JSON.parse(readFileSync(p, 'utf8')); } catch { return {}; }
}

// The list of per-seed fuzz configs to run in this session. Mirrors the other
// runners' batch contract (templates/explorer_headless.dart FuzzCfg.loadBatch,
// rn-runner, runners/linux-atspi.py load_batch): reproit's multi-seed fuzz
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

export { signatureOf, descriptorOf, valueClass };

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
        if (interactive(el, role)) {
          rawTaps.push({
            role, key: keyOf(el),
            label: name ? clipLabel(name) : '',
            unlabeled: !accessibleName(el),
          });
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
      // A control that exists in the DOM but isn't visible (e.g. behind an auth
      // gate) is not actionable: report it as a miss so a journey that assumed
      // it could reach this control is classified stale, not a silent pass.
      if (!visible(el)) return false;
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
        if (interactive(el, r) && r === role) { seen++; if (seen === idx) { target = el; return; } }
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
        if (interactive(el, r) && r === role) { seen++; if (seen === idx) { target = el; return; } }
        for (const c of el.children) walk(c);
      };
      const root = document.body || document.documentElement;
      if (root) walk(root);
      el = target;
    }
    if (!el) return false;
    // A field that isn't visible (behind an auth gate, a collapsed panel) is not
    // fillable: a miss, so the journey is stale rather than a silent pass.
    if (!visible(el)) return false;
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
  // runners' per-seed contract (rn-runner, runners/linux-atspi.py run_seed).
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
      const ok = await typeInto(page, sel, value);
      if (!ok) { log('FUZZ:MISS ' + act); stuck++; continue; }
      // Replays settle longer than the fuzz walk: under recording/CI load the
      // app's handler (and any uncaught throw it triggers) needs more wall-clock
      // to run and for `pageerror` to fire, so a deterministic crash isn't
      // missed. The fuzz walk stays fast.
      await page.waitForTimeout(replay ? 1100 : 700);
      // Typing + Enter can navigate (e.g. a search form submitting to another
      // origin). Stay on the app-under-test: drop off-origin destinations.
      if (await recoverIfOffOrigin()) { stuck++; current = await observe(); continue; }
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
    const ok = await tap(page, sel);
    if (!ok) { log('FUZZ:MISS ' + act); stuck++; continue; }
    // Replays settle longer than the fuzz walk (see the type branch): a
    // deterministic crash must have time to throw + flush `pageerror` under load.
    await page.waitForTimeout(replay ? 1100 : 700);
    // ORIGIN GUARD: a tap on an outbound link (footer "View on GitHub", a
    // social link) navigates off the app-under-test's origin. That page is NOT
    // a state of the app; recording it would make the whole map about the
    // foreign site. Recover (go back / re-goto) and do NOT record the state.
    if (await recoverIfOffOrigin()) { stuck++; current = await observe(); continue; }
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

main().catch((e) => {
  log('EXCEPTION CAUGHT BY WEB RUNNER');
  log(String(e && e.stack ? e.stack : e));
  log('Some tests failed');
  process.exit(0); // evidence already emitted; orchestrator judges by markers
});
