// ReproIt Electron runner. Electron's renderer is Chromium, so once we attach
// to its window we drive it exactly like the web runner: same DOM a11y
// snapshot, same CANONICAL structural signature, same marker protocol. Only the
// launch differs (we start the app binary instead of navigating to a URL).
//
// Env (set by drive.rs):
//   REPROIT_APP          path to the built Electron executable (packaged app),
//                        OR path to a dev app directory containing package.json
//                        (in which case the electron binary from that dir's
//                        node_modules is used automatically)
//   REPROIT_APP_DIR      alternative to REPROIT_APP for dev app directories;
//                        takes precedence when set
//   REPROIT_VIDEO_DIR    where to save the run video (optional)
//   REPROIT_FUZZ_CONFIG  fuzz config json (seed/budget/replay/prefix/edgeWeights)
//
// Status: validated end-to-end against a real Electron app (dev-dir mode).

// playwright is imported dynamically inside main() so this module stays
// import-safe (the parity test imports the host-pure signature functions
// below without needing the heavy runtime dependency installed).
import { readFileSync, statSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve as resolvePath, join as joinPath } from 'node:path';

const APP = process.env.REPROIT_APP_DIR || process.env.REPROIT_APP;
const VIDEO_DIR = process.env.REPROIT_VIDEO_DIR || undefined;
const ACTION_BUDGET = 36;
const MAX_LABEL_LEN = 40;
// Layer-1 value-class cap (docs/signature.md "Value-state"): once a structural
// node has shown more than this many DISTINCT value-class combinations, the
// runner drops it to structural-only so an adversarial value generator cannot
// explode the graph. The oracle is stateless; the cap is purely runner-local.
const VALUE_CLASS_CAP = 8;

function log(line) { process.stdout.write(line + '\n'); }

// Screenshot-capture contract (drive.rs): on a named "shoot" point, capture the
// current renderer window to $REPROIT_SHOTS_DIR/<name>.png, then print
// `SHOOT:<name>` so the orchestrator confirms the file and logs it. `name` is
// restricted to [A-Za-z0-9_/-] (the orchestrator filters to those anyway).
// Capture is via CDP `Page.captureScreenshot`: we open a CDP session on the
// renderer page (Electron's renderer is Chromium) and write the returned base64
// PNG to the path. If REPROIT_SHOTS_DIR is unset we skip the capture but STILL
// print the marker, so non-screenshot runs are unaffected.
async function shoot(page, name) {
  const dir = process.env.REPROIT_SHOTS_DIR;
  if (dir) {
    try {
      mkdirSync(dir, { recursive: true });
      const cdp = await page.context().newCDPSession(page);
      const { data } = await cdp.send('Page.captureScreenshot', { format: 'png' });
      writeFileSync(joinPath(dir, name + '.png'), Buffer.from(data, 'base64'));
      await cdp.detach().catch(() => {});
    } catch (e) { /* capture is best-effort; still emit the marker below */ }
  }
  log('SHOOT:' + name);
}

// Layer-3 opt-in (docs/signature.md "Value-state"): read `value_nodes:`
// selectors from reproit.yaml. We avoid adding a YAML dependency: the block is
// a simple flat list of strings, so a tiny line parser is enough and keeps the
// runner dependency-free. Path precedence: REPROIT_CONFIG env, else
// ./reproit.yaml in the cwd. A missing/unparseable file yields an empty list
// (value-less behavior, fully backward-compatible).
function loadValueNodes() {
  let p = (process.env.REPROIT_CONFIG || '').trim();
  if (!p) { const def = resolvePath(process.cwd(), 'reproit.yaml'); if (existsSync(def)) p = def; }
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
      const body = inline.replace(/^\[/, '').replace(/\].*$/, '');
      for (const part of body.split(',')) { const v = clean(part); if (v) out.push(v); }
      return out;
    }
    for (let j = i + 1; j < lines.length; j++) {
      const raw = lines[j];
      if (!raw.trim() || raw.trim().startsWith('#')) continue;
      const childIndent = raw.length - raw.replace(/^\s*/, '').length;
      if (childIndent <= indent) break;
      const item = raw.trim();
      if (!item.startsWith('-')) break;
      const v = clean(item.slice(1));
      if (v) out.push(v);
    }
    return out;
  }
  return out;
}

// FNV-1a over an arbitrary descriptor string. Used for the STRUCTURAL signature
// (fed a structure descriptor, never localized text) and for hashing long
// labels in clipLabel. Matches the web runner / Rust oracle.
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
//  web-runner/runner.mjs, and the golden vectors (signature_vectors.json).
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

function loadFuzz() {
  const p = process.env.REPROIT_FUZZ_CONFIG;
  if (!p) return {};
  try { return JSON.parse(readFileSync(p, 'utf8')); } catch { return {}; }
}

function rng(seed) {
  let s = (seed >>> 0) || 1;
  return (n) => { s ^= (s << 13); s >>>= 0; s ^= (s >> 17); s ^= (s << 5); s >>>= 0; return (s & 0x7fffffff) % n; };
}

// Determine launch mode: dev directory vs packaged executable.
// A dev directory has a package.json and its own node_modules/electron.
// A packaged executable is a regular file (or .app bundle on macOS).
function resolveElectronLaunch(app) {
  if (!app) return null;
  let isDir = false;
  try { isDir = statSync(app).isDirectory(); } catch { return null; }
  if (!isDir) {
    // Packaged executable path - existing behaviour, unchanged.
    return { executablePath: app, args: undefined };
  }
  // Dev app directory: find the electron binary inside its node_modules.
  // Support both direct node_modules/electron and local npm install layouts.
  const candidates = [
    resolvePath(app, 'node_modules', 'electron'),
    resolvePath(app, '..', 'node_modules', 'electron'),
  ];
  for (const candidate of candidates) {
    try {
      const req = createRequire(resolvePath(candidate, 'package.json'));
      // The electron npm package's main export is the path to the binary.
      const electronBin = req('./index.js');
      if (typeof electronBin === 'string') {
        return { executablePath: electronBin, args: [app] };
      }
    } catch { /* try next */ }
  }
  // Fallback: try resolving 'electron' from the app dir directly.
  try {
    const req = createRequire(resolvePath(app, 'package.json'));
    const electronBin = req('electron');
    if (typeof electronBin === 'string') {
      return { executablePath: electronBin, args: [app] };
    }
  } catch { /* not found */ }
  return null;
}

// Snapshot the DOM: a STRUCTURAL, locale-invariant signature plus display-only
// labels and the structural selectors for each tappable. Electron's renderer is
// Chromium, so this is identical to web-runner/runner.mjs: the signature is a
// hash of the canonical role tree + stable developer identifiers (data-testid,
// id, name, aria role, input type) + structural position, with ALL user-facing
// text excluded. Visible text is kept only as a display label for `map --show`,
// never folded into the hash or a selector. Elements are addressed by stable
// selector preference (data-testid > id > name > aria-role + structural index);
// a tappable lacking any stable id falls back to role+index and is flagged
// `nokey`.
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
    // inputs (password is never read). Identical to the web runner.
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
      if (tag === 'input') {
        const t = (el.getAttribute('type') || 'text').toLowerCase();
        return !['text', 'password', 'email', 'number', 'search'].includes(t);
      }
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
// true on success. Mirrors web-runner/runner.mjs's tap(). No visible text is
// ever used to locate the element.
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
        if (tag === 'input') {
          const t = (el.getAttribute('type') || 'text').toLowerCase();
          return !['text', 'password', 'email', 'number', 'search'].includes(t);
        }
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

    return false;
  }, { s: sel }).catch(() => false);
  return !!ok;
}

async function main() {
  if (!APP) { log('EXCEPTION CAUGHT BY REPROIT'); log('REPROIT_APP (executable path or dev dir) required'); log('═'.repeat(8)); process.exit(0); }
  const launch = resolveElectronLaunch(APP);
  if (!launch) { log('EXCEPTION CAUGHT BY REPROIT'); log('Could not resolve Electron binary from: ' + APP); log('═'.repeat(8)); process.exit(0); }
  const fuzz = loadFuzz();
  const launchOpts = {
    executablePath: launch.executablePath,
    recordVideo: VIDEO_DIR ? { dir: VIDEO_DIR } : undefined,
  };
  if (launch.args) launchOpts.args = launch.args;
  const { _electron: electron } = await import('playwright');
  const app = await electron.launch(launchOpts);
  const page = await app.firstWindow();
  page.on('pageerror', (err) => {
    log('EXCEPTION CAUGHT BY ELECTRON RENDERER');
    log('The following error was thrown:');
    log(String(err && err.message ? err.message : err));
    for (const line of String(err && err.stack ? err.stack : '').split('\n').slice(0, 8)) log(line);
    log('═'.repeat(8));
  });

  log('JOURNEY claimed role=a');
  await page.waitForTimeout(1200);
  const seen = new Set(), tried = new Set();
  const pick = rng(fuzz.seed || 0);

  // Layer-3 opt-in value-node selectors from reproit.yaml (empty if none).
  const valueNodeSelectors = loadValueNodes();
  if (valueNodeSelectors.length) log(`JOURNEY[a] step: value_nodes=${valueNodeSelectors.length}`);

  // Layer-1 hard cap (docs/signature.md "Value-state"): per structural node,
  // track the DISTINCT value-class combinations seen. Once a node exceeds
  // VALUE_CLASS_CAP, fall back to its structural-only signature for the rest of
  // the run so an adversarial value generator cannot explode the graph.
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

  const observe = async () => {
    const snap = await snapshot(page, valueNodeSelectors);
    snap.sig = effectiveSig(snap);
    if (!seen.has(snap.sig)) {
      seen.add(snap.sig);
      // sig: STRUCTURAL (roles + tree shape + stable developer keys),
      //      locale-invariant.
      // labels: DISPLAY-ONLY visible text (map --show), never in the sig.
      // elements: structural selectors for replay; `nokey` flags a tappable
      //           with no stable id (data-testid/id/name).
      log('EXPLORE:STATE ' + JSON.stringify({
        sig: snap.sig,
        // route: the URL path, so the candidate map reconciles by route (the
        // reliable join key), consistent with the web and Flutter runners.
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
  };

  let current = await observe(), stuck = 0;
  const prefix = fuzz.prefix || null, replay = fuzz.replay || null;
  const prefixLen = prefix ? prefix.length : 0;
  const budget = replay ? replay.length : ((fuzz.budget || ACTION_BUDGET) + prefixLen);
  for (let a = 0; a < budget && stuck < 3; a++) {
    let act;
    if (replay) act = replay[a];
    else if (prefix && a < prefixLen) act = prefix[a];
    else if (fuzz.seed) {
      // Inverse-visit-count weighted pick over STRUCTURAL selectors (key, else
      // role+index), never visible text, so seeded picks and replays are
      // locale-invariant and reproduce exactly.
      const taps = current.tappables.map((e) => e.sel).sort();
      const ew = (fuzz.edgeWeights && fuzz.edgeWeights[current.sig]) || {};
      const options = taps.map((s) => 'tap:' + s).concat(['back']);
      const weights = options.map((o) => 1 / (1 + (ew[o] || 0)));
      const total = weights.reduce((x, y) => x + y, 0);
      let r = (pick(1 << 20) / (1 << 20)) * total;
      act = options[options.length - 1];
      for (let k = 0; k < options.length; k++) { r -= weights[k]; if (r <= 0) { act = options[k]; break; } }
    } else {
      act = null;
      for (const el of current.tappables) { if (!tried.has(current.sig + '|' + el.sel)) { act = 'tap:' + el.sel; break; } }
      act = act || 'back';
    }
    log('FUZZ:ACT ' + act);
    if (act.startsWith('shoot:')) {
      // Screenshot point (e.g. a `do: shoot:<name>` journey/tour step): capture
      // the renderer window to REPROIT_SHOTS_DIR and emit the SHOOT marker. It
      // does not move the known state, so no observe/stuck change.
      await shoot(page, act.slice('shoot:'.length));
      continue;
    }
    if (act === 'back') {
      const before = current.sig;
      const beforeContent = current.content;
      await page.goBack({ timeout: 3000 }).catch(() => {});
      await page.waitForTimeout(600);
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
      current = next; continue;
    }
    const sel = act.slice('tap:'.length);
    tried.add(current.sig + '|' + sel);
    const before = current.sig;
    const beforeContent = current.content;
    if (!(await tap(page, sel))) { log('FUZZ:MISS ' + act); stuck++; continue; }
    await page.waitForTimeout(700);
    const next = await observe();
    if (next.sig !== before) {
      log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'tap:' + sel, to: next.sig }));
      stuck = 0;
    } else if (next.content !== beforeContent) {
      // Layer-1 effect detection: the tap changed displayed content (a capped
      // value display) without a structural move. EFFECTIVE, so reset stuck and
      // keep driving; no self-edge is recorded.
      stuck = 0;
    }
    current = next;
  }
  log(`JOURNEY[a] step: explored ${seen.size} states`);
  log('JOURNEY DONE');
  log('All tests passed');
  await app.close();
}

// Only auto-run when invoked as the entry point. When imported (e.g. by the
// parity test) the canonical signature is exported without launching Electron.
const INVOKED_DIRECTLY = process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href;
if (INVOKED_DIRECTLY) {
  main().catch((e) => {
    log('EXCEPTION CAUGHT BY ELECTRON RUNNER');
    log(String(e && e.stack ? e.stack : e));
    log('Some tests failed');
    process.exit(0);
  });
}
