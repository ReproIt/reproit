// ReproIt Tauri runner. Tauri renders in the system webview (WKWebView /
// WebView2 / WebKitGTK), which is driven over the W3C WebDriver protocol by
// `tauri-driver`. We connect with webdriverio and run the SAME DOM a11y
// snapshot as the web/Electron runners via browser.execute(), producing the
// SAME CANONICAL structural signature; only the transport differs (WebDriver
// instead of CDP). The webview builds a canonical Node tree, returns it to the
// host, and the host hashes it with the byte-identical canonical pipeline.
//
// Prereqs (on the host): `tauri-driver` and the platform webdriver
// (msedgedriver / WebKitWebDriver) on PATH. Start `tauri-driver` first, or set
// REPROIT_WEBDRIVER_URL to a running endpoint.
//
// Env: REPROIT_APP (built Tauri binary), REPROIT_FUZZ_CONFIG, REPROIT_WEBDRIVER_URL.
// Status: validated end-to-end against a real Tauri v2 Linux app via
// tauri-driver + WebKitWebDriver under Xvfb (Ubuntu 24.04 in Docker).

// webdriverio is imported dynamically inside main() so this module stays
// import-safe (the parity test imports the host-pure signature functions
// below without needing the heavy runtime dependency installed).
import { readFileSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve as resolvePath, join as joinPath } from 'node:path';
import { execFileSync } from 'node:child_process';
import { platform as osPlatform } from 'node:os';
// CHOICE-ANOMALY oracle, shared with the web + electron runners. We inject the
// SAME self-contained in-page pass into the webview via executeAsync() (the way
// every other oracle is injected on Tauri, which has no CDP); it works over WebKit
// or WebView2 alike because it only touches the live DOM + layout. The constants
// are the single source of truth for the outlier thresholds. Host-pure +
// dependency-free, so a static import keeps this module import-safe for the parity
// test (it imports the signature functions without the webdriverio runtime).
import {
  CHOICE_ANOMALY_IN_PAGE_SRC, CHOICE_OUTLIER_RATIO, CHOICE_MIN_MAGNITUDE, CHOICE_ROLES,
} from './web/choice-oracle.mjs';

// The choice-anomaly pass as an executeAsync() body. WebDriver executeAsync passes
// a `done` callback as the FINAL argument; the choice pass is async (it waits for
// layout to settle between options), so we run it then hand its findings to done.
// Built from CHOICE_ANOMALY_IN_PAGE_SRC (the exact function unit-tested via the web
// runner's page.evaluate) so there is no second copy to drift. The thresholds are
// interpolated from the shared constants.
const CHOICE_ANOMALY_ASYNC_JS = `
  var __reproitChoiceFn = ${CHOICE_ANOMALY_IN_PAGE_SRC};
  var __reproitDone = arguments[arguments.length - 1];
  __reproitChoiceFn({
    settleMs: 600,
    ratio: ${CHOICE_OUTLIER_RATIO},
    minMag: ${CHOICE_MIN_MAGNITUDE},
    choiceRoles: ${JSON.stringify(CHOICE_ROLES)},
  }).then(function (findings) { __reproitDone(findings || []); })
    .catch(function () { __reproitDone([]); });
`;

const APP = process.env.REPROIT_APP;
const WD_URL = process.env.REPROIT_WEBDRIVER_URL || 'http://127.0.0.1:4444';
const ACTION_BUDGET = 36;
const MAX_LABEL_LEN = 40;
// Layer-1 value-class cap (docs/signature.md "Value-state"): once a structural
// node has shown more than this many DISTINCT value-class combinations, the
// runner drops it to structural-only so an adversarial value generator cannot
// explode the graph. The oracle is stateless; the cap is purely runner-local.
const VALUE_CLASS_CAP = 8;

function log(line) { process.stdout.write(line + '\n'); }
function loadFuzz() { const p = process.env.REPROIT_FUZZ_CONFIG; if (!p) return {}; try { return JSON.parse(readFileSync(p, 'utf8')); } catch { return {}; } }

// Screenshot-capture contract (drive.rs): on a named "shoot" point, capture the
// current webview to $REPROIT_SHOTS_DIR/<name>.png, then print `SHOOT:<name>` so
// the orchestrator confirms the file and logs it. `name` is restricted to
// [A-Za-z0-9_/-] (the orchestrator filters to those anyway). Capture is the W3C
// WebDriver "Take Screenshot" command (browser.takeScreenshot in webdriverio),
// which returns the PNG as base64; we write those bytes to the path. If
// REPROIT_SHOTS_DIR is unset we skip the capture but STILL print the marker, so
// non-screenshot runs are unaffected.
async function shoot(browser, name) {
  const dir = process.env.REPROIT_SHOTS_DIR;
  if (dir) {
    try {
      mkdirSync(dir, { recursive: true });
      const b64 = await browser.takeScreenshot();
      writeFileSync(joinPath(dir, name + '.png'), Buffer.from(b64, 'base64'));
    } catch (e) { /* capture is best-effort; still emit the marker below */ }
  }
  log('SHOOT:' + name);
}

// Layer-3 opt-in (docs/signature.md "Value-state"): read `value_nodes:`
// selectors from reproit.yaml. We avoid adding a YAML dependency: the block is
// a simple flat list of strings, so a tiny line parser is enough and keeps the
// runner dependency-free. Path precedence: REPROIT_CONFIG env, else
// ./reproit.yaml in the cwd. A missing/unparseable file yields an empty list,
// so value-state is strictly opt-in.
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
function rng(seed) { let s = (seed >>> 0) || 1; return (n) => { s ^= (s << 13); s >>>= 0; s ^= (s >> 17); s ^= (s << 5); s >>>= 0; return (s & 0x7fffffff) % n; }; }

// The shared UTF-8 encoder for the canonical hash + V: byte-order sort. The
// descriptor and V: keys can carry non-ASCII (a localized anchor, a non-ASCII
// id, an emoji icon), so we MUST fold the UTF-8 BYTES, exactly like the Rust
// oracle's `desc.as_bytes()`. Folding UTF-16 code units silently diverged.
const REPROIT_UTF8 = new TextEncoder();

// FNV-1a over the UTF-8 BYTES of an arbitrary descriptor string. Used for the
// STRUCTURAL signature (fed a structure descriptor). Matches the web runner /
// Rust oracle.
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
//  runners/web/runner.mjs, and the golden vectors (signature_vectors.json).
//  Spec: docs/signature.md. This block is host-pure (no DOM) so the parity
//  test imports it directly; the webview-side snapshot() builds a Node tree in
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

export { signatureOf, descriptorOf, valueClass };

// The DOM walk runs INSIDE the webview via execute(); identical canonical
// DOM->Node logic to runners/web/runner.mjs. It returns a canonical Node tree
// (role + id + type + icon + children) plus display-only labels and the
// structural selectors for each tappable. ALL user-facing text is excluded from
// the tree; visible text is kept only as a display label for `map show`.
// Elements are addressed by stable selector preference
// (data-testid > id > name > aria-role + structural index); a tappable lacking
// any stable id falls back to role+index and is flagged `nokey`. The host then
// hashes the tree with the canonical signature, byte-identical to the oracle.
const snapshotJs = (valueNodeSelectors) => `
  const maxLen = ${MAX_LABEL_LEN};
  const selList = ${JSON.stringify(valueNodeSelectors || [])};
  const labels = [];
  const rawTaps = [];
  const textNodes = [];

  const ROLES = {
    screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
    icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
    slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
  };
  const TRANSIENT_ROLES = { toast: 1, snackbar: 1, spinner: 1, progress: 1, tooltip: 1, badge: 1 };

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

  const typeOf = (el, role) => {
    if (role !== 'textfield') return null;
    if (el.tagName.toLowerCase() !== 'input') return null;
    const t = (el.getAttribute('type') || 'text').toLowerCase();
    const allowed = { text: 1, password: 1, email: 1, number: 1, search: 1 };
    return allowed[t] ? t : 'text';
  };

  const iconOf = (el) => {
    const di = el.getAttribute('data-icon') || el.getAttribute('data-icon-name');
    if (di && di.trim()) return di.trim();
    const use = el.querySelector ? el.querySelector('use[href], use[xlink\\\\:href]') : null;
    if (use) {
      const href = use.getAttribute('href') || use.getAttribute('xlink:href');
      if (href && href.trim()) return href.trim().replace(/^#/, '');
    }
    return null;
  };

  const idOf = (el) => {
    const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
    if (testid && testid.trim()) return testid.trim();
    const id = el.getAttribute('id');
    if (id && id.trim()) return id.trim();
    const name = el.getAttribute('name');
    if (name && name.trim()) return name.trim();
    return null;
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

  const isTransientEl = (el) => {
    const ariaRole = (el.getAttribute('role') || '').toLowerCase();
    if (TRANSIENT_ROLES[ariaRole]) return true;
    if (ariaRole === 'alert' || ariaRole === 'status') return true;
    const live = (el.getAttribute('aria-live') || '').toLowerCase();
    if (live === 'assertive' || live === 'polite') return true;
    const cls = (el.getAttribute('class') || '').toLowerCase();
    if (/\\b(toast|snackbar|spinner|progress|loader|loading|tooltip|badge)\\b/.test(cls)) return true;
    if (el.hasAttribute('data-transient')) return true;
    return false;
  };

  // RAW value-role (docs/signature.md "Value-state"): the value-role name for a
  // value-bearing DOM element, NEVER from text. role=status/log/progressbar/
  // meter/timer pass through; <output>/role=output -> output; an aria-live
  // region (polite/assertive) -> status; text form fields -> textfield. null for
  // chrome / non-text inputs (password is never read). Identical to web runner.
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
    return (el.innerText || el.textContent || '').trim().split('\\n')[0].trim();
  };
  const accessibleName = (el) => {
    const aria = el.getAttribute('aria-label');
    if (aria && aria.trim()) return true;
    const title = el.getAttribute('title');
    if (title && title.trim()) return true;
    const alt = el.getAttribute('alt');
    if (alt && alt.trim()) return true;
    return (el.innerText || el.textContent || '').trim().length > 0;
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

  const buildNode = (el, isRoot) => {
    const role = isRoot ? 'screen' : roleOf(el);
    // Value-state (Layer 2): a value-role element (by tag/aria), an aria-live
    // region, or a Layer-3 opt-in node is value-bearing. Value-bearing WINS over
    // the transient heuristic, so a role=status / aria-live counter that the
    // transient heuristic would otherwise drop is kept as a value node and its
    // keypresses produce DISTINCT value-states.
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
      // The flag makes the canonical is_value_bearing accept the node even when
      // roleOf normalized its raw value-role (status/output/...) to node.
      node.value_node = true;
      // Layer-1 content fingerprint: a value node's stable key + its raw value.
      const fkey = id != null ? 'key:' + id : 'vrole:' + (vrole || 'opt');
      textNodes.push([fkey, node.value]);
    }
    if (transient) { node.transient = true; node.children = []; return node; }

    // Layer-1 content fingerprint over text-bearing nodes (runner-local, NOT
    // canonical): any keyed element's own (non-child) trimmed text contributes
    // (stable-key, text). Catches a display whose textContent changes without
    // any structural move; the raw text never enters the canonical key.
    if (!isRoot && id != null && !valueBearing) {
      let own = '';
      for (const c of el.childNodes) { if (c.nodeType === 3) own += c.textContent; }
      own = own.trim();
      if (own) textNodes.push(['text:' + id, own]);
    }

    if (!isRoot) {
      const name = nameOf(el);
      if (name) labels.push(clipLabel(name));
      if (interactive(el, role)) {
        rawTaps.push({ role, key: keyOf(el), label: name ? clipLabel(name) : '', unlabeled: !accessibleName(el) });
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

  const perRole = {};
  let unlabeled = 0;
  const tappables = rawTaps.map((tn) => {
    const idx = perRole[tn.role] || 0;
    perRole[tn.role] = idx + 1;
    if (tn.unlabeled) unlabeled++;
    const sel = tn.key ? 'key:' + tn.key : 'role:' + tn.role + '#' + idx;
    return { sel, role: tn.role, index: idx, key: tn.key, label: tn.label };
  });

  let anchor = null;
  try { if (location && location.pathname) anchor = location.pathname; } catch (e) {}

  // Layer-1 content fingerprint source: sorted (stable-key, trimmed text) over
  // value + keyed-text nodes. Sorted here so it is order-independent.
  textNodes.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : (a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0)));

  return { tree, anchor, labels: [...new Set(labels)], tappables, unlabeled, textNodes };
`;

async function snapshot(browser, valueNodeSelectors) {
  const snap = await browser.execute(snapshotJs(valueNodeSelectors || []));
  // Hash the canonical Node tree host-side, exactly like the Rust oracle and the
  // golden vectors. Text never contributes.
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
  // the structural sig OR this fingerprint changed. Never folded into the key.
  snap.content = snap.sig + '|' + snap.textNodes.map((p) => p[0] + '=' + p[1]).join(';');
  return snap;
}

// PARITY: keep in sync with runners/web/runner.mjs (operability + flicker oracle)
// ====================================================================
//  OPERABILITY / ACCESSIBILITY GROUND TRUTH (the EXPLORE:GROUNDTRUTH marker)
//  Mirrors runners/web/runner.mjs, but Tauri's webview has NO CDP, so GRAPH 1
//  (operableByPointer) uses native + cursor:pointer + delegation-marker signals
//  only (plus an inline onclick / a document.onclick handler we can read from
//  JS), never a captured event-listener list. GRAPH 2 (a11y dims) runs entirely
//  in-page: inTabOrder is the standard sequential-focus rule (focusable AND
//  tabIndex >= 0 -> a negative tabindex is reachable by script/pointer but NOT
//  by Tab), and keyboardActivatable is derived structurally (native semantics +
//  inline key handlers), never by synthesizing a keypress, which would fire the
//  app's handlers as a side effect. A missing dimension defaults to true (= no gap) in the
//  engine, so we only report what we measured. The whole probe is one execute().
//  Keyed by the SAME selector the EXPLORE:STATE elements use.
// ====================================================================
const GROUNDTRUTH_JS = `
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
  const keyOf = (el) => {
    const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
    if (testid && testid.trim()) return 'testid:' + testid.trim();
    const id = el.getAttribute('id');
    if (id && id.trim()) return 'id:' + id.trim();
    const name = el.getAttribute('name');
    if (name && name.trim()) return 'name:' + name.trim();
    return null;
  };
  const nativeInteractive = (el) => {
    const tag = el.tagName.toLowerCase();
    if (['a', 'button', 'select', 'textarea', 'summary'].includes(tag)) return true;
    if (tag === 'input') { const t = (el.getAttribute('type') || 'text').toLowerCase(); return t !== 'hidden'; }
    if (el.isContentEditable) return true;
    return false;
  };
  // Roles that name a region or a piece of document structure, NOT an operable
  // widget. A landmark (search/navigation/banner/...) or a structural/live role
  // is something a pointer user reads, not something they "operate", so it must
  // not count as a delegation marker, else it is promoted to operable by a
  // page-wide document click handler and surfaces as a phantom gap.
  const NON_INTERACTIVE_ROLES = new Set([
    'banner', 'complementary', 'contentinfo', 'form', 'main', 'navigation',
    'region', 'search',
    'article', 'definition', 'directory', 'document', 'feed', 'figure', 'group',
    'heading', 'img', 'list', 'listitem', 'math', 'none', 'note', 'presentation',
    'separator', 'table', 'term', 'toolbar', 'tooltip', 'caption', 'rowgroup',
    'row', 'cell', 'columnheader', 'rowheader',
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
  // roving "active" item). Such items are keyboard-reachable AND activatable even
  // with tabindex=-1, because the container handles the keys.
  const adManaged = (el) => {
    const isFocusable = (c) => {
      const ti = c.getAttribute('tabindex');
      return (ti !== null && parseInt(ti, 10) >= 0) || nativeInteractive(c);
    };
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
  // reachable: on-screen AND hit-testable, so a real pointer user can operate it.
  // The operable gate below uses this so an off-screen/occluded control is not a
  // phantom pointer-only/keyboard gap.
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
  const rolePresent = (el) => {
    const tag = el.tagName.toLowerCase();
    if (['a', 'button', 'select', 'textarea', 'input', 'summary'].includes(tag)) return true;
    if (/^h[1-6]$/.test(tag)) return true;
    const ar = (el.getAttribute('role') || '').trim().toLowerCase();
    if (!ar) return false;
    return !['none', 'presentation', 'generic'].includes(ar);
  };
  const namePresent = (el) => {
    const aria = el.getAttribute('aria-label'); if (aria && aria.trim()) return true;
    const lb = el.getAttribute('aria-labelledby'); if (lb && lb.trim()) return true;
    const title = el.getAttribute('title'); if (title && title.trim()) return true;
    const alt = el.getAttribute('alt'); if (alt && alt.trim()) return true;
    const ph = el.getAttribute('placeholder'); if (ph && ph.trim()) return true;
    const text = (el.innerText || el.textContent || '').trim();
    return text.length > 0;
  };
  const gestureKindOf = (el, role, native, deleg) => {
    if (role === 'textfield') return 'field';
    if (native) return 'button';
    if (deleg) return 'delegated';
    return 'tap';
  };
  // No CDP: approximate the document-level delegated-click pattern by reading
  // an inline document.onclick / body.onclick handler (the only listener kind
  // visible to script). Real addEventListener handlers are invisible here, so
  // Tauri's delegated detection is best-effort and conservative.
  const docDelegates = !!(document.onclick || (document.body && document.body.onclick));

  const out = [];
  const perRole = {};
  const root = document.body || document.documentElement;
  const walk = (el, isRoot) => {
    if (!isRoot && !visible(el)) { for (const c of el.children) walk(c, false); return; }
    if (!isRoot) {
      const role = roleOf(el);
      const inWalk = interactive(el, role);
      const native = nativeInteractive(el);
      const parentCursor = el.parentElement ? getComputedStyle(el.parentElement).cursor : '';
      const cursor = getComputedStyle(el).cursor === 'pointer' && parentCursor !== 'pointer';
      const deleg = hasDelegationMarker(el);
      const ownInline = !!el.onclick || el.hasAttribute('onclick');
      const candidate = inWalk || native || cursor || deleg || ownInline;
      let sel;
      if (inWalk) {
        const idx = perRole[role] || 0; perRole[role] = idx + 1;
        const key = keyOf(el); sel = key ? 'key:' + key : 'role:' + role + '#' + idx;
      } else if (candidate) {
        const key = keyOf(el); sel = key ? 'key:' + key : 'role:' + role + '#gt' + out.length;
      }
      if (candidate) {
        // operable is graph 1: an element a pointer can ACTUALLY operate now. An
        // off-screen/occluded control is not pointer-operable, so it cannot be a
        // pointer-only/keyboard gap either; gate on reachability to align the two
        // graphs (matches the web runner).
        const operable = reachable(el) && (native || cursor || ownInline || (docDelegates && deleg));
        // inTabOrder: sequential-focus reachability. An element is in the Tab
        // sequence iff it is focusable AND its tabIndex is >= 0. A tabindex=-1
        // element is script/pointer focusable but NOT reachable by Tab (the
        // motivating <div role=option tabindex=-1> case). An aria-activedescendant
        // item is reachable + activatable via its focusable composite container.
        const adm = adManaged(el);
        const focusable = native || el.tabIndex >= 0 || (el.hasAttribute('tabindex') && el.tabIndex >= 0) || adm;
        const inTabOrder = (el.tabIndex >= 0 && focusable) || adm;
        const a11y = {
          rolePresent: rolePresent(el),
          namePresent: namePresent(el),
          inTabOrder: inTabOrder,
          focusable: focusable,
        };
        if (operable) {
          if (!inTabOrder && !native) {
            a11y.keyboardActivatable = false;
          } else {
            // keyboardActivatable, derived WITHOUT firing the control. We must
            // NOT synthesize Enter/Space (even via dispatchEvent): a bubbling
            // keydown fires the app's real handler (a navigation, or a crash) as
            // a side effect, polluting the crash oracle. A Tauri webview has no
            // CDP, so we cannot enumerate addEventListener key handlers; the most
            // we can read cheaply is the native semantics and inline on* handlers.
            // A native control, or one with an inline key handler, is keyboard-
            // activatable. Otherwise, since the element is focusable and in the
            // Tab order, we assume activatable rather than flag a gap we cannot
            // prove (matches the web runner's no-CDP fallback; it means Tauri
            // under-reports the click-only-div case the CDP path catches).
            const inlineKey = !!(el.onkeydown || el.onkeypress || el.onkeyup);
            a11y.keyboardActivatable = native || inlineKey || focusable;
          }
        }
        out.push({ id: sel, operable: operable, gestureKind: gestureKindOf(el, role, native, deleg), a11y });
      }
    }
    for (const c of el.children) walk(c, false);
  };
  if (root) walk(root, true);
  // Focus trap detection needs a real Tab traversal, which the webview can't do
  // from script; report false (a missing/false focusTrap is the safe default).
  return { elements: out, focusTrap: false };
`;

// Emit the EXPLORE:GROUNDTRUTH record for the current state (Tauri). `sig` is the
// SAME signature the EXPLORE:STATE for this state carried. Best-effort: a failed
// probe simply emits nothing.
async function emitGroundtruth(browser, sig) {
  let res;
  try { res = await browser.execute(GROUNDTRUTH_JS); } catch (e) { return; }
  if (!res) return;
  log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap: !!res.focusTrap, elements: res.elements || [] }));
}

// Tier-1 flicker oracle (persistent-anchor churn), mirroring runners/web. Tag the
// persistent chrome before a transition; after it settles, flag any anchor that
// is VISUALLY UNCHANGED (same key, text, box) yet was REPLACED (DOM node identity
// changed) -> an innerHTML-wipe-and-rebuild that flickers, which the settled-frame
// oracle cannot see. Run as execute() source strings (the webview has no CDP);
// window persists between execute() calls in the same document, so the marks
// survive from before-action to after-settle. Pure DOM, no frame timing.
const ANCHOR_SEL_JS = JSON.stringify(
  'header,nav,main,footer,aside,' +
  '[role=banner],[role=navigation],[role=main],[role=contentinfo],' +
  '[role=complementary],[role=region],[role=search],[role=listbox],' +
  '[role=list],[role=tablist],[role=toolbar],[role=dialog],[id]'
);
const MARK_ANCHORS_JS = `
  const sel = ${ANCHOR_SEL_JS};
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
      text: (el.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 256),
      x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height),
    });
  }
  window.__reproitAnchors = anchors;
  window.__reproitAnchorDoc = document;
  return anchors.length;
`;
const CHURNED_ANCHORS_JS = `
  const sel = ${ANCHOR_SEL_JS};
  const old = window.__reproitAnchors;
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
    if (dup.has(a.key)) continue;
    const now = cur.get(a.key);
    if (!now) continue;
    if (now === a.node) continue;
    const r = now.getBoundingClientRect();
    const sameBox =
      Math.round(r.x) === a.x && Math.round(r.y) === a.y &&
      Math.round(r.width) === a.w && Math.round(r.height) === a.h;
    const sameText = (now.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 256) === a.text;
    if (sameBox && sameText) churned.push(a.key);
  }
  window.__reproitAnchors = null;
  return churned;
`;

// PARITY: keep in sync with runners/web/runner.mjs (overflow oracle).
//
// DOM/layout OVERFLOW oracle (deterministic, structural). The i18n / long-string
// / RTL failure class: a long label overflowing a fixed-width button, a child
// wider than its parent's content box, or text clipped by text-overflow. Caught
// from STRUCTURAL MEASUREMENTS (scrollWidth/clientWidth, child-vs-parent content
// box, offsetWidth<scrollWidth), never a pixel diff, so the same DOM yields the
// same finding byte-for-byte on every run and on replay. Tauri's webview is a
// real DOM, so the measurement is identical to the web runner; only the transport
// (browser.execute over WebDriver) differs. Returns the sorted item list (or []).
const OVERFLOW_TOL = 2;
const DETECT_OVERFLOW_JS = `
  const tol = ${OVERFLOW_TOL};
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
  const out = [];
  const seen = new Set();
  const add = (el, kind, by) => {
    const k = keyOf(el) + '|' + kind;
    if (seen.has(k)) return;
    seen.add(k);
    out.push({ key: keyOf(el), kind, by: Math.round(by) });
  };
  const doc = document.documentElement;
  if (doc && doc.scrollWidth - doc.clientWidth > tol) {
    out.push({ key: 'tag:html', kind: 'scroll', by: Math.round(doc.scrollWidth - doc.clientWidth) });
  }
  const all = document.body ? document.body.querySelectorAll('*') : [];
  for (const el of all) {
    if (!visible(el)) continue;
    const st = getComputedStyle(el);
    if (el.scrollWidth - el.clientWidth > tol) add(el, 'scroll', el.scrollWidth - el.clientWidth);
    const clips = st.overflow === 'hidden' || st.overflowX === 'hidden' || st.textOverflow === 'ellipsis';
    const oneLine = st.whiteSpace === 'nowrap' || st.textOverflow === 'ellipsis';
    if (clips && oneLine && el.scrollWidth - el.offsetWidth > tol) {
      add(el, 'clip', el.scrollWidth - el.offsetWidth);
    }
    const p = el.parentElement;
    if (p && p !== document.body && p !== doc) {
      const ps = getComputedStyle(p);
      const scrollsX = ps.overflowX === 'auto' || ps.overflowX === 'scroll' || ps.overflow === 'auto' || ps.overflow === 'scroll';
      if (!scrollsX) {
        const pr = p.getBoundingClientRect();
        const cr = el.getBoundingClientRect();
        const padL = parseFloat(ps.paddingLeft) || 0;
        const padR = parseFloat(ps.paddingRight) || 0;
        const bL = parseFloat(ps.borderLeftWidth) || 0;
        const bR = parseFloat(ps.borderRightWidth) || 0;
        const contentLeft = pr.left + bL + padL;
        const contentRight = pr.right - bR - padR;
        const over = Math.max(cr.right - contentRight, contentLeft - cr.left);
        if (over > tol) add(el, 'spill', over);
      }
    }
  }
  out.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : (a.kind < b.kind ? -1 : a.kind > b.kind ? 1 : 0)));
  return out;
`;

// PARITY: keep in sync with runners/web/runner.mjs (content-bug oracle).
//
// CONTENT-BUG oracle (deterministic, DOM/label-based). The literal artifacts a
// stringify/template bug leaks to the screen: [object Object], whole-word
// undefined/null/NaN, an unrendered {{...}}/${...} placeholder. Scans only the
// OWN text of keyed, visible elements so the finding is addressed by a stable,
// locale-invariant key (never the text). Pure substring/structure test, no pixel
// or timing read, so the same DOM yields the same finding on every run/replay.
// Identical to the web runner; runs in-webview via browser.execute.
const DETECT_CONTENTBUG_JS = `
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
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
  const ownText = (el) => {
    let t = '';
    for (const c of el.childNodes) if (c.nodeType === 3) t += c.textContent;
    return t.replace(/\\s+/g, ' ').trim();
  };
  const reasonOf = (text) => {
    if (!text) return null;
    if (text.includes('[object Object]')) return 'object-object';
    if (/\\{\\{[^}]*\\}\\}/.test(text) || /\\$\\{[^}]*\\}/.test(text)) return 'unrendered-template';
    if (/(^|[\\s:>(\\[,])undefined($|[\\s.,!?)\\]<])/.test(text)) return 'undefined';
    if (/(^|[\\s:>(\\[,])null($|[\\s.,!?)\\]<])/.test(text)) return 'null';
    if (/(^|[\\s:>(\\[,])NaN($|[\\s.,!?)\\]<])/.test(text)) return 'nan';
    return null;
  };
  const out = [];
  const seen = new Set();
  const all = document.body ? document.body.querySelectorAll('*') : [];
  for (const el of all) {
    if (!visible(el)) continue;
    const key = keyOf(el);
    if (!key) continue;
    const text = ownText(el);
    const reason = reasonOf(text);
    if (!reason) continue;
    const dedup = key + '|' + reason;
    if (seen.has(dedup)) continue;
    seen.add(dedup);
    out.push({ key, reason, text: text.slice(0, 80) });
  }
  out.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : (a.reason < b.reason ? -1 : a.reason > b.reason ? 1 : 0)));
  return out;
`;

// PARITY: keep in sync with runners/web/runner.mjs (jank/hang watchdog).
//
// JANK / HANG watchdog (deterministic, recorded-trace based). Two paths, both
// installed inside the webview via execute() (idempotent) and re-installed each
// observe() since a navigation replaces the window:
//
//   1. Long Tasks (CHROMIUM / WebView2 only). We key off the webview's own Long
//      Tasks trace, never a wall-clock duration sample: a `longtask`
//      PerformanceObserver entry is emitted for any task that blocks the main
//      thread > 50ms, buffered and delivered after the blocking task finishes.
//      We classify by the MAX blocked duration into coarse, well-separated
//      floors (>=2000ms hang, >=200ms jank) so timing jitter can never flip the
//      verdict. The Long Tasks API exists ONLY in Chromium/WebView2; on Tauri's
//      WebKit webview (WKWebView on macOS, WebKitGTK on Linux) it is ABSENT, so
//      this path records nothing there.
//
//   2. requestAnimationFrame frame-drop detector (CROSS-ENGINE). rAF fires once
//      per would-be paint in EVERY engine, so the interval between two callbacks
//      is how long the main thread blocked between two frames. This is the path
//      that closes the silence on Tauri's WebKit webview, where Long Tasks is
//      unavailable. The classifier (classifyFrameIntervals) and its floors are
//      COPIED VERBATIM from runners/web/runner.mjs, where they are FP-validated
//      on real firefox/webkit (clean + animated sites stay silent). Emits the
//      SAME EXPLORE:JANK / EXPLORE:HANG markers with the SAME reused
//      JANK_FLOOR_MS / HANG_FLOOR_MS buckets, so the marker is byte-identical
//      across paths and to the web runner.
//
// drainJankForEngine() runs the Long Tasks path when it produced entries (the
// precise Chromium/WebView2 signal) and otherwise falls back to the rAF path,
// so a WebView2 verdict is unchanged while WebKit gets the cross-engine signal.
// A webview where NEITHER path sees a stall stays SILENT, NEVER a false positive
// (same honesty as the web runner's firefox/webkit fallback).
const JANK_FLOOR_MS = 200;
const HANG_FLOOR_MS = 2000;
const INSTALL_LONGTASK_JS = `
  try {
    if (!window.__reproitLongTaskHooked) {
      window.__reproitLongTaskHooked = true;
      window.__reproitLongTasks = [];
      const obs = new PerformanceObserver((list) => {
        for (const e of list.getEntries()) window.__reproitLongTasks.push(Math.round(e.duration));
      });
      obs.observe({ entryTypes: ['longtask'] });
    }
  } catch (_) { /* no Long Tasks API: jank/hang silent on this webview */ }
  return true;
`;
const RESET_LONGTASK_JS = `try { window.__reproitLongTasks = []; } catch (_) {} return true;`;
const DRAIN_LONGTASK_JS = `
  const t = window.__reproitLongTasks || [];
  window.__reproitLongTasks = [];
  return t;
`;
async function installLongTaskObserver(browser) {
  try { await browser.execute(INSTALL_LONGTASK_JS); } catch { /* webview not ready */ }
}
async function drainJank(browser) {
  let tasks = [];
  try { tasks = await browser.execute(DRAIN_LONGTASK_JS); } catch { return null; }
  if (!tasks || !tasks.length) return null;
  const max = Math.max(...tasks);
  if (max >= HANG_FLOOR_MS) return { kind: 'hang', bucket: HANG_FLOOR_MS, count: tasks.length };
  if (max >= JANK_FLOOR_MS) return { kind: 'jank', bucket: JANK_FLOOR_MS, count: tasks.length };
  return null;
}

// CROSS-ENGINE jank/hang path (requestAnimationFrame frame-drop detector). COPIED
// VERBATIM from runners/web/runner.mjs (installFrameObserver / drainFrameJank /
// classifyFrameIntervals + the RAF_* constants). The Long Tasks path above is
// CHROMIUM/WebView2-ONLY; on Tauri's WebKit webview it records nothing. rAF works
// in every engine: the browser fires the callback once per would-be paint, so the
// interval between two callbacks is how long the main thread blocked between two
// frames. The classifier is deliberately conservative to stay FALSE-POSITIVE-FREE:
//   - HANG: a single interval >= HANG_FLOOR_MS (2000ms). Nothing benign blocks
//     paint for two whole seconds.
//   - JANK: EITHER a LONE long frame >= RAF_JANK_LONE_MS (a stall far above any
//     GC/scheduling blip), OR a SUSTAINED RUN of >= RAF_JANK_RUN_MIN consecutive
//     long (>= RAF_FRAME_MS) frames whose summed blocked time reaches
//     JANK_FLOOR_MS. A single GC pause is one sub-lone-floor frame -> dropped.
// The EMITTED bucket is the SAME reused JANK_FLOOR_MS / HANG_FLOOR_MS constant the
// Long Tasks path uses, so the marker is byte-identical across paths and to the
// web runner. `count` is the number of distinct stall EVENTS (runs), not raw
// frames. The floors are FP-validated on real firefox/webkit; do not retune them.
const RAF_FRAME_MS = 100;       // an inter-frame interval this long is a "long frame"
const RAF_JANK_RUN_MIN = 2;     // a sustained jank run needs >= this many long frames
const RAF_JANK_LONE_MS = 350;   // a single frame this long is jank on its own (> GC noise, < the 600ms fixture)

// Pure classifier over a list of inter-frame intervals (ms). Deterministic: the
// SAME interval list always yields the same verdict. Byte-identical to the web
// runner's classifyFrameIntervals. Returns { kind, bucket, count } or null.
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

// Install the rAF frame-interval recorder inside the webview, alongside the
// longtask observer. A self-perpetuating requestAnimationFrame loop appends each
// inter-frame interval to a window-global the per-action probe drains. Idempotent
// (a navigation drops it; observe() re-installs). Cross-engine (rAF is universal),
// cheap (one timestamp per frame), side-effect-free. The buffer is capped so a
// long idle stretch cannot grow it unbounded.
const INSTALL_FRAME_JS = `
  try {
    if (!window.__reproitFrameHooked) {
      window.__reproitFrameHooked = true;
      window.__reproitFrameIntervals = [];
      let last = -1;
      const tick = (now) => {
        if (last >= 0) {
          const d = now - last;
          const buf = window.__reproitFrameIntervals;
          if (buf.length < 4096) buf.push(Math.round(d));
        }
        last = now;
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    }
  } catch (_) { /* no rAF: cross-engine jank/hang silent (never a false positive) */ }
  return true;
`;
const RESET_FRAME_JS = `try { window.__reproitFrameIntervals = []; } catch (_) {} return true;`;
const DRAIN_FRAME_JS = `
  const t = window.__reproitFrameIntervals || [];
  window.__reproitFrameIntervals = [];
  return t;
`;
async function installFrameObserver(browser) {
  try { await browser.execute(INSTALL_FRAME_JS); } catch { /* webview not ready */ }
}
// Drain the rAF interval buffer and classify it. Returns the SAME shape as
// drainJank ({ kind, bucket, count }) or null. The cross-engine path.
async function drainFrameJank(browser) {
  let intervals = [];
  try { intervals = await browser.execute(DRAIN_FRAME_JS); } catch { return null; }
  return classifyFrameIntervals(intervals);
}
// Per-action jank/hang verdict, engine-agnostic. Tauri cannot tell us which
// engine backs the webview from JS, so we run the PRECISE Long Tasks path first;
// when it produced a verdict (Chromium/WebView2), we keep it unchanged. When it
// is silent (no Long Tasks API, i.e. WebKit, OR a genuinely clean Chromium
// action), we fall back to the rAF path, which is the cross-engine signal that
// closes the WebKit silence. A clean action returns null on both -> no marker.
async function drainJankForEngine(browser) {
  const lt = await drainJank(browser);
  if (lt) return lt;
  return drainFrameJank(browser);
}

export { classifyFrameIntervals };

// LEAK sampler (deterministic). `--soak` replays a reversible cycle N times and
// reads the heap slope. The web/Electron runners read the PRECISE, unrounded v8
// used-heap via CDP `Runtime.getHeapUsage` + a forced GC. Tauri is driven over
// WebDriver, which has NO CDP, so that precise path is unreachable here.
//
// PRIMARY (real, coarse, session-level): the Tauri webview is a HOST PROCESS, so we
// sample its resident set size (RSS) with a host process tool. The app's main
// process is the one whose executable IS the built binary ($REPROIT_APP); helper
// processes (WebKitWebProcess / msedgewebview2 / *Helper) have a different argv[0]
// and never match, so the read is the MAIN process's footprint, not a helper's.
// RSS is whole-process memory (native + webview heaps), so it is COARSER than the
// JS heap and attributed to the SOAK RUN, not a transition; but it is REAL and
// DETERMINISTIC: a true leak grows RSS monotonically with cycle count, and the soak
// floor (262KB/cycle) is far above sampling noise. Gated HARD: we use it only when
// the app path resolves to EXACTLY ONE host pid; any ambiguity (zero or several
// matches) -> we do NOT guess and fall through to the JS fallback below.
//
// FALLBACK (when the pid can't be cleanly resolved): `performance.memory.
// usedJSHeapSize`, the same fallback the web runner uses on firefox/webkit. That
// value is QUANTIZED by Chromium (WebView2) to a coarse bucket and ABSENT entirely
// in WebKit (WKWebView / WebKitGTK), so the slope may be too coarse to see a small
// leak, or no sample is emitted at all on WebKit ('~'). We emit MEMORY:SAMPLE when
// a number is available and stay silent otherwise; soak reads whatever it gets.
const PERF_MEMORY_JS = `
  try {
    if (performance.memory && typeof performance.memory.usedJSHeapSize === 'number') {
      return performance.memory.usedJSHeapSize;
    }
  } catch (_) {}
  return null;
`;

// Run a host process tool and return trimmed stdout, or null. Pure read; never
// throws (a missing binary / non-zero exit / spawn error yields null, so the
// sampler degrades to the JS fallback).
function hostExec(cmd, args) {
  try {
    const out = execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'], timeout: 5000 });
    return out == null ? null : String(out);
  } catch { return null; }
}

// Resolve the Tauri app's MAIN host pid from its binary path ($REPROIT_APP), or
// null. Cross-platform: macOS/Linux read `ps -axww -o pid=,comm=` and keep rows
// whose command IS the app path; Windows queries `tasklist` by image name. We
// require EXACTLY ONE matching pid (the main process); zero or several -> null, so
// a helper-process race or a second instance never yields a wrong-process read.
function resolveTauriPid(appPath) {
  if (!appPath) return null;
  const isWin = osPlatform() === 'win32';
  if (isWin) {
    // tasklist filters by image name; argv[0] path isn't exposed, so match the
    // executable's base name and require a single PID row.
    const base = appPath.split(/[\\/]/).pop() || appPath;
    const out = hostExec('tasklist', ['/FI', 'IMAGENAME eq ' + base, '/FO', 'CSV', '/NH']);
    if (out == null) return null;
    const pids = [];
    for (const line of out.split(/\r?\n/)) {
      // CSV: "name","pid","session","sess#","mem". Take the 2nd quoted field.
      const m = line.match(/^"[^"]*","(\d+)"/);
      if (m) pids.push(parseInt(m[1], 10));
    }
    if (pids.length !== 1 || !Number.isFinite(pids[0]) || pids[0] <= 0) return null;
    return pids[0];
  }
  const out = hostExec('ps', ['-axww', '-o', 'pid=,comm=']);
  if (out == null) return null;
  const pids = [];
  for (const line of out.split('\n')) {
    const m = line.match(/^\s*(\d+)\s+(.*)$/);
    if (!m) continue;
    if (m[2].trim() === appPath) pids.push(parseInt(m[1], 10));
  }
  if (pids.length !== 1 || !Number.isFinite(pids[0]) || pids[0] <= 0) return null;
  return pids[0];
}

// Read a host pid's RSS as BYTES, or null. macOS/Linux: `ps -o rss=` (KB).
// Windows: `tasklist` reports "N,NNN K" memory; parse the digits as KB.
function hostRssBytes(pid) {
  if (!(pid > 0)) return null;
  if (osPlatform() === 'win32') {
    const out = hostExec('tasklist', ['/FI', 'PID eq ' + pid, '/FO', 'CSV', '/NH']);
    if (out == null) return null;
    const m = out.match(/"([\d.,]+)\s*K"/);
    if (!m) return null;
    const kb = parseInt(m[1].replace(/[.,]/g, ''), 10);
    if (!Number.isFinite(kb) || kb <= 0) return null;
    return kb * 1024;
  }
  const out = hostExec('ps', ['-o', 'rss=', '-p', String(pid)]);
  if (out == null) return null;
  const kb = parseInt(out.trim(), 10);
  if (!Number.isFinite(kb) || kb <= 0) return null;
  return kb * 1024;
}

// Sample the leak signal and emit MEMORY:SAMPLE (heap_used in BYTES). PRIMARY:
// the main webview process RSS (real, coarse, session-level), used when the pid
// resolves uniquely. FALLBACK: performance.memory.usedJSHeapSize over WebDriver.
// `pidRef` is a one-shot cache ({ pid, tried }) so the host pid is resolved once.
async function sampleHeap(browser, tMs, pidRef) {
  // PRIMARY: process RSS, gated on a uniquely resolved main-process pid.
  if (pidRef) {
    if (!pidRef.tried) { pidRef.tried = true; pidRef.pid = resolveTauriPid(APP); }
    if (pidRef.pid > 0) {
      const rss = hostRssBytes(pidRef.pid);
      if (rss != null) { log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: rss })); return; }
    }
  }
  // FALLBACK: quantized JS heap (Chromium/WebView2) or silence (WebKit '~').
  let used = null;
  try { used = await browser.execute(PERF_MEMORY_JS); } catch (_) { used = null; }
  if (used == null) return;
  log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: used }));
}

// Exception oracle for the webview. tap() clicks an element via execute(); a
// throw inside that element's event LISTENER does not propagate back through
// click()'s return value, it surfaces as an uncaught error on the webview
// window. el.click() returning true therefore says nothing about whether the
// listener threw. So we install window-level error hooks (matching the
// Playwright web runner's page.on('pageerror')) that buffer every uncaught
// error and unhandled rejection, then drain that buffer after each action.
//
// Hooks must be re-installed after navigations (each document gets a fresh
// window), so installHooks() is idempotent and called on every observe().
const INSTALL_HOOKS_JS = `
  if (!window.__reproit_hooked) {
    window.__reproit_hooked = true;
    window.__reproit_errors = [];
    window.addEventListener('error', (ev) => {
      try {
        const e = ev.error;
        window.__reproit_errors.push({
          message: (e && e.message) || ev.message || String(e || ev),
          source: ev.filename || '',
          line: ev.lineno || 0,
          stack: (e && e.stack) ? String(e.stack) : '',
        });
      } catch (_) { /* never let the hook itself throw */ }
    });
    window.addEventListener('unhandledrejection', (ev) => {
      try {
        const r = ev.reason;
        window.__reproit_errors.push({
          message: (r && r.message) ? r.message : ('Unhandled rejection: ' + String(r)),
          source: '',
          line: 0,
          stack: (r && r.stack) ? String(r.stack) : '',
        });
      } catch (_) { /* never let the hook itself throw */ }
    });
    // We intentionally do NOT also set window.onerror: in WebKitGTK both the
    // 'error' event listener above and window.onerror fire for the same
    // uncaught error, which would emit the block twice. The 'error' event is
    // the reliable single source (same as the web runner's page.on('pageerror')).
  }
  return true;
`;

async function installHooks(browser) {
  try { await browser.execute(INSTALL_HOOKS_JS); } catch { /* webview not ready yet */ }
}

// Emit the SAME exception block the web/Electron runners emit and the Rust
// oracle parses (drive.rs / fuzz.rs look for "EXCEPTION CAUGHT BY", read until
// a line of pure ═, and pull the message from after "The following ...").
function emitError(err) {
  log('EXCEPTION CAUGHT BY TAURI WEBVIEW');
  log('The following error was thrown:');
  log(String(err && err.message ? err.message : err));
  const stack = (err && err.stack) ? String(err.stack) : '';
  for (const line of stack.split('\n').slice(0, 8)) { if (line) log(line); }
  log('════════');
}

// Pull every buffered error out of the webview and emit one block each.
async function drainErrors(browser) {
  let errs = [];
  try {
    errs = await browser.execute(() => {
      const e = window.__reproit_errors || [];
      window.__reproit_errors = [];
      return e;
    });
  } catch { return; }
  if (Array.isArray(errs)) { for (const e of errs) emitError(e); }
}

// STRUCTURAL tap: resolve a locale-invariant selector and click it inside the
// webview. Returns true on success. Mirrors runners/web/runner.mjs's tap(). No
// visible text is ever used to locate the element.
//   key:testid:<v> -> [data-testid="v"] (or data-test-id)
//   key:id:<v>     -> #<v>
//   key:name:<v>   -> [name="v"]
//   role:<role>#<idx> -> the idx-th visible tappable of that role, document order
const TAP_JS = `
  const s = arguments[0];
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const cssEscape = (v) => (window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\\\]/g, '\\\\$&'));

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
`;

async function tap(browser, sel) {
  try {
    const ok = await browser.execute(TAP_JS, sel);
    return !!ok;
  } catch { return false; }
}

async function main() {
  if (!APP) { log('EXCEPTION CAUGHT BY REPROIT'); log('REPROIT_APP (executable path) required'); log('═'.repeat(8)); process.exit(0); }
  const fuzz = loadFuzz();
  const { remote } = await import('webdriverio');
  const url = new URL(WD_URL);
  const browser = await remote({
    hostname: url.hostname,
    port: Number(url.port || 4444),
    path: url.pathname || '/',
    // No browserName: tauri-driver forwards it verbatim to the native driver
    // (WebKitWebDriver on Linux), which rejects unknown values like 'wry' with
    // "Failed to match capabilities". The official Tauri v2 WebDriver example
    // sends only tauri:options. tauri-driver reads tauri:options from
    // alwaysMatch (where wdio places a single plain capabilities object).
    capabilities: { 'tauri:options': { application: APP } },
  });

  log('JOURNEY claimed role=a');
  await browser.pause(1500);
  // Raise the async-script timeout so the choice-anomaly pass (which waits for
  // layout to settle between each option of a multi-choice component) is not cut
  // off mid-exercise. A picker with many options at ~600ms each can run several
  // seconds; 30s leaves comfortable headroom without hanging the run if a webview
  // wedges (executeAsync still rejects on its own timeout). Best-effort.
  try { await browser.setTimeout({ script: 30000 }); } catch (_) {}
  // Install the exception hooks before the first snapshot so even errors thrown
  // during initial render are captured.
  await installHooks(browser);
  // Install the Long Tasks observer (jank/hang watchdog) so it is live for every
  // action. Re-installed in observe() since a navigation replaces the window.
  await installLongTaskObserver(browser);
  // Install the cross-engine rAF frame observer too (the path that catches
  // jank/hang on Tauri's WebKit webview, where Long Tasks is unavailable).
  // Re-installed in observe() since a navigation replaces the window.
  await installFrameObserver(browser);
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
    // Re-install hooks first (a navigation since the last observe would have
    // replaced the window and dropped them); installHooks is idempotent.
    await installHooks(browser);
    // Re-install the Long Tasks observer too (a navigation drops it); idempotent.
    await installLongTaskObserver(browser);
    // Re-install the cross-engine rAF frame observer too (a navigation drops it).
    await installFrameObserver(browser);
    // Drain any errors that the just-completed action produced. observe() runs
    // after every action (tap and back), so this covers all action sites.
    await drainErrors(browser);
    const snap = await snapshot(browser, valueNodeSelectors);
    snap.sig = effectiveSig(snap);
    if (!seen.has(snap.sig)) {
      seen.add(snap.sig);
      // sig: STRUCTURAL (roles + tree shape + stable developer keys),
      //      locale-invariant.
      // labels: DISPLAY-ONLY visible text (map show), never in the sig.
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
      // Operability/accessibility ground truth for this newly-seen state, keyed
      // by the SAME sig. Tauri has no CDP, so it uses native+cursor+attr signals
      // and an in-page focusability rule (see GROUNDTRUTH_JS). The synthetic
      // keydown probe can mutate the DOM, so it runs AFTER the state is recorded.
      await emitGroundtruth(browser, snap.sig);
      // DOM/layout overflow for this newly-seen state, keyed by the SAME sig.
      // Pure structural measurement, no pixels, so it reproduces on replay. Only
      // emitted when something overflows; a clean layout stays silent.
      let ovf = null;
      try { ovf = await browser.execute(DETECT_OVERFLOW_JS); } catch (_) {}
      if (ovf && ovf.length) {
        log('EXPLORE:OVERFLOW ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: ovf }));
      }
      // CONTENT-BUG for this newly-seen state, keyed by the SAME sig. Pure
      // DOM/label scan (no pixels, no timing), so it reproduces on replay. Only
      // emitted when a broken-content artifact is actually rendered.
      let cbug = null;
      try { cbug = await browser.execute(DETECT_CONTENTBUG_JS); } catch (_) {}
      if (cbug && cbug.length) {
        log('EXPLORE:CONTENTBUG ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: cbug }));
      }
    }
    return snap;
  };

  let current = await observe(), stuck = 0;
  const prefix = fuzz.prefix || null, replay = fuzz.replay || null;
  const prefixLen = prefix ? prefix.length : 0;
  const budget = replay ? replay.length : ((fuzz.budget || ACTION_BUDGET) + prefixLen);
  const exercisedChoiceStates = new Set(); // sigs whose choice components were exercised
  // LEAK sampler: in REPLAY mode (the `--soak` tier writes {"replay":[...]}),
  // sample memory at the start and after every action so the Rust soak oracle gets
  // a heap-vs-time series. Off outside replay. t0 anchors t_ms. PRIMARY signal is
  // the webview process RSS (real, coarse); FALLBACK is performance.memory (no CDP
  // over WebDriver); see sampleHeap. tauriPid caches the resolved host pid.
  const t0 = Date.now();
  const tauriPid = { pid: null, tried: false };
  if (replay) await sampleHeap(browser, 0, tauriPid);
  for (let a = 0; a < budget && stuck < 3; a++) {
    // LEAK sampler: in replay mode, sample once per action (fires BEFORE acting,
    // so action a's sample reflects the heap after the previous action settled).
    if (replay && a > 0) await sampleHeap(browser, Date.now() - t0, tauriPid);
    // COMPONENT-CHOICE differential (fuzz only, not replay), ported from the web
    // runner. Tauri has no CDP, so the SAME self-contained in-page pass is injected
    // via executeAsync(): it finds the webview's choice components (native
    // <select>, ARIA tab/radio groups, button-cluster pickers), exercises each
    // option, measures the global-layout effect, and returns the outlier(s) using
    // the SHARED threshold rule -- entirely in-page, so it needs no presented-frame
    // or status stream the WebDriver surface lacks. Non-destructive (it restores
    // each component) and once per state per seed. Each finding -> EXPLORE:CHOICEBUG.
    if (!replay && !exercisedChoiceStates.has(current.sig)) {
      exercisedChoiceStates.add(current.sig);
      let findings = [];
      try { findings = await browser.executeAsync(CHOICE_ANOMALY_ASYNC_JS); } catch (_) { findings = []; }
      let emitted = false;
      for (const f of (findings || [])) {
        emitted = true;
        log('EXPLORE:CHOICEBUG ' + JSON.stringify({
          from: current.sig,
          role: f.role,
          outlier: f.outlier,
          magnitude: f.magnitude,
          siblingMedian: f.siblingMedian,
        }));
      }
      if (emitted) { current = await observe(); continue; }
    }
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
      // the webview to REPROIT_SHOTS_DIR and emit the SHOOT marker. It does not
      // move the known state, so no observe/stuck change.
      await shoot(browser, act.slice('shoot:'.length));
      continue;
    }
    if (act === 'back') {
      const before = current.sig;
      const beforeContent = current.content;
      await browser.back().catch(() => {});
      await browser.pause(600);
      const next = await observe();
      if (next.sig !== before) {
        log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'back', to: next.sig }));
        stuck = 0;
      } else if (next.content !== beforeContent) {
        // Layer-1: the action changed on-screen content without moving the
        // structural sig (a value-state change on a capped node). EFFECTIVE, so
        // do not count it as stuck, but no graph edge is added.
        stuck = 0;
      } else stuck++;
      current = next; continue;
    }
    const sel = act.slice('tap:'.length);
    tried.add(current.sig + '|' + sel);
    const before = current.sig;
    const beforeContent = current.content;
    try { await browser.execute(MARK_ANCHORS_JS); } catch (e) {} // flicker oracle: tag persistent chrome
    try { await browser.execute(RESET_LONGTASK_JS); } catch (e) {} // jank/hang: drop pre-action longtasks
    try { await browser.execute(RESET_FRAME_JS); } catch (e) {} // jank/hang: drop pre-action rAF intervals
    if (!(await tap(browser, sel))) { log('FUZZ:MISS ' + act); stuck++; continue; }
    await browser.pause(700);
    // Tier-1 flicker oracle: did this transition rebuild persistent chrome that
    // did not change? (DOM node-identity churn; settled either way, so invisible
    // to the visual oracle.) Reported per transition, independent of the sig move.
    let tapChurn = null;
    try { tapChurn = await browser.execute(CHURNED_ANCHORS_JS); } catch (e) {}
    if (tapChurn && tapChurn.length) {
      log('EXPLORE:RERENDER ' + JSON.stringify({ from: before, action: 'tap:' + sel, churned: tapChurn }));
    }
    // JANK/HANG watchdog: did this action block the main thread past the
    // jank/hang floor? Keyed by (from, action) like the flicker oracle, so the
    // Rust side attributes it to this transition and `check` re-confirms it.
    // drainJankForEngine uses the precise Long Tasks path on WebView2/Chromium
    // and the cross-engine rAF path on Tauri's WebKit webview, where Long Tasks
    // is unavailable, so the signal is no longer silent on mac/Linux.
    const tapJank = await drainJankForEngine(browser);
    if (tapJank) {
      log('EXPLORE:' + (tapJank.kind === 'hang' ? 'HANG' : 'JANK') + ' ' +
        JSON.stringify({ from: before, action: 'tap:' + sel, bucket: tapJank.bucket, count: tapJank.count }));
    }
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
  // LEAK sampler: a final sample after the last action, so the series spans the
  // whole soak (start ... last action). No-op outside replay.
  if (replay) await sampleHeap(browser, Date.now() - t0, tauriPid);
  // Final drain: catch any error produced by the last action (or by async work
  // that settled after the last observe).
  await drainErrors(browser);
  log(`JOURNEY[a] step: explored ${seen.size} states`);
  log('JOURNEY DONE');
  log('All tests passed');
  await browser.deleteSession();
}

// Only auto-run when invoked as the entry point. When imported (e.g. by the
// parity test) the canonical signature is exported without connecting WebDriver.
const INVOKED_DIRECTLY = process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href;
if (INVOKED_DIRECTLY) {
  main().catch((e) => {
    log('EXCEPTION CAUGHT BY TAURI RUNNER');
    log(String(e && e.stack ? e.stack : e));
    log('Some tests failed');
    process.exit(0);
  });
}
