// ReproIt RN runner: drives a React Native app over an Appium session and emits
// the SAME marker protocol as the Flutter and web runners, so the whole map /
// graph / fuzz / soak / a11y / evidence pipeline works on RN unchanged. Appium's
// accessibility source (the page-source XML) is to RN what the semantics tree is
// to Flutter and the a11y tree is to web.
//
// The state signature is the CANONICAL STRUCTURAL signature (docs/signature.md):
// we walk Appium's page-source XML into a canonical Node tree (role from native
// a11y traits + class -> the fixed vocabulary; id from resource-id / testID /
// accessibility-id; type for inputs; icon if available; children) and hash the
// normalized descriptor with FNV-1a. It is byte-identical to the Rust oracle
// (crates/reproit/src/model/signature.rs), the web/RN SDKs, and the golden
// vectors (signature_vectors.json). Localized text NEVER enters the hash; it is
// kept only as display-only labels + an elements list with structural selectors.
//
// Records (one JSON per line, parsed from stdout):
//   EXPLORE:STATE {"sig":..,"labels":[..],"elements":[{sel,role,label,nokey?}]}
//   EXPLORE:EDGE  {"from":..,"action":"tap:<selector>"|"back","to":..}
//                 selector = "key:<id>" or "role:<role>#<idx>", never text.
//
// Env (set by the orchestrator's rn-appium runner):
//   REPROIT_APPIUM_URL    Appium server base URL (e.g. http://127.0.0.1:4723)
//   REPROIT_APPIUM_CAPS   JSON capabilities (platformName, app, deviceName, ...)
//   REPROIT_FUZZ_CONFIG   seed/budget/replay/prefix json
//
// stdout is the marker stream; the orchestrator captures it like a drive log.
//
// STATUS: v0, structurally complete and sharing the exact signature contract
// that web and Flutter validated. End-to-end validation needs a running Appium
// server + an iOS sim or Android emulator + the app build; see CLOUD.md.

import { remote } from 'webdriverio';
import { readFileSync, existsSync } from 'node:fs';
import { resolve } from 'node:path';

const APPIUM = process.env.REPROIT_APPIUM_URL || 'http://127.0.0.1:4723';
const CAPS = JSON.parse(process.env.REPROIT_APPIUM_CAPS || '{}');
const ACTION_BUDGET = 36;
const MAX_LABEL_LEN = 40;
// Layer-1 value-class cap (docs/signature.md "Value-state"): once a structural
// node has shown more than this many DISTINCT value-class combinations, the
// runner drops it to structural-only so an adversarial value generator cannot
// explode the graph. The oracle is stateless; the cap is purely runner-local.
const VALUE_CLASS_CAP = 8;

function log(line) { process.stdout.write(line + '\n'); }
function loadFuzz() {
  const p = process.env.REPROIT_FUZZ_CONFIG;
  if (!p) return {};
  try { return JSON.parse(readFileSync(p, 'utf8')); } catch { return {}; }
}

// Layer-3 opt-in (docs/signature.md "Value-state"): read `value_nodes:`
// selectors from reproit.yaml. We avoid adding a YAML dependency: the block is
// a simple flat list of strings, so a tiny line parser is enough and keeps the
// runner dependency-free. Path precedence: REPROIT_CONFIG env, else
// ./reproit.yaml in the cwd. A missing/unparseable file yields an empty list
// (value-less behavior, fully backward-compatible). Mirrors web-runner.
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
      const body = inline.replace(/^\[/, '').replace(/\].*$/, '');
      for (const part of body.split(',')) { const v = clean(part); if (v) out.push(v); }
      return out;
    }
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

// xorshift32, identical to the Flutter/web runners so seeds mean the same thing.
function rng(seed) {
  let s = (seed >>> 0) || 1;
  return (n) => { s ^= (s << 13); s >>>= 0; s ^= (s >> 17); s ^= (s << 5); s >>>= 0; return (s & 0x7fffffff) % n; };
}

// FNV-1a over an arbitrary descriptor string. Matches the Rust oracle / web SDK
// / explorer.dart so signatures and seeds line up across platforms.
function fnv1a(s) {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; }
  return (h >>> 0).toString(16).padStart(8, '0');
}

// ====================================================================
//  CANONICAL STRUCTURAL SIGNATURE (pure, Node-tree -> 8 hex)
//  Byte-identical to crates/reproit/src/model/signature.rs, the RN/web SDKs,
//  and signature_vectors.json. Spec: docs/signature.md.
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

// ====================================================================
//  Appium page-source XML -> canonical Node tree
//  Appium exposes nested elements with platform-specific tags + attributes:
//    iOS (XCUITest): <XCUIElementTypeButton name=".." label=".." value=".."
//                     enabled=".." visible=".." accessible=".."/>
//    Android (UiA2): <android.widget.Button text=".." content-desc=".."
//                     resource-id=".." class=".." clickable=".."/>
//  We map each element to a canonical role from its tag/class + a11y traits
//  (NEVER from visible text), pull a stable id, refine input types, and recurse.
// ====================================================================

// iOS XCUIElementType* tag -> canonical role.
function roleFromXcui(tag) {
  switch (tag) {
    case 'XCUIElementTypeButton':
    case 'XCUIElementTypeBackButton':
    case 'XCUIElementTypeMenuButton':
    case 'XCUIElementTypeToolbarButton':
      return 'button';
    case 'XCUIElementTypeLink':
      return 'link';
    case 'XCUIElementTypeTextField':
    case 'XCUIElementTypeSecureTextField':
    case 'XCUIElementTypeSearchField':
    case 'XCUIElementTypeTextView':
      return 'textfield';
    case 'XCUIElementTypeStaticText':
      return 'text';
    case 'XCUIElementTypeImage':
      return 'image';
    case 'XCUIElementTypeSwitch':
    case 'XCUIElementTypeToggle':
      return 'switch';
    case 'XCUIElementTypeSlider':
      return 'slider';
    case 'XCUIElementTypeCheckBox':
      return 'checkbox';
    case 'XCUIElementTypeRadioButton':
      return 'radio';
    case 'XCUIElementTypeTabBar':
    case 'XCUIElementTypeSegmentedControl':
      return 'menu';
    case 'XCUIElementTypeTab':
      return 'tab';
    case 'XCUIElementTypeNavigationBar':
      return 'header';
    case 'XCUIElementTypeTable':
    case 'XCUIElementTypeCollectionView':
    case 'XCUIElementTypeScrollView':
      return 'list';
    case 'XCUIElementTypeCell':
      return 'listitem';
    case 'XCUIElementTypeMenu':
      return 'menu';
    case 'XCUIElementTypeMenuItem':
      return 'menuitem';
    case 'XCUIElementTypeAlert':
    case 'XCUIElementTypeSheet':
    case 'XCUIElementTypeDialog':
      return 'dialog';
    case 'XCUIElementTypeActivityIndicator':
    case 'XCUIElementTypeProgressIndicator':
      return 'progress';
    case 'XCUIElementTypeApplication':
    case 'XCUIElementTypeWindow':
      return 'screen';
    default:
      return null;
  }
}

// Android widget class -> canonical role. The class attribute (or the tag) holds
// the fully-qualified widget name; we match on its leaf, case-insensitively.
function roleFromAndroid(cls) {
  const c = cls.toLowerCase();
  if (c.includes('imagebutton') || c.includes('togglebutton')) return 'button';
  if (c.includes('button')) return 'button';
  if (c.includes('edittext') || c.includes('autocompletetextview') || c.includes('textinput')) return 'textfield';
  if (c.includes('switch')) return 'switch';
  if (c.includes('seekbar') || c.includes('slider')) return 'slider';
  if (c.includes('checkbox')) return 'checkbox';
  if (c.includes('radiobutton')) return 'radio';
  if (c.includes('progressbar')) return 'progress';
  if (c.includes('imageview') || c.includes('image')) return 'image';
  if (c.includes('tablayout')) return 'menu';
  if (c.includes('recyclerview') || c.includes('listview') || c.includes('scrollview')) return 'list';
  if (c.includes('viewgroup') || c.includes('linearlayout') || c.includes('framelayout') || c.includes('relativelayout')) return 'group';
  if (c.includes('textview')) return 'text';
  if (c.includes('toolbar') || c.includes('actionbar')) return 'header';
  return null;
}

// ARIA-style / generic a11y trait (accessibility-role, role) -> canonical role.
function roleFromTrait(trait) {
  switch ((trait || '').toLowerCase()) {
    case 'header': case 'heading': return 'header';
    case 'button': case 'imagebutton': case 'togglebutton': return 'button';
    case 'link': return 'link';
    case 'search': case 'searchbox': case 'combobox': case 'textbox': return 'textfield';
    case 'image': case 'img': return 'image';
    case 'switch': return 'switch';
    case 'checkbox': return 'checkbox';
    case 'radio': return 'radio';
    case 'adjustable': case 'slider': return 'slider';
    case 'tab': return 'tab';
    case 'tablist': case 'menubar': case 'toolbar': case 'menu': return 'menu';
    case 'menuitem': return 'menuitem';
    case 'list': return 'list';
    case 'listitem': case 'cell': return 'listitem';
    case 'alert': case 'dialog': return 'dialog';
    case 'text': case 'summary': return 'text';
    case 'progressbar': return 'progress';
    default: return null;
  }
}

// Canonical role for an Appium element: explicit a11y trait wins, then the iOS
// XCUI tag, then the Android widget class/tag, else `node`. Never from text.
function roleOfEl(tag, get) {
  const trait = get('accessibility-role') || get('role') || '';
  if (trait) { const r = roleFromTrait(trait); if (r) return r; }
  const xc = roleFromXcui(tag); if (xc) return xc;
  const cls = get('class') || tag;
  const ar = roleFromAndroid(cls); if (ar) return ar;
  return 'node';
}

// Stable developer id: resource-id (Android) > accessibility-id / testID > name.
// On iOS, `name` is the accessibilityIdentifier when set (else the label), so we
// only take it when it looks like an identifier (no spaces) to avoid folding
// localized text into the hash; the display label is captured separately.
function idOfEl(get) {
  const rid = get('resource-id');
  if (rid && rid.trim()) {
    const leaf = rid.includes('/') ? rid.split('/').pop() : rid;
    if (leaf && leaf.trim()) return leaf.trim();
  }
  for (const key of ['accessibility-id', 'testID', 'test-id', 'nativeID']) {
    const v = get(key);
    if (v && v.trim()) return v.trim();
  }
  const name = get('name');
  if (name && name.trim() && !/\s/.test(name.trim())) return name.trim();
  return null;
}

// Input-type refinement for textfields. iOS SecureTextField + Android password
// flags => password; numeric/email keyboards refine the rest. Never text value.
function typeOfEl(tag, get, role) {
  if (role !== 'textfield') return null;
  if (tag === 'XCUIElementTypeSecureTextField') return 'password';
  if (tag === 'XCUIElementTypeSearchField') return 'search';
  if (get('password') === 'true') return 'password';
  const it = (get('inputType') || get('keyboardType') || '').toLowerCase();
  if (it.includes('password')) return 'password';
  if (it.includes('email')) return 'email';
  if (it.includes('number') || it.includes('numeric') || it.includes('phone')) return 'number';
  if (it.includes('search')) return 'search';
  const t = (get('type') || '').toLowerCase();
  if (['text', 'password', 'email', 'number', 'search'].includes(t)) return t;
  return 'text';
}

// Language-independent icon identity from a stable attribute (no visible text).
function iconOfEl(get) {
  for (const key of ['icon', 'icon-name', 'data-icon']) {
    const v = get(key);
    if (v && v.trim()) return v.trim();
  }
  return null;
}

// Transient heuristic: progress role, live-region announcement, or a flagged
// class drops the node + subtree from the hash (matches the web/RN SDKs).
function isTransientEl(get, role, cls) {
  if (role === 'progress') return true;
  const live = (get('aria-live') || get('live-region') || '').toLowerCase();
  if (live === 'assertive' || live === 'polite') return true;
  const trait = (get('accessibility-role') || get('role') || '').toLowerCase();
  if (trait === 'alert' || trait === 'status' || trait === 'timer') return true;
  if (/\b(toast|snackbar|spinner|progress|loader|loading|tooltip|badge)\b/.test((cls || '').toLowerCase())) return true;
  return false;
}

// The RAW value-role of an Appium element for the Layer-2 value-class (docs/
// signature.md "Value-state"), derived from a11y traits + tag/class, NEVER from
// text. Distinct from roleOfEl: it returns one of the value-role names
// (status/log/progressbar/meter/timer/output) for the matching a11y roles and
// "textfield" for text-entry controls, so the canonical is_value_bearing test
// sees the RAW role the oracle expects. A live-region (polite/assertive) maps to
// "status" so a counter/stopwatch readout is value-bearing WITHOUT opt-in.
// Returns null for chrome and for password fields (never read).
function valueRoleOfEl(tag, get, role) {
  const trait = (get('accessibility-role') || get('role') || '').toLowerCase();
  if (trait === 'status' || trait === 'log' || trait === 'progressbar' ||
      trait === 'meter' || trait === 'timer' || trait === 'output') {
    return trait;
  }
  const live = (get('aria-live') || get('live-region') || '').toLowerCase();
  if (live === 'polite' || live === 'assertive') return 'status';
  // Text-entry controls hold an editable value: they are textfield value-roles.
  // A secure (password) field is never read.
  if (role === 'textfield') {
    if (tag === 'XCUIElementTypeSecureTextField') return null;
    if (get('password') === 'true') return null;
    return 'textfield';
  }
  return null;
}

// The displayed data value of a value-role element, NEVER from a password. For
// text-entry controls and status/output/live nodes Appium surfaces the current
// content under `value` (iOS) / `text` (Android) / content-desc; we read those
// stable attributes only. The raw value never enters the canonical key (it is
// bucketed to a value-class), and it feeds the Layer-1 content fingerprint.
function valueOfEl(get) {
  const v = get('value');
  if (v != null && v !== '') return String(v);
  const t = get('text');
  if (t != null && t !== '') return String(t);
  const cd = get('content-desc');
  if (cd != null && cd !== '') return String(cd);
  return '';
}

// Display-only accessible name (label/content-desc/text). Never in the hash.
function nameOfEl(get) {
  return (get('label') || get('content-desc') || get('text') || get('value') || '').trim().split('\n')[0].trim();
}

// Interactive: a tappable role, or an explicit clickable/enabled-button flag.
function isTappableEl(get, role) {
  if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role)) return true;
  if (get('clickable') === 'true') return true;
  return false;
}

// Clip an accessible name to the display label cap (display only; never hashed).
function clipLabel(name) {
  if (name.length <= MAX_LABEL_LEN) return name;
  const suffix = '#' + fnv1a(name);
  return name.slice(0, MAX_LABEL_LEN - suffix.length) + suffix;
}

// ---- a tiny, dependency-free XML tree parser ------------------------------
// Appium page source is well-formed XML. We tokenize tags (open / self-close /
// close) and build a nesting tree of { tag, attrs, children }. Text nodes are
// ignored (all signal lives in attributes), which is exactly what we want since
// localized text never enters the signature.
function parseXml(xml) {
  const tagRe = /<(\/)?([A-Za-z_][\w.\-]*)((?:\s+[\w:.\-]+="[^"]*")*)\s*(\/?)>/g;
  const attrRe = /([\w:.\-]+)="([^"]*)"/g;
  const root = { tag: '#root', attrs: {}, children: [] };
  const stack = [root];
  let m;
  while ((m = tagRe.exec(xml))) {
    const closing = m[1] === '/';
    const tag = m[2];
    const rawAttrs = m[3] || '';
    const selfClose = m[4] === '/';
    if (closing) {
      if (stack.length > 1) stack.pop();
      continue;
    }
    const attrs = {};
    let a;
    while ((a = attrRe.exec(rawAttrs))) attrs[a[1]] = decodeXmlEntities(a[2]);
    const node = { tag, attrs, children: [] };
    stack[stack.length - 1].children.push(node);
    if (!selfClose) stack.push(node);
  }
  return root;
}
function decodeXmlEntities(s) {
  return s
    .replace(/&lt;/g, '<').replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"').replace(/&apos;/g, "'")
    .replace(/&amp;/g, '&');
}

// Test one element's stable id / canonical role against the active Layer-3
// value-node selectors (docs/signature.md "Value-state"). key:<id> compares the
// node's stable id; role:<role>#<idx> matches the idx-th element of that
// canonical role in document order (out.roleSeen supplies the running index).
function matchesValueNode(out, id, role, myRoleIndex) {
  const sels = out.valueNodeSelectors || [];
  if (!sels.length) return false;
  for (const sel of sels) {
    if (!sel) continue;
    if (sel.indexOf('key:') === 0) {
      const want = sel.slice(4);
      if (want && id != null && id === want) return true;
    } else if (sel.indexOf('role:') === 0) {
      const hash = sel.indexOf('#');
      if (hash < 0) continue;
      const wantRole = sel.slice(5, hash);
      const idx = parseInt(sel.slice(hash + 1), 10);
      if (!(idx >= 0)) continue;
      if (role === wantRole && myRoleIndex === idx) return true;
    }
  }
  return false;
}

// Build the canonical Node tree from a parsed XML element subtree. Invisible
// elements (visible="false") are skipped but their visible descendants are
// hoisted, matching the SDKs. The display labels / elements list are collected
// along the way. Returns an array of canonical Node children.
function buildNodes(xmlEl, out) {
  const nodes = [];
  for (const child of xmlEl.children) {
    appendNode(child, out, nodes);
  }
  return nodes;
}
function appendNode(xmlEl, out, into) {
  const attrs = xmlEl.attrs;
  const get = (name) => (attrs[name] != null ? attrs[name] : '');
  if (get('visible') === 'false') {
    // hoist visible descendants of an invisible wrapper
    for (const child of xmlEl.children) appendNode(child, out, into);
    return;
  }
  const tag = xmlEl.tag;
  const cls = get('class') || tag;
  const role = roleOfEl(tag, get);
  const id = idOfEl(get);
  // Document-order index of this element among same-canonical-role peers, for a
  // Layer-3 role:<role>#<idx> value-node selector. Incremented for every element.
  const myRoleIndex = out.roleSeen[role] || 0;
  out.roleSeen[role] = myRoleIndex + 1;

  // Value-state (Layer 2): a value-role element (by trait/tag, or a live region)
  // or a Layer-3 opt-in node is value-bearing. Value-bearing WINS over the
  // transient heuristic, so a role=status / live-region counter that the
  // transient heuristic would otherwise drop is kept as a value node instead,
  // and its updates produce DISTINCT value-states.
  const vrole = valueRoleOfEl(tag, get, role);
  const optIn = matchesValueNode(out, id, role, myRoleIndex);
  const valueBearing = !!vrole || optIn;
  const transient = !valueBearing && isTransientEl(get, role, cls);
  const node = { role };
  if (id != null) node.id = id;
  const type = typeOfEl(tag, get, role);
  if (type != null) node.type = type;
  const icon = iconOfEl(get);
  if (icon != null) node.icon = icon;
  if (valueBearing) {
    node.value = valueOfEl(get);
    // The flag makes the canonical is_value_bearing accept the node even when
    // roleOfEl normalized its raw value-role (status/output/...) to "node".
    node.value_node = true;
    // Layer-1 content fingerprint: a value node's stable key + its raw value.
    const fkey = id != null ? 'key:' + id : 'vrole:' + (vrole || 'opt');
    out.textNodes.push([fkey, node.value]);
  }
  if (transient) { node.transient = true; into.push(node); return; }

  // Layer-1 content fingerprint over keyed text-bearing nodes (runner-local, NOT
  // canonical): a keyed text/static element's own value contributes (stable-key,
  // text). This catches a display whose text changes without any structural move
  // (a calculator/counter) so the action is seen as EFFECTIVE even when the node
  // was not detected as a value-role. The raw text never enters the canonical key.
  if (id != null && !valueBearing && (role === 'text' || role === 'header')) {
    const own = valueOfEl(get);
    if (own) out.textNodes.push(['text:' + id, own]);
  }

  // display labels + elements list (never in the hash)
  const name = nameOfEl(get);
  if (name) {
    const lbl = clipLabel(name);
    if (!out.seenLabel.has(lbl)) { out.seenLabel.add(lbl); out.labels.push(lbl); }
  }
  if (isTappableEl(get, role)) {
    const display = name ? clipLabel(name) : '';
    const idx = out.perRole[role] || 0;
    out.perRole[role] = idx + 1;
    const sel = id != null ? `key:${id}` : `role:${role}#${idx}`;
    out.elements.push({ sel, role, label: display, key: id, nokey: id == null });
    if (!display) out.unlabeled++;
  }

  node.children = buildNodes(xmlEl, out);
  into.push(node);
}

// The screen anchor: the foreground activity (Android) or the app bundle/window
// (iOS), when observable. The route/activity is the canonical anchor prefix.
function anchorFrom(xmlRoot, activity) {
  if (activity && String(activity).trim()) return String(activity).trim();
  // Fall back to the top window/application element's name if it is an id-like
  // token (avoids folding a localized window title into the anchor).
  const top = xmlRoot.children[0];
  if (top) {
    const name = top.attrs.name || '';
    if (name && !/\s/.test(name)) return name;
  }
  return null;
}

async function snapshot(driver, valueNodeSelectors) {
  const xml = await driver.getPageSource();
  const xmlRoot = parseXml(xml);
  let activity = null;
  try {
    if (typeof driver.getCurrentActivity === 'function') activity = await driver.getCurrentActivity();
  } catch { /* iOS / unsupported: anchor stays best-effort */ }
  const out = {
    labels: [], elements: [], unlabeled: 0, seenLabel: new Set(), perRole: {},
    // roleSeen: document-order count of elements per canonical role, used to
    // resolve a Layer-3 role:<role>#<idx> value-node selector.
    roleSeen: {},
    // textNodes: (stable-key, raw text) pairs feeding the Layer-1 content
    // fingerprint. Carries localized text; NEVER folded into the canonical key.
    textNodes: [],
    valueNodeSelectors: valueNodeSelectors || [],
  };
  // The canonical root is a single `screen` node; the parsed app subtree hangs
  // under it (parallels the SDKs forcing the root role to "screen").
  const screen = { role: 'screen', children: buildNodes(xmlRoot, out) };
  const anchor = anchorFrom(xmlRoot, activity);
  const sig = signatureOf(anchor, screen);
  // Structural-only signature (no V: section): the per-node key the Layer-1 cap
  // tracks. Computed by hashing the descriptor with the value-class suffix
  // stripped, so it is the exact pre-value-state signature of this structure.
  const full = descriptorOf(anchor, screen);
  const vAt = full.indexOf('\nV:');
  const vsection = vAt >= 0 ? full.slice(vAt + 3) : '';
  const structuralSig = vAt >= 0 ? fnv1a(full.slice(0, vAt)) : sig;
  // Layer-1 content fingerprint (runner-local, ephemeral): structural sig plus
  // the sorted (stable-key, trimmed raw text) list. An action is EFFECTIVE iff
  // the structural sig OR this fingerprint changed (see observe/effect checks).
  out.textNodes.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : (a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0)));
  const content = sig + '|' + out.textNodes.map((p) => p[0] + '=' + p[1]).join(';');
  return {
    sig,
    structuralSig,
    vsection,
    content,
    anchor,
    labels: [...new Set(out.labels)],
    elements: out.elements,
    unlabeled: out.unlabeled,
  };
}

// STRUCTURAL tap: resolve a canonical selector and click it. No visible text is
// used to locate the element.
//   key:<id>      -> resource-id / accessibility-id / testID / name
//   role:<role>#<idx> -> the idx-th tappable element of that role, document order
async function tap(driver, sel, snap) {
  if (sel.startsWith('key:')) {
    const id = sel.slice('key:'.length);
    const strategies = [
      `~${id}`,
      `//*[@resource-id="${id}"]`,
      `//*[contains(@resource-id,"/${id}")]`,
      `//*[@name="${id}"]`,
      `//*[@content-desc="${id}"]`,
    ];
    for (const s of strategies) {
      try { const el = await driver.$(s); if (await el.isExisting()) { await el.click(); return true; } }
      catch { /* next */ }
    }
    return false;
  }
  if (sel.startsWith('role:')) {
    // Resolve via the elements list captured in THIS snapshot (same structural
    // index basis as the signature), then click by its label/key if it has one.
    const el = (snap.elements || []).find((e) => e.sel === sel);
    if (!el) return false;
    const candidates = [];
    if (el.key) candidates.push(`~${el.key}`, `//*[@resource-id="${el.key}"]`, `//*[@name="${el.key}"]`);
    if (el.label) candidates.push(`~${el.label}`, `//*[@label="${el.label}"]`, `//*[@text="${el.label}"]`, `//*[@content-desc="${el.label}"]`);
    for (const s of candidates) {
      try { const e = await driver.$(s); if (await e.isExisting()) { await e.click(); return true; } }
      catch { /* next */ }
    }
    return false;
  }
  return false;
}

// The target app's identifier, for the crash oracle.
function targetAppId() {
  return (
    CAPS['appium:appPackage'] ||
    CAPS.appPackage ||
    CAPS['appium:bundleId'] ||
    CAPS.bundleId ||
    ''
  );
}

// Emit the EXACT exception block the Rust oracle parses (drive.rs: a line
// containing "EXCEPTION CAUGHT BY" opens the block, a line of pure ═ closes it).
function emitCrash(action) {
  log('EXCEPTION CAUGHT BY RN RUNNER');
  log('The following error was thrown:');
  log('app crashed during ' + action + ' (foreground left ' + targetAppId() + ')');
  log('════════');
}

// Conservatively decide whether the target app has left the foreground.
async function appCrashed(driver) {
  const target = targetAppId();
  if (!target) return false;
  const wantPkg = CAPS['appium:appPackage'] || CAPS.appPackage || '';
  try {
    if (wantPkg && typeof driver.getCurrentPackage === 'function') {
      const pkg = await driver.getCurrentPackage();
      if (pkg && pkg !== wantPkg) return true;
    }
  } catch { /* probe unavailable; try queryAppState */ }
  try {
    if (typeof driver.queryAppState === 'function') {
      const state = await driver.queryAppState(target);
      if (typeof state === 'number' && state < 4) return true;
    }
  } catch { /* probe unavailable: stay silent */ }
  return false;
}

async function main() {
  const fuzz = loadFuzz();
  const url = new URL(APPIUM);
  const driver = await remote({
    hostname: url.hostname,
    port: Number(url.port) || 4723,
    path: url.pathname && url.pathname !== '/' ? url.pathname : '/',
    capabilities: { 'appium:autoGrantPermissions': true, ...CAPS },
    logLevel: 'error',
  });

  log('JOURNEY claimed role=a');
  await driver.pause(1500);

  const seenStates = new Set();
  const triedEdges = new Set();
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

  async function observe() {
    const snap = await snapshot(driver, valueNodeSelectors);
    snap.sig = effectiveSig(snap);
    if (!seenStates.has(snap.sig)) {
      seenStates.add(snap.sig);
      // sig: CANONICAL STRUCTURAL signature (anchor + normalized Node tree),
      //      locale-invariant.
      // labels: DISPLAY-ONLY visible text (map --show), never in the sig.
      // elements: structural selectors for replay; `nokey` flags a tappable with
      //           no stable id so the map layer can warn the developer.
      log('EXPLORE:STATE ' + JSON.stringify({
        sig: snap.sig,
        // route: the foreground activity / screen anchor, so the candidate map
        // reconciles by route (the reliable join key), consistent with the web
        // and Flutter runners.
        ...(snap.anchor ? { route: snap.anchor } : {}),
        labels: snap.labels.slice(0, 24),
        elements: snap.elements.slice(0, 24).map((e) => {
          const o = { sel: e.sel, role: e.role, label: e.label };
          if (e.nokey) o.nokey = true;
          return o;
        }),
        unlabeled: snap.unlabeled,
      }));
    }
    return snap;
  }

  let current = await observe();
  let stuck = 0;
  let crashed = false;
  const prefix = fuzz.prefix || null;
  const replay = fuzz.replay || null;
  const prefixLen = prefix ? prefix.length : 0;
  const budget = replay ? replay.length : ((fuzz.budget || ACTION_BUDGET) + prefixLen);

  for (let actions = 0; actions < budget && stuck < 3; actions++) {
    let act;
    if (replay) act = replay[actions];
    else if (prefix && actions < prefixLen) act = prefix[actions];
    else if (fuzz.seed) {
      // Inverse-visit-count weighted pick over STRUCTURAL selectors, plus 'back'.
      // Seeded + deterministic, so replays reproduce exactly. Candidates are
      // addressed by selector (key, else role+index), never by visible text.
      const sels = current.elements.map((e) => e.sel).sort();
      const ew = (fuzz.edgeWeights && fuzz.edgeWeights[current.sig]) || {};
      const options = sels.map((s) => 'tap:' + s).concat(['back']);
      const weights = options.map((o) => 1 / (1 + (ew[o] || 0)));
      const total = weights.reduce((a, b) => a + b, 0);
      let r = (pick(1 << 20) / (1 << 20)) * total;
      act = options[options.length - 1];
      for (let k = 0; k < options.length; k++) { r -= weights[k]; if (r <= 0) { act = options[k]; break; } }
    } else {
      act = null;
      for (const el of current.elements) {
        if (!triedEdges.has(current.sig + '|' + el.sel)) { act = 'tap:' + el.sel; break; }
      }
      act = act || 'back';
    }

    log('FUZZ:ACT ' + act);
    if (act === 'back') {
      const before = current.sig;
      const beforeContent = current.content;
      try { await driver.back(); } catch { /* ignore */ }
      await driver.pause(700);
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
    const sel = act.slice('tap:'.length);
    triedEdges.add(current.sig + '|' + sel);
    const before = current.sig;
    const beforeContent = current.content;
    const ok = await tap(driver, sel, current);
    if (!ok) { log('FUZZ:MISS ' + act); stuck++; continue; }
    await driver.pause(800);
    // Crash oracle: if the target app left the foreground after this tap, the app
    // crashed (uncaught exception -> process died -> launcher).
    if (await appCrashed(driver)) { emitCrash(act); crashed = true; break; }
    const next = await observe();
    if (next.sig !== before) {
      log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'tap:' + sel, to: next.sig }));
      stuck = 0;
    } else if (next.content !== beforeContent) {
      // Layer-1 effect detection: the tap changed displayed content (a calculator
      // keypress / counter on a capped display) without a structural move.
      // EFFECTIVE, so reset stuck and keep driving; no self-edge is recorded.
      stuck = 0;
    }
    current = next;
  }

  log(`JOURNEY[a] step: explored ${seenStates.size} states`);
  log('JOURNEY DONE');
  log(crashed ? 'Some tests failed' : 'All tests passed');
  await driver.deleteSession();
}

// Only auto-run when invoked directly (not when imported by the parity test).
const invokedDirectly = process.argv[1] && import.meta.url === `file://${process.argv[1]}`;
if (invokedDirectly) {
  main().catch((e) => {
    log('EXCEPTION CAUGHT BY RN RUNNER');
    log('The following error was thrown:');
    log(String(e && e.message ? e.message : e));
    log('════════');
    log('Some tests failed');
    process.exit(0);
  });
}
