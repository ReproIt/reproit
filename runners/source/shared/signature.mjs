// Host-side canonical structural signature, shared by the Electron and Tauri
// runners (they hash the SAME canonical Node tree; only the transport that
// produces the tree differs, CDP vs WebDriver). This module is host-pure (no
// DOM, no runtime driver), so the parity gate imports it transitively through
// each runner without the heavy dependencies installed.
//
// The webview-side snapshot walks are NOT importable from here: they are
// stringified into the page (page.evaluate / browser.execute), so each runner
// keeps its self-contained in-page copy. This module is the single host-side
// implementation.
import { readFileSync, existsSync } from 'node:fs';
import { resolve as resolvePath } from 'node:path';

// Layer-3 opt-in (docs/signature.md "Value-state"): read `value_nodes:`
// selectors from reproit.yaml. We avoid adding a YAML dependency: the block is
// a simple flat list of strings, so a tiny line parser is enough and keeps the
// runner dependency-free. Path precedence: REPROIT_CONFIG env, else
// ./reproit.yaml in the cwd. A missing/unparseable file yields an empty list,
// so value-state is strictly opt-in.
function loadValueNodes() {
  let p = (process.env.REPROIT_CONFIG || '').trim();
  if (!p) {
    const def = resolvePath(process.cwd(), 'reproit.yaml');
    if (existsSync(def)) p = def;
  }
  if (!p || !existsSync(p)) return [];
  let text = '';
  try {
    text = readFileSync(p, 'utf8');
  } catch {
    return [];
  }
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
    const h = v.indexOf('#');
    if (h >= 0) v = v.slice(0, h).trim();
    if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'")))
      v = v.slice(1, -1);
    return v.trim();
  };
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(/^(\s*)value_nodes\s*:(.*)$/);
    if (!m) continue;
    const indent = m[1].length;
    const inline = m[2].trim();
    if (inline.startsWith('[')) {
      const body = inline.replace(/^\[/, '').replace(/\].*$/, '');
      for (const part of body.split(',')) {
        const v = clean(part);
        if (v) out.push(v);
      }
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

// The shared UTF-8 encoder for the canonical hash + V: byte-order sort. The
// descriptor and V: keys can carry non-ASCII (a localized anchor, a non-ASCII
// id, an emoji icon), so we MUST fold the UTF-8 BYTES, exactly like the Rust
// oracle's `desc.as_bytes()`. Folding UTF-16 code units silently diverged.
const REPROIT_UTF8 = new TextEncoder();

// FNV-1a over the UTF-8 BYTES of an arbitrary descriptor string. Used for the
// STRUCTURAL signature (fed a structure descriptor) and for hashing long
// labels in clipLabel. Matches the web runner / Rust oracle.
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
  for (let i = 0; i < n; i++) {
    if (ab[i] !== bb[i]) return ab[i] < bb[i] ? -1 : 1;
  }
  return ab.length === bb.length ? 0 : ab.length < bb.length ? -1 : 1;
}

// ====================================================================
//  CANONICAL STRUCTURAL SIGNATURE (pure, Node-tree -> 8 hex)
//  Byte-identical to the Rust oracle (crates/reproit/src/model/signature.rs),
//  runners/web/runner.mjs, and the golden vectors (signature_vectors.json).
//  Spec: docs/signature.md. This block is host-pure (no DOM): the runner's
//  in-page snapshot builds a Node tree in page context and feeds it here in
//  Node.
// ====================================================================
const ROLES = {
  screen: 1,
  header: 1,
  text: 1,
  button: 1,
  link: 1,
  textfield: 1,
  image: 1,
  icon: 1,
  list: 1,
  listitem: 1,
  tab: 1,
  switch: 1,
  checkbox: 1,
  radio: 1,
  slider: 1,
  menu: 1,
  menuitem: 1,
  dialog: 1,
  group: 1,
  node: 1,
};
const TRANSIENT_ROLES = { toast: 1, snackbar: 1, spinner: 1, progress: 1, tooltip: 1, badge: 1 };
// Value-role set (docs/signature.md "Value-state", Layer 2). A node is value-
// bearing iff it has a `value` AND either its RAW role is one of these OR it
// carries the opt-in value_node flag (Layer 3). status/log/progressbar/meter/
// timer/output are NOT in the structural vocabulary so they normalize to "node"
// in the body; the value-role test uses the RAW role on purpose. Chrome roles
// (button/header/text/link) are NEVER value-bearing (rule 1 preserved).
const VALUE_ROLES = {
  textfield: 1,
  status: 1,
  log: 1,
  progressbar: 1,
  meter: 1,
  timer: 1,
  output: 1,
};

function normalizeRole(role) {
  return ROLES[role] ? role : 'node';
}
function isTransientNode(node) {
  return !!node.transient || !!TRANSIENT_ROLES[node.role];
}
function isValueBearing(node) {
  return node.value != null && (!!VALUE_ROLES[node.role] || !!node.value_node);
}

function normalizeNode(node) {
  if (isTransientNode(node)) return null;
  const kids = [];
  const children = node.children || [];
  for (const c of children) {
    const n = normalizeNode(c);
    if (n) kids.push(n);
  }
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
    serializeNode(children[i], depth, j - i >= 2, tokens);
    i = j;
  }
}
// ---- Layer 2: value-class identity (canonical, mirrors the Rust oracle) ----
// Strict ^[+-]?[0-9]+(\.[0-9]+)?$: optional sign, >=1 ASCII digits, optional
// period + >=1 ASCII digits. No grouping, no exponent, no leading/trailing dot.
function isStrictDecimal(s) {
  let i = 0;
  const n = s.length;
  if (i < n && (s.charCodeAt(i) === 43 || s.charCodeAt(i) === 45)) i++;
  const intStart = i;
  while (i < n && s.charCodeAt(i) >= 48 && s.charCodeAt(i) <= 57) i++;
  if (i === intStart) return false;
  if (i < n && s.charCodeAt(i) === 46) {
    i++;
    const fracStart = i;
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
function signatureOf(anchor, root) {
  return fnv1a(descriptorOf(anchor, root));
}

export { signatureOf, descriptorOf, valueClass, fnv1a, loadValueNodes };
