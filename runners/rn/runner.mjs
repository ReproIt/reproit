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
//   EXPLORE:STATE      {"sig":..,"labels":[..],"elements":[{sel,role,label,nokey?}]}
//   EXPLORE:EDGE       {"from":..,"action":"tap:<selector>"|"back","to":..}
//                      selector = "key:<id>" or "role:<role>#<idx>", never text.
//   EXPLORE:OVERFLOW   {"sig":..,"items":[{key,kind,by}]}   per-state, structural
//   EXPLORE:CONTENTBUG {"sig":..,"items":[{key,reason,text}]} per-state, label scan
//   EXPLORE:HANG       {"from":..,"action":..,"bucket":..} per-transition watchdog
//   EXPLORE:JANK       {"from":..,"action":..,"bucket":..,"count":..} Android gfxinfo
//   MEMORY:SAMPLE      {"t_ms":..,"heap_used":..}  Android PSS series under --soak
// The OVERFLOW/CONTENTBUG/HANG/JANK/MEMORY markers share the EXACT contract the
// web runner emits and the Rust core already parses (model/map.rs, modes/soak.rs);
// the core is unchanged. iOS LEAK is now covered COARSELY (session-level process
// RSS sampled per replay cycle: the booted-sim app is a host process whose pid the
// runner resolves over `simctl spawn booted launchctl list`, read with host `ps`);
// see sampleIosHeap. iOS JANK stays a documented gap (no per-transition frame
// trace over the XCUITest session); see the HANG/JANK/LEAK section.
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
import { execFileSync } from 'node:child_process';

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
// (value-less behavior, fully backward-compatible). Mirrors runners/web.
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

// The element's on-screen frame as {l,t,r,b} in device pixels, or null when no
// geometry is exposed. Appium surfaces bounds in two platform shapes, both of
// which the page source carries as plain attributes (no extra round-trip):
//   Android (UiA2): bounds="[left,top][right,bottom]"
//   iOS (XCUITest): x="..", y="..", width="..", height=".."
// This is the same geometry an evidence screenshot crops to; we read it from the
// page source so the overflow oracle is a pure structural measurement (no pixel
// diff, no second query), reproducible byte-for-byte on replay.
function rectOfEl(get) {
  const b = get('bounds');
  if (b) {
    const m = b.match(/^\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]$/);
    if (m) {
      const l = parseInt(m[1], 10), t = parseInt(m[2], 10);
      const r = parseInt(m[3], 10), bot = parseInt(m[4], 10);
      if ([l, t, r, bot].every(Number.isFinite)) return { l, t, r, b: bot };
    }
  }
  const xs = get('x'), ys = get('y'), ws = get('width'), hs = get('height');
  if (xs !== '' && ys !== '' && ws !== '' && hs !== '') {
    const x = parseFloat(xs), y = parseFloat(ys), w = parseFloat(ws), h = parseFloat(hs);
    if ([x, y, w, h].every(Number.isFinite)) return { l: x, t: y, r: x + w, b: y + h };
  }
  return null;
}

// CONTENT-BUG classifier (deterministic, label-based). Byte-identical rule to
// the web runner's reasonOf (runners/web/runner.mjs): the literal artifacts a
// stringify/template bug leaks to the screen, matched on STRUCTURE (a literal
// token), never on natural language. Six classes, first match wins so a label
// carries one reason:
//   [object Object]     -> object-object       (an object coerced to a string)
//   {{ .. }} / ${ .. }  -> unrendered-template  (the binding never evaluated)
//   undefined           -> undefined  (whole word: \b guards ordinary prose)
//   null                -> null
//   NaN                 -> nan
// We scan the displayed text the runner already gathers (the same value/text/
// content-desc nameOfEl reads). A real label that merely mentions "null" in
// prose is NOT flagged: the token must stand alone, so the control stays silent.
function contentBugReason(text) {
  if (!text) return null;
  if (text.includes('[object Object]')) return 'object-object';
  if (/\{\{[^}]*\}\}/.test(text) || /\$\{[^}]*\}/.test(text)) return 'unrendered-template';
  if (/(^|[\s:>(\[,])undefined($|[\s.,!?)\]<])/.test(text)) return 'undefined';
  if (/(^|[\s:>(\[,])null($|[\s.,!?)\]<])/.test(text)) return 'null';
  if (/(^|[\s:>(\[,])NaN($|[\s.,!?)\]<])/.test(text)) return 'nan';
  return null;
}

// The raw displayed text of an element for the content-bug scan: label /
// content-desc / text / value, NEVER from a password field (it would leak the
// secret AND the masked dots are not a content bug). Full string (not clipped /
// not first-line-only like nameOfEl) so a multi-line "[object Object]" embedded
// past a newline is still caught.
function displayTextOfEl(tag, get, role) {
  if (role === 'textfield') {
    if (tag === 'XCUIElementTypeSecureTextField') return '';
    if (get('password') === 'true') return '';
  }
  for (const key of ['label', 'content-desc', 'text', 'value']) {
    const v = get(key);
    if (v != null && v !== '') return String(v);
  }
  return '';
}

// HOST-SIDE pure reducer: collected (key, kind, by) overflow tuples -> the sorted
// EXPLORE:OVERFLOW `items` array (byte-identical shape to the web runner / the
// Rust map.rs parser: each item is {key, kind, by}). Deduped on key|kind, sorted
// by key then kind so the marker is identical run to run / on replay. Pure +
// deterministic, so it is unit-testable in Node without a device.
function overflowItems(raw) {
  const out = [];
  const seen = new Set();
  for (const it of raw || []) {
    if (!it || !it.key || !it.kind) continue;
    const k = it.key + '|' + it.kind;
    if (seen.has(k)) continue;
    seen.add(k);
    out.push({ key: it.key, kind: it.kind, by: Math.round(it.by || 0) });
  }
  out.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : (a.kind < b.kind ? -1 : a.kind > b.kind ? 1 : 0)));
  return out;
}

// HOST-SIDE pure reducer: collected (key, reason, text) content-bug tuples -> the
// sorted EXPLORE:CONTENTBUG `items` array (byte-identical shape to the web runner
// / the Rust map.rs parser: each item is {key, reason, text}). Deduped on
// key|reason, sorted by key then reason, text clipped to 80 chars (display
// detail; the key+reason are the stable identity). Pure + deterministic.
function contentBugItems(raw) {
  const out = [];
  const seen = new Set();
  for (const it of raw || []) {
    if (!it || !it.key || !it.reason) continue;
    const k = it.key + '|' + it.reason;
    if (seen.has(k)) continue;
    seen.add(k);
    out.push({ key: it.key, reason: it.reason, text: String(it.text || '').slice(0, 80) });
  }
  out.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : (a.reason < b.reason ? -1 : a.reason > b.reason ? 1 : 0)));
  return out;
}

// Interactive: a tappable role, or an explicit clickable/enabled-button flag.
function isTappableEl(get, role) {
  if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role)) return true;
  if (get('clickable') === 'true') return true;
  return false;
}

// The canonical roles that, when present on an element, expose a real semantic
// role to assistive tech (a screen reader announces "button", "link", ...). A
// clickable element whose canonical role is NOT one of these (it normalized to
// the generic `group`/`node`, i.e. an Android android.view.ViewGroup with no
// accessibilityRole) is operable by finger but role-less to AT: the WCAG 4.1.2
// no_role gap. This is the host-readable native equivalent of the fiber probe's
// `accessibilityRole == null` test, used by the native-fallback groundtruth.
const AT_ROLES = {
  button: 1, link: 1, menuitem: 1, tab: 1, checkbox: 1, switch: 1,
  radio: 1, slider: 1, menu: 1, textfield: 1, listitem: 1,
};
function exposesAtRole(role) { return !!AT_ROLES[role]; }

// Does the element actually carry a press affordance natively? On Android RN an
// operable Pressable surfaces clickable="true"; a real <Button> widget is also
// clickable. We require the native clickable flag (not merely a tappable ROLE)
// so a decorative element that merely normalized to `button` by class never
// counts, and only genuinely pointer-operable nodes become gap candidates.
function isPointerOperable(get) {
  return get('clickable') === 'true' || get('long-clickable') === 'true';
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

// A stable, locale-invariant key for an oracle finding (overflow / content-bug),
// in reproit's selector grammar so it lines up with the elements list and the
// `key:<id>` replay selectors. id-bearing nodes are addressed by their developer
// id; others by their canonical role + document-order index (out.roleSeen[role]
// already counts this exactly). NEVER the visible text, so a translated string
// does not change a finding's identity (matches the web runner's keyOf intent).
function oracleKeyOf(id, role, roleIndex) {
  if (id != null) return 'key:' + id;
  return 'role:' + role + '#' + roleIndex;
}

// OVERFLOW per-element measurement (deterministic, structural). Two signals over
// the page-source geometry the runner already reads, mirroring the web runner's
// SCROLL/SPILL (CLIP needs a content-vs-box scrollWidth the page source does not
// expose, so it is intentionally omitted on native):
//   - SPILL: this element's frame escapes its PARENT's frame (right/bottom/left/
//     top) by more than OVERFLOW_TOL px: it overlaps / spills out of its
//     container rather than being contained. The dominant native symptom of a
//     long i18n string in a fixed-size button (the child grows past the parent).
//   - VIEWPORT: the element's frame extends past the screen frame on the right /
//     bottom by more than the tolerance: content is pushed off-screen.
// TOLERANCE: a fixed integer px floor so sub-pixel rounding never false-flags; a
// real overflow exceeds it by many px. Returns an array of {key, kind, by}.
const OVERFLOW_TOL = 2;
function overflowOf(key, rect, parentRect, screenRect) {
  const out = [];
  if (!rect) return out;
  if (parentRect) {
    const over = Math.max(
      rect.r - parentRect.r, parentRect.l - rect.l,
      rect.b - parentRect.b, parentRect.t - rect.t,
    );
    if (over > OVERFLOW_TOL) out.push({ key, kind: 'spill', by: over });
  }
  if (screenRect) {
    const off = Math.max(rect.r - screenRect.r, rect.b - screenRect.b);
    if (off > OVERFLOW_TOL) out.push({ key, kind: 'viewport', by: off });
  }
  return out;
}

// Build the canonical Node tree from a parsed XML element subtree. Invisible
// elements (visible="false") are skipped but their visible descendants are
// hoisted, matching the SDKs. The display labels / elements list are collected
// along the way. Returns an array of canonical Node children. `parentRect` is
// the on-screen frame of the nearest visible ancestor (for the overflow SPILL
// test); null at the root.
function buildNodes(xmlEl, out, parentRect) {
  const nodes = [];
  for (const child of xmlEl.children) {
    appendNode(child, out, nodes, parentRect);
  }
  return nodes;
}
function appendNode(xmlEl, out, into, parentRect) {
  const attrs = xmlEl.attrs;
  const get = (name) => (attrs[name] != null ? attrs[name] : '');
  if (get('visible') === 'false') {
    // hoist visible descendants of an invisible wrapper (keep the same parent
    // frame: an invisible wrapper has no contributing geometry of its own)
    for (const child of xmlEl.children) appendNode(child, out, into, parentRect);
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
  // On-screen frame (page-source geometry), for the overflow oracle + as the
  // parent frame passed to children. Null when no geometry is exposed.
  const rect = rectOfEl(get);
  const okey = oracleKeyOf(id, role, myRoleIndex);

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

  // OVERFLOW oracle (deterministic, structural): does this NON-transient element's
  // frame spill out of its container, or off the screen? Pure geometry from the
  // page source (no pixels, no second query), so it reproduces byte-for-byte on
  // replay. Transient nodes (toast/spinner, returned above) never contribute.
  if (rect) {
    for (const item of overflowOf(okey, rect, parentRect, out.screenRect)) out.overflows.push(item);
  }

  // CONTENT-BUG oracle (deterministic, label scan): a rendered label carrying a
  // stringify/template artifact ([object Object], whole-word undefined/null/NaN,
  // an unrendered {{..}}/${..}). Scans the displayed text the runner already
  // gathers; addressed by the same stable locale-invariant key, so a clean app
  // stays silent. Skips secure fields (never read a password).
  const dtext = displayTextOfEl(tag, get, role);
  const cbReason = contentBugReason(dtext);
  if (cbReason) out.contentBugs.push({ key: okey, reason: cbReason, text: dtext });

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

  // NATIVE-FALLBACK GROUNDTRUTH candidate (graph-1 from graph 2). The fiber probe
  // (graph 1) is the primary operability oracle, but the uiautomator2 driver has
  // NO JS transport into the RN runtime on a real device, so on Android it yields
  // nothing. The native a11y tree the runner already reads ALREADY encodes the
  // gap: a finger-operable Pressable that exposed an accessibilityRole renders as
  // an android.widget.Button (role `button`); one that did NOT (or set
  // accessible={false}) renders as a bare android.view.ViewGroup (role `group`).
  // So we collect every pointer-operable element that carries a STABLE id (the
  // join key the developer can address + fix) and record whether it exposes a
  // real AT role / name. The id requirement also filters dev-build chrome (the
  // RN "Open debugger" warning bubble is clickable but id-less). When the fiber
  // probe is empty, groundtruthFromNative() turns these into the same elements
  // list the engine parses: role-less operable -> no_role (+ pointer_only).
  if (id != null && isPointerOperable(get)) {
    out.nativeCandidates.push({
      id,
      rolePresent: exposesAtRole(role),
      namePresent: !!name,
    });
  }

  node.children = buildNodes(xmlEl, out, rect || parentRect);
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
    // nativeCandidates: pointer-operable, id-bearing elements from the native
    // a11y tree, with whether each exposes a real AT role/name. Feeds the
    // native-fallback groundtruth when the JS fiber probe is unavailable.
    nativeCandidates: [],
    // overflows / contentBugs: oracle findings accumulated during the tree walk
    // (raw tuples; reduced + sorted below). screenRect: the device frame the
    // overflow VIEWPORT test clips against (the top application/window element's
    // frame), null when no geometry is exposed.
    overflows: [], contentBugs: [], screenRect: null,
  };
  // The top application/window element's frame is the screen frame the overflow
  // VIEWPORT signal clips against (an element pushed past it is off-screen).
  out.screenRect = (() => {
    const top = xmlRoot.children[0];
    return top ? rectOfEl((n) => (top.attrs[n] != null ? top.attrs[n] : '')) : null;
  })();
  // The canonical root is a single `screen` node; the parsed app subtree hangs
  // under it (parallels the SDKs forcing the root role to "screen"). parentRect
  // starts null at the app root (the screen frame is the VIEWPORT reference, not
  // a SPILL container, so the topmost app element never self-spills).
  const screen = { role: 'screen', children: buildNodes(xmlRoot, out, null) };
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
    nativeCandidates: out.nativeCandidates,
    // Reduced + sorted oracle items (byte-identical shape to the web runner / the
    // Rust map.rs parser), ready to emit as EXPLORE:OVERFLOW / EXPLORE:CONTENTBUG.
    overflows: overflowItems(out.overflows),
    contentBugs: contentBugItems(out.contentBugs),
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

// ====================================================================
//  HANG / JANK / LEAK ORACLES (mirror the web runner's marker contract)
//
//  The Rust core parses (crates/reproit/src/model/map.rs, modes/soak.rs), shared
//  with the web/Flutter runners and NOT changed here:
//    EXPLORE:HANG  {"from","action","bucket"[,"count"]}  per-transition freeze
//    EXPLORE:JANK  {"from","action","bucket"[,"count"]}  per-transition stall
//    MEMORY:SAMPLE {"t_ms","heap_used"}                  heap-vs-time soak series
//  The marker carries the coarse BUCKET, not a raw ms / byte read, so the finding
//  id is deterministic for a fixed seed/replay even though the underlying timing
//  jitters. Floors are far from any real fixture so jitter can't flip a verdict.
//
//  PLATFORM COVERAGE:
//    HANG      both. Deterministic wall-clock watchdog around tap/back; Android
//              optionally confirms with the ANR trace ("ANR in <pkg>").
//    JANK      ANDROID ONLY, via `dumpsys gfxinfo <pkg>` framestats. iOS is a
//              DOCUMENTED GAP: no per-frame trace is available over the Appium /
//              XCUITest session. The only iOS frame source is xctrace /
//              Instruments, which runs OUT-OF-BAND (a separate process attached to
//              the device), not through the WebDriver session, so it cannot be
//              keyed to a (from, action) transition deterministically here, and a
//              session-level CA-commit / frame-timing capture from xctrace cannot
//              be bucketed without flake (no clean, false-positive-free floor that
//              maps to a deterministic finding id over a noisy sim). We do NOT fake
//              an iOS jank signal: drainGfxinfoJank returns null on iOS, and no
//              iOS jank marker is ever emitted. (APIs considered + rejected for
//              cleanliness: `xcrun xctrace record --template 'Core Animation'` /
//              `'Time Profiler'`; Appium `mobile: startPerfRecord` (Android-only);
//              `driver.getPerformanceData` (Android-only).)
//    LEAK      BOTH, COARSELY, under --soak.
//              ANDROID: `dumpsys meminfo <pkg>` retained PSS (sampleAndroidHeap).
//              iOS: process RESIDENT SET SIZE (footprint) of the booted-sim app,
//              sampled per replay cycle (sampleIosHeap). The XCUITest session
//              exposes no heap/footprint readout (getPerformanceData is Android-
//              only; there is no `mobile: shell` on iOS), BUT a sim app is a HOST
//              macOS process, so the runner resolves its pid deterministically
//              from `simctl spawn booted launchctl list` (the single
//              `UIKitApplication:<bundleId>[...]` row) and reads RSS with host
//              `ps -o rss= -p <pid>`. This is a COARSE, SESSION-LEVEL signal
//              (whole-process RSS, not the JS heap, attributed to the soak run not
//              a transition), but it is REAL and DETERMINISTIC: a true leak grows
//              RSS monotonically with cycle count. It is gated HARD on a uniquely
//              resolved pid (exactly one matching app row + a single host pid);
//              when the bundleId is unset, the row is ambiguous, simctl/ps are
//              unavailable, or the pid does not resolve to one host process, it
//              stays SILENT (emits nothing) rather than risk a wrong-process read.
//              So iOS leak is DONE(coarse); iOS jank remains the documented gap.
//
//  The Android shell path (gfxinfo / meminfo / dumpsys / logcat) goes through the
//  Appium `mobile: shell` extension, which requires the server to run with
//  relaxed security (`appium:relaxedSecurity` / `--relaxed-security`). When that
//  channel is absent every shell read returns null and the oracle degrades to
//  silence (HANG via wall-clock still works), never a false positive.
// ====================================================================

// Whether the session targets Android (a `mobile: shell` / adb path exists for
// the gfxinfo jank + meminfo leak probes) vs iOS (no such path: documented gap).
function isAndroid() {
  const p = (CAPS['appium:platformName'] || CAPS.platformName || CAPS['appium:automationName'] || CAPS.automationName || '').toLowerCase();
  if (p.includes('android') || p.includes('uiautomator')) return true;
  if (p.includes('ios') || p.includes('xcuitest')) return false;
  // Fall back to the presence of an Android-only cap (appPackage/appActivity).
  return !!(CAPS['appium:appPackage'] || CAPS.appPackage || CAPS['appium:appActivity'] || CAPS.appActivity);
}
function androidPkg() {
  return CAPS['appium:appPackage'] || CAPS.appPackage || targetAppId() || '';
}

// HANG watchdog (deterministic, wall-clock). Wraps a tap+observe; we time the
// action with a monotonic clock and classify the BLOCKED wall time into the same
// coarse floors the web runner uses, so a slow handler that froze the UI is a
// HANG regardless of which platform it ran on. The floors are far apart so timing
// jitter never flips the verdict. We do NOT emit a JANK bucket from wall-clock
// (a sub-second stall is indistinguishable from normal Appium round-trip latency
// over a real device, so it would false-positive); wall-clock yields HANG only.
// Per-frame JANK on Android comes from gfxinfo instead (see jankFromGfxinfo).
const HANG_FLOOR_MS = 2000;
function hangBucket(ms) {
  return ms >= HANG_FLOOR_MS ? HANG_FLOOR_MS : null;
}

// Optionally CONFIRM an Android hang with the system ANR trace. `dumpsys activity
// processes` / logcat surface "ANR in <pkg>" when the watchdog killed the main
// thread; when present it upgrades a wall-clock hang from "slow" to a real freeze.
// Best-effort: a session without the shell path simply skips confirmation (the
// wall-clock floor still stands). Never throws.
async function androidAnrSeen(driver, pkg) {
  if (!isAndroid() || !pkg) return false;
  const out = await mobileShell(driver, 'dumpsys', ['activity', 'processes']);
  if (out && out.includes('ANR in ' + pkg)) return true;
  const log = await mobileShell(driver, 'logcat', ['-d', '-t', '200']);
  return !!(log && log.includes('ANR in ' + pkg));
}

// Run an adb shell command over the Appium `mobile: shell` extension (requires
// the server-side `--relaxed-security` / `appium:relaxedSecurity`). Returns the
// trimmed stdout, or null when the channel is unavailable / errored. Pure read;
// the gfxinfo/meminfo/dumpsys commands below never mutate the app. Never throws.
async function mobileShell(driver, command, args) {
  try {
    if (typeof driver.execute !== 'function') return null;
    const r = await driver.execute('mobile: shell', { command, args: args || [] });
    if (r == null) return null;
    if (typeof r === 'string') return r;
    if (typeof r === 'object' && typeof r.stdout === 'string') return r.stdout;
    return String(r);
  } catch { return null; }
}

// JANK from Android gfxinfo framestats (deterministic, bucketed). `dumpsys
// gfxinfo <pkg>` reports a "Janky frames:" summary line: "<n> (<pct>%)". We key
// the verdict off the PERCENTAGE of janky frames crossing a coarse floor, not a
// raw frame-time, so the same render workload yields the same bucket on replay.
// A clean render stays well under the floor (0-a few %); a dropped-frame storm is
// tens of percent. Returns { bucket, count } or null. The bucket is the floor
// (the deterministic detail the marker carries), count is the janky-frame count.
const JANK_PCT_FLOOR = 30;          // >= 30% janky frames over the window -> jank
const JANK_BUCKET = JANK_PCT_FLOOR; // coarse, well-separated detail for the marker
function jankFromGfxinfo(text) {
  if (!text) return null;
  // "Janky frames: 42 (35.00%)" — read the count and the percentage.
  const m = text.match(/Janky frames:\s*(\d+)\s*\(([\d.]+)%\)/);
  if (!m) return null;
  const count = parseInt(m[1], 10);
  const pct = parseFloat(m[2]);
  if (!Number.isFinite(pct) || pct < JANK_PCT_FLOOR) return null;
  return { bucket: JANK_BUCKET, count: Number.isFinite(count) ? count : 0 };
}
// Reset the gfxinfo framestats window so the NEXT read reflects only the frames
// rendered by the action under test (otherwise jank accumulates across the run
// and every later action inherits it -> not per-transition). Best-effort.
async function resetGfxinfo(driver, pkg) {
  if (!isAndroid() || !pkg) return;
  await mobileShell(driver, 'dumpsys', ['gfxinfo', pkg, 'reset']);
}
// Read + classify the Android render jank for the action that just ran. Null on
// iOS / no shell channel / clean render.
async function drainGfxinfoJank(driver, pkg) {
  if (!isAndroid() || !pkg) return null;
  const text = await mobileShell(driver, 'dumpsys', ['gfxinfo', pkg]);
  return jankFromGfxinfo(text);
}

// LEAK sample from Android meminfo (deterministic, retained PSS). `dumpsys
// meminfo <pkg>` reports a "TOTAL" / "TOTAL PSS:" line in KB; PSS is the app's
// proportional set size (retained memory), the Android equivalent of the web
// runner's post-GC v8 used-heap read. We emit it as the SAME MEMORY:SAMPLE
// marker the soak oracle reads (heap_used in BYTES, so KB*1024). A true leak
// grows monotonically with the soak cycle count; a resource-neutral cycle stays
// flat. Returns the bytes, or null when meminfo is unavailable.
function pssFromMeminfo(text) {
  if (!text) return null;
  // Newer: "TOTAL PSS:   123456 ..."; older: a "TOTAL" row whose first number is
  // the total PSS in KB. Prefer the explicit label, fall back to the TOTAL row.
  let m = text.match(/TOTAL PSS:\s*(\d+)/);
  if (!m) m = text.match(/\n\s*TOTAL\s+(\d+)/);
  if (!m) return null;
  const kb = parseInt(m[1], 10);
  if (!Number.isFinite(kb)) return null;
  return kb * 1024;
}
async function sampleAndroidHeap(driver, pkg, tMs) {
  if (!isAndroid() || !pkg) return;
  const text = await mobileShell(driver, 'dumpsys', ['meminfo', pkg]);
  const used = pssFromMeminfo(text);
  if (used == null) return;
  log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: used }));
}

// ---- iOS LEAK: session-level process RSS of the booted-sim app (COARSE) -------
// The XCUITest session exposes no heap/footprint readout, but an iOS-simulator app
// is a HOST macOS process. We resolve its pid deterministically from the simulator
// and read its resident set size (footprint) with host `ps`, giving a real, coarse
// MEMORY:SAMPLE series the soak oracle reads. A true leak grows RSS monotonically
// with cycle count; the floor in soak.rs (262KB/cycle) is far above GC noise, so a
// resource-neutral cycle is not a false leak. Gated HARD: any ambiguity -> silent.

// The target app's iOS bundle identifier (the join key for the launchctl row).
function iosBundleId() {
  return CAPS['appium:bundleId'] || CAPS.bundleId || '';
}

// Run a host command (simctl / ps) and return trimmed stdout, or null. Pure read;
// never mutates the device or the app. Never throws (a missing binary, a non-zero
// exit, or any spawn error yields null, so the sampler degrades to silence).
function hostExec(cmd, args) {
  try {
    const out = execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'], timeout: 5000 });
    return out == null ? null : String(out);
  } catch { return null; }
}

// Resolve the booted-sim app's HOST pid from `simctl spawn booted launchctl list`.
// Each running app is one row "pid status UIKitApplication:<bundleId>[token]...".
// We require EXACTLY ONE row whose UIKitApplication bundleId equals the target and
// a single numeric pid; zero or multiple matches -> null (we never guess). The pid
// is a real host pid (sim apps are host processes), readable with `ps`. iOS only.
function resolveIosAppPid() {
  if (isAndroid()) return null;
  const bundle = iosBundleId();
  if (!bundle) return null;
  const out = hostExec('xcrun', ['simctl', 'spawn', 'booted', 'launchctl', 'list']);
  if (out == null) return null;
  const pids = [];
  for (const line of out.split('\n')) {
    // Match "<pid>\t<status>\tUIKitApplication:<bundleId>[..." anchoring the
    // bundleId to a '[' so a prefix bundle (com.x vs com.x.y) never cross-matches.
    const m = line.match(/^(\d+)\s+\S+\s+UIKitApplication:([^\[]+)\[/);
    if (!m) continue;
    if (m[2] !== bundle) continue;
    pids.push(parseInt(m[1], 10));
  }
  if (pids.length !== 1 || !Number.isFinite(pids[0]) || pids[0] <= 0) return null;
  return pids[0];
}

// Read a host pid's resident set size (KB, from `ps -o rss=`) as BYTES, or null.
function hostRssBytes(pid) {
  if (!(pid > 0)) return null;
  const out = hostExec('ps', ['-o', 'rss=', '-p', String(pid)]);
  if (out == null) return null;
  const kb = parseInt(out.trim(), 10);
  if (!Number.isFinite(kb) || kb <= 0) return null;
  return kb * 1024;
}

// Sample the iOS-sim app's process RSS and emit the SAME MEMORY:SAMPLE marker the
// soak oracle reads (heap_used in BYTES). No-op on Android / when the pid cannot be
// uniquely resolved / when ps is unavailable -> stays silent, never false-positive.
// `pidRef` is a one-shot cache ({ pid }) so the pid is resolved once per soak.
function sampleIosHeap(pidRef, tMs) {
  if (isAndroid()) return;
  if (pidRef.pid == null) pidRef.pid = resolveIosAppPid();
  if (!(pidRef.pid > 0)) return;
  const used = hostRssBytes(pidRef.pid);
  if (used == null) return;
  log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: used }));
}

export { jankFromGfxinfo, pssFromMeminfo, overflowItems, contentBugItems, contentBugReason, rectOfEl, overflowOf, hangBucket };

// ====================================================================
//  OPERABILITY / ACCESSIBILITY GROUND TRUTH (the EXPLORE:GROUNDTRUTH marker)
//
//  Appium's page source (above) is GRAPH 2: the accessibility tree, the subset
//  of the UI a screen-reader / keyboard user reaches. It is structurally blind
//  to a control that has an onPress but exposes NO a11y role/label, which is
//  exactly the WCAG operability gap reproit hunts (docs/operability-graph.md).
//
//  GRAPH 1 (ground truth) for React Native comes from the JS side: React's
//  FIBER TREE knows every node that has a press/gesture handler
//  (onPress/onPressIn/onLongPress, Pressable, Touchable*, PanResponder,
//  Gesture.Tap) AND the a11y props the developer exported (accessible,
//  accessibilityRole, accessibilityLabel, nativeID/testID). A
//  `<TouchableOpacity onPress>` with accessible={false} / no role is operable by
//  finger but invisible to AT: a gap.
//
//  The engine rule (crates/reproit/src/model/map.rs gaps_from_groundtruth):
//    operable && (rolePresent==false) -> no_role (WCAG 4.1.2)
//  We set operable = has-press-handler, rolePresent = accessibilityRole present,
//  namePresent = accessibilityLabel present. We do NOT assert keyboardActivatable
//  / inTabOrder on RN (no hardware-keyboard tab model on a touch surface), so
//  those default true in the engine and never spuriously flag.
//
//  HOW WE READ THE FIBER (and its constraints):
//    - Needs a DEV / Hermes build that exposes the React DevTools global hook
//      `__REACT_DEVTOOLS_GLOBAL_HOOK__` (present in dev; stripped in release),
//      or an app that registered `global.__REPROIT_FIBER__`. A release build has
//      neither, so the probe is a NO-OP there (a11y-only mapping, unchanged).
//    - The JS bridge runs IN the RN JS runtime. Appium can reach it on Hermes
//      via the `mobile: executeScript` / inspector channel; the exact transport
//      is environment-specific, so emitGroundtruth tries the documented hooks
//      and degrades gracefully (logs why it could not run, never throws).
//    - The JOIN to graph 2 is by nativeID / testID: the fiber record carries the
//      node's nativeID/testID and that is the same stable id idOfEl() pulls from
//      the page source, so the runtime `key:<id>` selector lines up.
// ====================================================================

// The bridge SOURCE that runs inside the RN JS runtime. It is a self-contained
// IIFE-returning function body (no closure over runner state) so it can be
// stringified and injected over whatever JS channel the build exposes. It walks
// every mounted fiber root and returns a flat array of records:
//   { id, hasPress, role, label, accessible }
// id = nativeID || testID (the join key; null if neither). hasPress = any press/
// gesture handler prop present. role/label = accessibilityRole/accessibilityLabel
// (null when absent). Pure read; it mutates nothing in the app.
const FIBER_PROBE_SRC = `(function reproitFiberProbe() {
  var records = [];
  var PRESS_PROPS = ['onPress','onPressIn','onPressOut','onLongPress','onClick'];
  function hasPressProp(props) {
    if (!props) return false;
    for (var i = 0; i < PRESS_PROPS.length; i++) {
      if (typeof props[PRESS_PROPS[i]] === 'function') return true;
    }
    // PanResponder spreads its handlers onto props (onStartShouldSetResponder /
    // onResponderRelease); a Gesture.Tap detector exposes an onGestureEvent.
    if (typeof props.onResponderRelease === 'function') return true;
    if (typeof props.onStartShouldSetResponder === 'function') return true;
    if (typeof props.onStartShouldSetResponderCapture === 'function') return true;
    if (typeof props.onGestureEvent === 'function') return true;
    return false;
  }
  // A composite type whose NAME implies a press affordance (Pressable,
  // TouchableOpacity, TouchableHighlight, TouchableWithoutFeedback, Button).
  function pressByType(type) {
    if (!type) return false;
    var name = typeof type === 'string' ? type
      : (type.displayName || type.name || '');
    return /Pressable|Touchable|^Button$|^TouchableOpacity$/.test(name);
  }
  function recordFiber(fiber) {
    if (!fiber) return;
    var props = fiber.memoizedProps || (fiber.pendingProps) || null;
    if (props) {
      var id = props.nativeID != null ? props.nativeID
        : (props.testID != null ? props.testID : null);
      var hasPress = hasPressProp(props) || pressByType(fiber.type);
      // Only emit a record for a node that is either operable OR carries a
      // join id, so the host side has something to reason about. A bare layout
      // View with neither is noise.
      if (hasPress || id != null) {
        records.push({
          id: id != null ? String(id) : null,
          hasPress: !!hasPress,
          role: props.accessibilityRole != null ? String(props.accessibilityRole) : null,
          label: props.accessibilityLabel != null ? String(props.accessibilityLabel) : null,
          accessible: props.accessible === undefined ? null : !!props.accessible,
        });
      }
    }
    // Depth-first over the fiber child/sibling links.
    var child = fiber.child;
    while (child) { recordFiber(child); child = child.sibling; }
  }
  try {
    var hook = (typeof global !== 'undefined' && global.__REACT_DEVTOOLS_GLOBAL_HOOK__) ||
      (typeof window !== 'undefined' && window.__REACT_DEVTOOLS_GLOBAL_HOOK__) || null;
    // App-registered explicit hook wins (a build can export its fiber roots).
    var explicit = (typeof global !== 'undefined' && global.__REPROIT_FIBER__) || null;
    if (explicit && typeof explicit.getRoots === 'function') {
      var roots = explicit.getRoots() || [];
      for (var r = 0; r < roots.length; r++) recordFiber(roots[r]);
    } else if (hook && hook.renderers) {
      // DevTools hook: getFiberRoots(rendererId) -> Set of FiberRoot, each with
      // a .current pointer to the root fiber.
      var ids = [];
      try { hook.renderers.forEach(function (_v, k) { ids.push(k); }); } catch (e) {}
      for (var j = 0; j < ids.length; j++) {
        var set = hook.getFiberRoots ? hook.getFiberRoots(ids[j]) : null;
        if (!set) continue;
        set.forEach(function (root) { if (root && root.current) recordFiber(root.current); });
      }
    } else {
      return { ok: false, reason: 'no-fiber-hook', records: [] };
    }
  } catch (e) {
    return { ok: false, reason: String(e && e.message ? e.message : e), records: [] };
  }
  return { ok: true, records: records };
})()`;

// HOST-SIDE pure reducer: turn the raw fiber records the bridge returned into
// the EXPLORE:GROUNDTRUTH `elements` list. Pure + deterministic (sorted by id),
// so it is unit-testable in Node WITHOUT a device. The engine rule only consults
// `operable` + `a11y.{rolePresent,namePresent,...}`:
//   operable      = the fiber node has a press/gesture handler.
//   rolePresent   = accessibilityRole was set (else AT sees a generic node).
//   namePresent   = accessibilityLabel was set.
// We DON'T claim keyboardActivatable / inTabOrder (no keyboard tab model on a
// touch surface), so the engine defaults them true and never false-flags those.
// `nativeIds` is the set of stable ids present in the native page source; when an
// operable fiber node's id is NOT among them, the native a11y tree never exposed
// it at all -> rolePresent=false (the strongest no-role signal: invisible to AT).
function groundtruthFromFiber(records, nativeIds) {
  const native = nativeIds instanceof Set ? nativeIds : new Set(nativeIds || []);
  const els = [];
  let idx = 0;
  for (const rec of records || []) {
    if (!rec || !rec.hasPress) continue; // only operable nodes are gap candidates
    // Join key: the fiber node's nativeID/testID, addressed in reproit's `key:`
    // grammar so it lines up with the runtime selector (idOfEl pulls the same id).
    // A node with no id can't be joined or fixed precisely; address it by a
    // synthetic structural index so the count is still reported.
    const sel = rec.id != null ? 'key:' + rec.id : 'fiber:press#' + idx;
    // accessible={false} hides the node from AT entirely: treat as no role AND no
    // name regardless of what role/label strings were set (they're inert then).
    const hidden = rec.accessible === false;
    // An operable node whose id never appeared in the native a11y tree was not
    // exposed to AT at all -> no role.
    const inNative = rec.id != null && native.has(rec.id);
    const rolePresent = !hidden && rec.role != null && (rec.id == null || inNative || native.size === 0);
    const namePresent = !hidden && rec.label != null;
    els.push({
      id: sel,
      operable: true,
      gestureKind: 'tap',
      a11y: {
        rolePresent,
        namePresent,
        // focusable / inTabOrder / keyboardActivatable: not asserted on a touch
        // surface; omitted so the engine defaults them true (no spurious flag).
      },
    });
    idx++;
  }
  // Deterministic order: by selector.
  els.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return els;
}

// Run the fiber probe over the RN JS runtime and emit EXPLORE:GROUNDTRUTH for
// the current state. Best-effort: a release build (no DevTools hook) or a
// transport that can't reach JS yields an empty-elements marker (still emitted,
// so the engine records "no gaps observed" rather than nothing). `nativeIds` is
// the set of stable ids the page-source snapshot saw, for the graph-1<->graph-2
// join. Never throws.
async function emitGroundtruth(driver, sig, nativeIds, nativeCandidates) {
  let result = null;
  // Appium exposes the RN JS runtime over `mobile: executeScript` on Hermes /
  // debug builds. webdriverio surfaces it as executeScript(script, args) or the
  // legacy execute(script). We try the documented entry points in order and
  // accept the first that returns our { ok, records } shape.
  const tryRun = async (fn) => {
    try {
      const r = await fn();
      if (r && typeof r === 'object' && Array.isArray(r.records)) return r;
    } catch (e) { /* transport unavailable: fall through */ }
    return null;
  };
  if (typeof driver.executeScript === 'function') {
    result = await tryRun(() => driver.executeScript(FIBER_PROBE_SRC, []));
  }
  if (!result && typeof driver.execute === 'function') {
    result = await tryRun(() => driver.execute(FIBER_PROBE_SRC));
  }
  // PRIMARY: a successful fiber probe (Hermes/dev build with the DevTools hook)
  // with at least one operable record wins. It is the true graph-1 oracle: it
  // sees a press handler even on a node the native a11y tree never exposed.
  if (result && result.ok) {
    const elements = groundtruthFromFiber(result.records, nativeIds);
    if (elements.length > 0) {
      log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap: false, elements }));
      return;
    }
  }

  // FALLBACK: the fiber probe was unavailable (no JS channel: uiautomator2 on a
  // real device) or yielded no operable record. Derive groundtruth from the
  // native a11y tree instead: a pointer-operable, id-bearing element that exposes
  // a generic/non-button role (android.view.ViewGroup, role `group`, no AT role)
  // is the WCAG 4.1.2 no_role (+ pointer_only) gap; one that exposes a real role
  // is clean. This keeps RN operability working live where the fiber path can't.
  const nativeEls = groundtruthFromNative(nativeCandidates);
  if (nativeEls.length > 0) {
    const reason = result && result.ok ? 'fiber-empty' : (result && result.reason ? result.reason : 'no-js-channel');
    log('JOURNEY[a] step: groundtruth from native a11y tree (' + reason + '; ' + nativeEls.length + ' operable)');
    log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap: false, elements: nativeEls }));
    return;
  }

  // Neither path produced anything: emit an empty ground-truth so the engine sees
  // the state was probed (no false gaps), and log why so the operator knows.
  const reason = result && result.reason ? result.reason : 'no-js-channel';
  log('JOURNEY[a] step: groundtruth probe skipped (' + reason + '; no fiber + no native operable)');
  log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap: false, elements: [] }));
}

export { groundtruthFromFiber };

// HOST-SIDE pure reducer for the NATIVE FALLBACK: turn the pointer-operable,
// id-bearing native-tree candidates (snapshot.nativeCandidates) into the same
// EXPLORE:GROUNDTRUTH `elements` list groundtruthFromFiber produces, for the case
// where the JS fiber probe could not run (uiautomator2 has no JS channel into the
// RN runtime on a real device). Each candidate is operable by pointer; we report
// `rolePresent` from whether the native node exposed a real AT role (it rendered
// as an android.widget.Button vs a bare android.view.ViewGroup) and `namePresent`
// from its accessible name. A role-less operable element is the WCAG 4.1.2 case:
// the engine counts it as a no_role gap, and because it is operable ONLY by
// pointer with no exposed semantics we also assert keyboardActivatable=false so
// the engine additionally counts it pointer_only. A candidate that DOES expose a
// real role is reported clean (all dims true) and is not a gap. Pure +
// deterministic (sorted by selector), so it is unit-testable without a device.
function groundtruthFromNative(candidates) {
  const els = [];
  for (const c of candidates || []) {
    if (!c || c.id == null) continue;
    const rolePresent = !!c.rolePresent;
    const namePresent = !!c.namePresent;
    els.push({
      id: 'key:' + c.id,
      operable: true,
      gestureKind: 'tap',
      a11y: {
        rolePresent,
        namePresent,
        // A role-less, pointer-operable native node is reachable ONLY by finger:
        // it carries no exposed semantics for a keyboard/switch user to activate,
        // so it is pointer_only. A node that exposes a real AT role is keyboard-
        // activatable like any focusable control; report it clean.
        keyboardActivatable: rolePresent,
        inTabOrder: rolePresent,
        focusable: rolePresent,
      },
    });
  }
  els.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return els;
}

export { groundtruthFromNative };

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
      // GRAPH 1 vs GRAPH 2: once per newly-seen state, probe the React fiber
      // tree for press handlers + exported a11y props and emit EXPLORE:GROUNDTRUTH
      // so the engine can diff the operable set against the a11y tree. Joined to
      // the native page source by the stable ids it just saw. Best-effort.
      const nativeIds = new Set(snap.elements.map((e) => e.key).filter((k) => k != null));
      await emitGroundtruth(driver, snap.sig, nativeIds, snap.nativeCandidates);
      // OVERFLOW for this newly-seen state, keyed by the SAME sig so the oracle
      // attributes the finding to this state (last write wins on the Rust side).
      // Pure structural geometry from the page source, so it reproduces on
      // replay; only emitted when something actually overflows (clean layout =>
      // silent, no marker, no finding).
      if (snap.overflows.length) {
        log('EXPLORE:OVERFLOW ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: snap.overflows }));
      }
      // CONTENT-BUG for this newly-seen state, keyed by the SAME sig. Pure label
      // scan (no pixels, no timing), so it reproduces on replay; only emitted
      // when a broken-content artifact is actually rendered (clean app stays
      // silent).
      if (snap.contentBugs.length) {
        log('EXPLORE:CONTENTBUG ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: snap.contentBugs }));
      }
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

  // LEAK sampler: in REPLAY mode (the `--soak` tier writes {"replay":[...]}),
  // sample memory once at the start and after every action so the Rust soak oracle
  // gets a heap-vs-time series to read the slope from. Off outside replay (a plain
  // fuzz walk is not a soak). ANDROID samples retained PSS (dumpsys meminfo); iOS
  // samples the sim app's process RSS (a coarse, session-level signal resolved over
  // simctl+ps, gated hard on a unique pid). t0 anchors t_ms to walk start; iosPid
  // is the one-shot pid cache the iOS sampler resolves lazily on first use.
  const pkg = androidPkg();
  const iosPid = { pid: null };
  const sampleHeap = async (tMs) => { await sampleAndroidHeap(driver, pkg, tMs); sampleIosHeap(iosPid, tMs); };
  const t0 = Date.now();
  if (replay) await sampleHeap(0);

  for (let actions = 0; actions < budget && stuck < 3; actions++) {
    // LEAK sampler: in replay mode, sample memory once per action (BEFORE acting,
    // so action k's sample reflects the heap after the previous action settled);
    // together with the start + final samples it forms the monotonic series the
    // soak slope is read from. No-op outside replay; per-platform inside sampleHeap.
    if (replay && actions > 0) await sampleHeap(Date.now() - t0);
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
      const tHang0 = Date.now();
      try { await driver.back(); } catch { /* ignore */ }
      await driver.pause(700);
      // HANG watchdog on the back transition (same floor + keying as the tap path).
      const hb = hangBucket((Date.now() - tHang0) - 700);
      if (hb != null) {
        const confirmed = await androidAnrSeen(driver, pkg);
        log('EXPLORE:HANG ' + JSON.stringify({ from: before, action: 'back', bucket: hb, ...(confirmed ? { anr: true } : {}) }));
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
    const sel = act.slice('tap:'.length);
    triedEdges.add(current.sig + '|' + sel);
    const before = current.sig;
    const beforeContent = current.content;
    // JANK: reset the gfxinfo framestats window so the read after this tap counts
    // only the frames this action rendered (per-transition, not run-cumulative).
    await resetGfxinfo(driver, pkg);
    // HANG: time the action's blocking wall-clock. We measure tap + settle only
    // (NOT the subsequent observe, which is a page-source round-trip whose latency
    // is unrelated to the app's responsiveness), so the floor reflects the app
    // freezing, not Appium overhead.
    const tHang0 = Date.now();
    const ok = await tap(driver, sel, current);
    if (!ok) { log('FUZZ:MISS ' + act); stuck++; continue; }
    await driver.pause(800);
    const blockedMs = (Date.now() - tHang0) - 800; // subtract the fixed settle pause
    // Crash oracle: if the target app left the foreground after this tap, the app
    // crashed (uncaught exception -> process died -> launcher).
    if (await appCrashed(driver)) { emitCrash(act); crashed = true; break; }
    // HANG watchdog: did the action block past the freeze floor? Keyed by (from,
    // action) so the Rust side attributes it to this transition and `check`
    // re-confirms it. On Android, optionally upgrade-confirm with the ANR trace.
    const hb = hangBucket(blockedMs);
    if (hb != null) {
      const confirmed = await androidAnrSeen(driver, pkg);
      log('EXPLORE:HANG ' + JSON.stringify({ from: before, action: 'tap:' + sel, bucket: hb, ...(confirmed ? { anr: true } : {}) }));
    }
    // JANK watchdog (Android only): did this transition render a dropped-frame
    // storm? Read the gfxinfo framestats window we reset above. Keyed by (from,
    // action) like HANG. iOS has no per-frame trace over XCUITest (documented gap).
    const jk = await drainGfxinfoJank(driver, pkg);
    if (jk) {
      log('EXPLORE:JANK ' + JSON.stringify({ from: before, action: 'tap:' + sel, bucket: jk.bucket, count: jk.count }));
    }
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

  // LEAK sampler: a final sample after the last action, so the series spans the
  // whole soak (start ... last action). No-op outside replay; per-platform inside.
  if (replay) await sampleHeap(Date.now() - t0);
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
