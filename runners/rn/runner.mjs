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
//   EXPLORE:CONTENTBUG {"sig":..,"items":[{key,reason,text}]} per-state, label scan
//   EXPLORE:BLANKSCREEN {"sig":..,"items":[{key:"root",w,h}]} per-state, WSOD
//   EXPLORE:SAFEAREA   {"sig":..,"items":[{key,edge,by}]}  per-state, inset geometry
//   EXPLORE:PERMISSIONWALK {"sig":..,"permission":..}  per-state, denial sweep
//   EXPLORE:BROKENASSET {"sig":..,"items":[{key,reason:"tofu",detail}]} per-state
//   EXPLORE:HANG       {"from":..,"action":..,"bucket":..} per-transition watchdog
//   EXPLORE:JANK       {"from":..,"action":..,"bucket":..,"count":..} Android gfxinfo
//   EXPLORE:WAKELOCK   {"sig":..,"items":[{tag,kind}]} Android dumpsys-power leak
//   MEMORY:SAMPLE      {"t_ms":..,"heap_used":..}  Android PSS series under --soak
// The OVERFLOW/CONTENTBUG/HANG/JANK/MEMORY markers share the EXACT contract the
// web runner emits and the Rust core already parses (model/map.rs, modes/soak.rs);
// the core is unchanged. iOS LEAK is now covered COARSELY (session-level process
// RSS sampled per replay cycle: the booted-sim app is a host process whose pid the
// runner resolves over `simctl spawn booted launchctl list`, read with host `ps`);
// see sampleIosHeap. iOS JANK stays a documented gap (no clean, non-flaky,
// sim-attributable per-frame trace exists for a simulator app: Animation Hitches
// is unsupported on the sim, Metal System Trace captures host-wide GPU work not
// the sim app, and xctrace cannot attach to an in-sim process); the exact commands
// tried and why each fails are recorded in the HANG/JANK/LEAK section.
//
// Env (set by the orchestrator's react-native runner):
//   REPROIT_APPIUM_URL    Appium server base URL (e.g. http://127.0.0.1:4723)
//   REPROIT_APPIUM_CAPS   JSON capabilities (platformName, app, deviceName, ...)
//   REPROIT_FUZZ_CONFIG   seed/budget/replay/prefix json
//
// stdout is the marker stream; the orchestrator captures it like a drive log.
//
// Runtime validation: validation/backends/run-react-native-android.sh builds a
// bundled React Native release app, drives it through this runner on Appium,
// and requires a keyed press, structural state change, and EXPLORE:EDGE. Native
// SwiftUI/iOS and Compose/Android fixtures gate the other Appium platform ids.

import { remote } from 'webdriverio';
import { readFileSync, writeFileSync, existsSync, mkdirSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { execFileSync, spawn } from 'node:child_process';

const APPIUM = process.env.REPROIT_APPIUM_URL || 'http://127.0.0.1:4723';
const CAPS = JSON.parse(process.env.REPROIT_APPIUM_CAPS || '{}');
const ACTION_BUDGET = 36;
const MAX_LABEL_LEN = 40;
// Layer-1 value-class cap (docs/signature.md "Value-state"): once a structural
// node has shown more than this many DISTINCT value-class combinations, the
// runner drops it to structural-only so an adversarial value generator cannot
// explode the graph. The oracle is stateless; the cap is purely runner-local.
const VALUE_CLASS_CAP = 8;
let causalActionIndex = 0;

async function advanceCausalAction(driver) {
  causalActionIndex += 1;
  if (isAndroid()) {
    await mobileShell(driver, 'setprop', ['debug.reproit.action', String(causalActionIndex)]);
  }
}

function log(line) { process.stdout.write(line + '\n'); }
function stageAndroidCausalBeforeLaunch(caps) {
  if (!isAndroid()) return true;
  const serial = caps['appium:udid'] || caps.udid;
  const adb = (args) => execFileSync('adb', [...(serial ? ['-s', String(serial)] : []), ...args], { stdio: 'ignore' });
  try {
    adb(['shell', 'setprop', 'debug.reproit.fuzz', '1']);
    adb(['shell', 'setprop', 'debug.reproit.action', '0']);
    if (process.env.REPROIT_CAPSULE) {
      const destination = '/data/local/tmp/reproit-capsule.json';
      adb(['push', process.env.REPROIT_CAPSULE, destination]);
      adb(['shell', 'chmod', '0644', destination]);
      adb(['shell', 'setprop', 'debug.reproit.capsule', destination]);
    } else {
      // adb/Appium drop empty shell arguments. The SDK treats this explicit,
      // shell-safe sentinel as no capsule, preventing stale replay state.
      adb(['shell', 'setprop', 'debug.reproit.capsule', '__reproit_none__']);
    }
    return true;
  } catch (_) {
    // The post-session mobile:shell path below is the fallback for remote device
    // farms where adb is intentionally unavailable on the runner host.
    return false;
  }
}
function loadFuzz() {
  const p = process.env.REPROIT_FUZZ_CONFIG;
  if (!p) return {};
  try { return JSON.parse(readFileSync(p, 'utf8')); } catch { return {}; }
}

// The multi-seed BATCH contract shared with every other runner (runners/web
// loadBatch, templates/explorer_headless.dart FuzzCfg.loadBatch, runners/linux-
// atspi.py load_batch). `reproit check` with gate.runs > 1 (and the multi-seed
// fuzz path) writes {"batch":[ <cfg>, ... ]} where each <cfg> is the single-seed
// shape ({seed, replay?, prefix?, ...}); a single run writes the bare <cfg>
// directly (no "batch" key). Returns { seeds, isBatch }; isBatch is true ONLY for
// the multi-seed shape, so the caller wraps each seed in SEED:BEGIN/SEED:END only
// then and the Rust core (fuzz.rs split_log_segments) can split the one drive log
// back into one segment per replay. WITHOUT this the runner read the {batch:..}
// object as a single fuzz config whose `replay`/`seed` were undefined, silently
// fell into a fresh EXPLORE walk, and never replayed the stored actions. As a result, a
// real crash repro re-confirmed as clean (PASS). See the replay branch in main().
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

// Layer-3 opt-in (docs/signature.md "Value-state"): read `value_nodes:`
// selectors from reproit.yaml. We avoid adding a YAML dependency: the block is
// a simple flat list of strings, so a tiny line parser is enough and keeps the
// runner dependency-free. Path precedence: REPROIT_CONFIG env, else
// ./reproit.yaml in the cwd. A missing/unparseable file yields an empty list,
// so value-state is strictly opt-in. Mirrors runners/web.
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

// The shared UTF-8 encoder for the canonical hash + V: byte-order sort. The
// descriptor and V: keys can carry non-ASCII (a localized anchor, a non-ASCII
// id, an emoji icon), so we MUST fold the UTF-8 BYTES, exactly like the Rust
// oracle's `desc.as_bytes()`. Folding UTF-16 code units silently diverged.
const REPROIT_UTF8 = new TextEncoder();

// FNV-1a over the UTF-8 BYTES of an arbitrary descriptor string. Matches the
// Rust oracle / web SDK / explorer.dart so signatures and seeds line up across
// platforms.
function fnv1a(s) {
  const bytes = REPROIT_UTF8.encode(s);
  let h = 0x811c9dc5;
  for (let i = 0; i < bytes.length; i++) { h ^= bytes[i]; h = Math.imul(h, 0x01000193) >>> 0; }
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

// Android SYSTEM-CHROME node: a view in the platform's own `android:` resource
// namespace (android:id/navigationBarBackground, android:id/statusBarBackground,
// and framework decor generally), as opposed to app content in the app package's
// namespace (com.example.app:id/...). The OS draws these to the device insets /
// screen edges, so their frame legitimately spills past the app content box. An
// overflow marker on them is pure noise about system UI the developer neither
// owns nor can fix. Excluded from OVERFLOW candidacy, mirroring the Windows
// caption-chrome exclusion. `idOfEl` strips the namespace, so we read the RAW
// resource-id here.
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

// Locale-independent credential purpose. Uses platform autofill/content-type
// metadata only; visible labels and placeholders are intentionally excluded.
function inputPurposeOfEl(tag, get, role) {
  if (role !== 'textfield') return null;
  const hint = [
    get('textContentType'), get('content-type'), get('autofillHints'),
    get('autofill-hints'), get('importantForAutofill'), get('autocomplete'),
  ].filter(Boolean).join(' ').toLowerCase();
  const type = typeOfEl(tag, get, role);
  if (hint.includes('onetimecode') || hint.includes('one-time-code') || hint.includes('smsotp')) return 'otp';
  if (hint.includes('password') || type === 'password') return 'password';
  if (hint.includes('username')) return 'username';
  if (hint.includes('email') || type === 'email') return 'email';
  if (hint.includes('phone') || hint.includes('telephone')) return 'phone';
  return null;
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
// page source so evidence and interaction checks reuse the same geometry.
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
// the web runner's reasonOf (runners/web/runner.mjs): fires ONLY on a GROUND-TRUTH
// artifact impossible to render as legitimate copy, matched on STRUCTURE (a literal
// token), never on natural language. Two classes, first match wins so a label
// carries one reason:
//   [object Object]     -> object-object       (an object coerced to a string)
//   {{ .. }} / ${ .. }  -> unrendered-template  (the binding never evaluated)
// The bare words undefined/null/NaN are NOT matched: they occur in real copy and
// code samples ("undefined behavior", a "Null Island" pin), so keying on them
// false-positived. We scan the displayed text the runner already gathers (the same
// value/text/content-desc nameOfEl reads).
// Prose guard for BOTH artifact kinds: a real leaked artifact IS the label (bare,
// or a short field-name prefix like "Price: X"); prose that merely MENTIONS the
// token ("[object Object]" or the "{{ }}" syntax) inside a sentence is legitimate
// copy. Fire only when, with the artifact(s) removed, the remainder is a SHORT
// label with no sentence structure.
function contentBugReason(text) {
  if (!text) return null;
  const dominates = (s) => s.length <= 24 && !/[.!?]/.test(s);
  if (text.includes('[object Object]')) {
    const s = text.replace(/\[object Object\]/g, ' ').replace(/\s+/g, ' ').trim();
    if (dominates(s)) return 'object-object';
  }
  if (/\{\{[^}]*\}\}/.test(text) || /\$\{[^}]*\}/.test(text)) {
    const s = text.replace(/\{\{[^}]*\}\}/g, ' ').replace(/\$\{[^}]*\}/g, ' ').replace(/\s+/g, ' ').trim();
    if (dominates(s)) return 'unrendered-template';
  }
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

// BROKEN-ASSET classifier, tofu only on native: a rendered U+FFFD, the
// replacement character an encoding failure paints as tofu. The web runner's
// other reasons (img/font) interrogate DOM subresources that do not exist in a
// native view tree, so they stay web-only and the native `reason` vocabulary is
// a strict subset of the web one (runners/web/hygiene-oracles.mjs
// brokenAssetScan). Pure substring test over the displayed text the runner
// already gathers (never a password: displayTextOfEl blanks secure fields), so
// ordinary text never trips it and the control stays silent when clean.
function tofuReason(text) {
  return text && text.includes('�') ? 'tofu' : null;
}

// Provenance ledger for the broken-asset (tofu) oracle: every value the fuzzer
// types is recorded so a fuzzer-injected U+FFFD (an emoji / RTL probe the app
// echoes back) is not mistaken for an app encoding bug. Mirrors the web runner's
// brokenAssetScan provenance guard. Native RN has no <img>/favicon subresources,
// so tofu is the only broken-asset signal and the only one needing provenance.
const INJECTED_VALUES = new Set();
// A reflected fuzzer value (a probe the app echoes back) is not the app's own
// content: shared by the tofu (broken-asset) AND content-bug oracles. Native RN
// renders reflected text intact (no HTML parsing), so the direct substring test in
// both directions is sufficient (no artifact-fragment handling needed as on web).
function fromFuzzInjection(text) {
  const n = String(text == null ? '' : text).toLowerCase();
  if (!n) return false;
  for (const raw of INJECTED_VALUES) {
    const v = String(raw).toLowerCase();
    if (!v) continue;
    if (v.indexOf(n) !== -1 || (v.length >= 3 && n.indexOf(v) !== -1)) return true;
  }
  return false;
}

// HOST-SIDE pure reducer: collected (key, detail) tofu tuples -> the sorted
// EXPLORE:BROKENASSET `items` array (same shape the web runner emits / the Rust
// map.rs parser reads: each item is {key, reason, detail}). Deduped on key,
// sorted by key, detail trimmed + clipped to 60 chars (display detail; the
// key+reason are the stable identity). Pure + deterministic, so it is
// unit-testable in Node without a device.
function brokenAssetItems(raw) {
  const out = [];
  const seen = new Set();
  for (const it of raw || []) {
    if (!it || !it.key || !it.reason) continue;
    const k = it.key + '|' + it.reason;
    if (seen.has(k)) continue;
    seen.add(k);
    out.push({ key: it.key, reason: it.reason, detail: String(it.detail || '').trim().slice(0, 60) });
  }
  out.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  return out;
}

// HOST-SIDE pure reducer: collected tappable frames + device safe-area insets ->
// the EXPLORE:SAFEAREA `items` array (same shape as the Flutter explorer / the
// Rust map.rs parser: each item is {key, edge, by}). A tappable whose frame
// intersects an inset band is drawn under the status bar / notch (top), the home
// indicator (bottom), or a landscape notch / rounded corner (left/right), so it
// is obscured or hard to tap. `insets` is {top,bottom,left,right} in the SAME px
// space as the frames and screenRect (Android: physical px from getSystemBars();
// iOS: the XCUITest driver does not expose safe-area insets, so this is called
// with zero insets and stays silent -- use the Flutter path for iOS safe-area
// ground truth). A device with NO insets on every edge yields [] (nothing to
// collide with). An intrusion of 1px or less is flush-adjacent rounding, not a
// collision. Deduped by key|edge, capped at 20, sorted by key then edge so the
// marker is byte-identical run to run. Pure + deterministic (no device needed to
// test).
function safeAreaItems(tapRects, insets, screenRect) {
  if (!insets || !screenRect) return [];
  const top = insets.top || 0, bottom = insets.bottom || 0;
  const left = insets.left || 0, right = insets.right || 0;
  if (top <= 0 && bottom <= 0 && left <= 0 && right <= 0) return [];
  const H = screenRect.b - screenRect.t, W = screenRect.r - screenRect.l;
  const els = (tapRects || []).filter(
    (e) => e && e.rect && e.rect.r - e.rect.l > 0 && e.rect.b - e.rect.t > 0,
  );
  const out = [];
  const seen = new Set();
  const add = (key, edge, overlap) => {
    if (overlap <= 1) return; // flush-adjacent rounding, not a collision
    const dedup = key + '|' + edge;
    if (seen.has(dedup)) return;
    seen.add(dedup);
    if (out.length < 20) out.push({ key, edge, by: Math.round(overlap) });
  };
  for (const e of els) {
    const r = e.rect;
    if (top > 0) add(e.key, 'top', Math.min(r.b, screenRect.t + top) - r.t);
    if (bottom > 0) {
      const bandTop = screenRect.b - bottom;
      add(e.key, 'bottom', r.b - Math.max(r.t, bandTop));
    }
    if (left > 0) add(e.key, 'left', Math.min(r.r, screenRect.l + left) - r.l);
    if (right > 0) {
      const bandLeft = screenRect.r - right;
      add(e.key, 'right', r.r - Math.max(r.l, bandLeft));
    }
  }
  // H/W are referenced for clarity of the band model; guard against a degenerate
  // frame so a zero-size screen never manufactures a collision.
  if (!(H > 0 && W > 0)) return [];
  out.sort((x, y) => (x.key < y.key ? -1 : x.key > y.key ? 1
    : (x.edge < y.edge ? -1 : x.edge > y.edge ? 1 : 0)));
  return out;
}

// HOST-SIDE pure predicate: the BLANK-SCREEN (white-screen-of-death) verdict
// over facts the tree walk already gathered (mirrors runners/web/
// hygiene-oracles.mjs blankScreenScan). Blank iff the page source shows ZERO
// visible text labels AND ZERO tappables AND no text field / image (a
// media-only or input-only screen is NOT blank, the web scan's media check)
// while the window frame is non-zero. A driver that exposed no window geometry
// yields [] (cannot confirm the viewport, never guess-and-flag). Returns one
// [{key:"root", w, h}] record naming the scanned root and the window frame, or
// [] when any content is visible. The CALLER additionally confirms a blank
// verdict against a second settled snapshot before emitting, so a
// transiently-empty a11y tree (app boot) never false-positives.
function blankScreenItems(labels, elements, roleSeen, screenRect) {
  if (!screenRect) return [];
  const w = screenRect.r - screenRect.l, h = screenRect.b - screenRect.t;
  if (!(w > 0 && h > 0)) return [];
  if ((labels && labels.length) || (elements && elements.length)) return [];
  if (roleSeen && ((roleSeen.textfield || 0) > 0 || (roleSeen.image || 0) > 0)) return [];
  // A visible LOADING / progress / status indicator (a native ActivityIndicator
  // normalizes to the `progress` role; a live status region to `status`) means the
  // screen is MID-LOAD, not a permanently-blank WSOD -- never fire while one shows.
  if (roleSeen && ((roleSeen.progress || 0) > 0 || (roleSeen.status || 0) > 0)) return [];
  return [{ key: 'root', w: Math.round(w), h: Math.round(h) }];
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

// Jetpack Compose can expose one control twice through UiAutomator2: a keyed,
// clickable generic semantics wrapper and an unkeyed actionable child occupying
// the same hit rectangle. Treat that pair as one control. The stable key comes
// from the wrapper while role/name come from the semantic child.
function reconcileComposeControls(elements, nativeCandidates) {
  const input = (elements || []).map((e) => ({ ...e }));
  const removed = new Set();
  const generic = new Set(['node', 'group']);
  const sameBounds = (a, b) => Array.isArray(a) && Array.isArray(b) && a.length === 4 &&
    a.every((v, i) => Math.abs(Number(v) - Number(b[i])) <= 1);

  for (let i = 0; i < input.length; i++) {
    const keyed = input[i];
    if (!keyed.key || !generic.has(keyed.role) || !keyed.bounds) continue;
    for (let j = 0; j < input.length; j++) {
      const semantic = input[j];
      if (i === j || semantic.key || generic.has(semantic.role) || !sameBounds(keyed.bounds, semantic.bounds)) continue;
      keyed.role = semantic.role;
      if (!keyed.label && semantic.label) keyed.label = semantic.label;
      keyed.sel = `key:${keyed.key}`;
      keyed.nokey = false;
      removed.add(j);
      break;
    }
  }

  // Removing an id-less duplicate must not leave holes in role:<role># indexes.
  const perRole = {};
  const controls = input.filter((_, i) => !removed.has(i)).map((e) => {
    if (e.key) return e;
    const idx = perRole[e.role] || 0;
    perRole[e.role] = idx + 1;
    return { ...e, sel: `role:${e.role}#${idx}` };
  });

  const byId = new Map((nativeCandidates || []).filter((c) => c && c.id != null).map((c) => [c.id, { ...c }]));
  for (const e of controls) {
    if (!e.key || !byId.has(e.key)) continue;
    const c = byId.get(e.key);
    if (exposesAtRole(e.role)) c.rolePresent = true;
    if (e.label) c.namePresent = true;
  }
  return { elements: controls, nativeCandidates: [...byId.values()] };
}

export { reconcileComposeControls };

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
    // Numeric character references: serializers escape non-ASCII / control
    // chars this way (e.g. Android's &#10; newlines, a tofu U+FFFD as
    // &#xFFFD;). Decoded BEFORE &amp; so a literal "&amp;#65;" stays "&#65;".
    .replace(/&#x([0-9a-fA-F]+);/g, (_, h) => String.fromCodePoint(parseInt(h, 16)))
    .replace(/&#([0-9]+);/g, (_, d) => String.fromCodePoint(parseInt(d, 10)))
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
  // On-screen frame (page-source geometry), used for
  // parent frame passed to children. Null when no geometry is exposed.
  const rect = rectOfEl(get);
  const okey = oracleKeyOf(id, role, myRoleIndex);
  // DFS enter index (every element consumes a slot), recorded on each tappable
  // frame so host-side reducers can interval-compare ancestor/descendant pairs.
  const enterSeq = out.walkSeq++;
  let tapRec = null;

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

  // CONTENT-BUG oracle (deterministic, label scan): a rendered label carrying a
  // stringify/template artifact ([object Object], whole-word undefined/null/NaN,
  // an unrendered {{..}}/${..}). Scans the displayed text the runner already
  // gathers; addressed by the same stable locale-invariant key, so a clean app
  // stays silent. Skips secure fields (never read a password).
  const dtext = displayTextOfEl(tag, get, role);
  const cbReason = contentBugReason(dtext);
  // Skip a reflected fuzzer probe (e.g. a typed "{{7*7}}" the app echoes back).
  if (cbReason && !fromFuzzInjection(dtext)) out.contentBugs.push({ key: okey, reason: cbReason, text: dtext });

  // BROKEN-ASSET oracle (tofu only on native): a rendered U+FFFD is an encoding
  // failure leaked to the screen. Same displayed text and stable key as the
  // content-bug scan; silent when every label decodes cleanly.
  const baReason = tofuReason(dtext);
  // Provenance: skip tofu the fuzzer itself typed (a reflected U+FFFD probe), not
  // an app encoding bug.
  if (baReason && !fromFuzzInjection(dtext)) out.brokenAssets.push({ key: okey, reason: baReason, detail: dtext });

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
    const bounds = rect ? [Math.round(rect.l), Math.round(rect.t), Math.round(rect.r - rect.l), Math.round(rect.b - rect.t)] : null;
    const purpose = inputPurposeOfEl(tag, get, role);
    out.elements.push({ sel, role, label: display, bounds, key: id, nokey: id == null, purpose });
    // Tappable frame + its DFS interval, consumed by the SAFE-AREA reducer in
    // snapshot(). Zero-area frames are skipped.
    if (rect && rect.r - rect.l > 0 && rect.b - rect.t > 0) {
      tapRec = { key: okey, rect, enter: enterSeq, exit: 0 };
      out.tapRects.push(tapRec);
    }
  }
  if (name && rect) {
    out.texts.push({
      text: clipLabel(name),
      bounds: [Math.round(rect.l), Math.round(rect.t), Math.round(rect.r - rect.l), Math.round(rect.b - rect.t)],
    });
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
  // DFS exit index: closes this element's interval AFTER its whole subtree
  // consumed enter slots, so `enter < other.enter < exit` iff `other` is a
  // descendant (host-side reducers' ancestor/descendant wrapping exclusion).
  const exitSeq = out.walkSeq++;
  if (tapRec) tapRec.exit = exitSeq;
  into.push(node);
}

// The screen anchor: the foreground activity (Android) or the app bundle/window
// (iOS), when observable. The route/activity is the canonical anchor prefix.
//
// DEEP-LINK PARITY is EXCLUDED on React Native / native iOS / Android (ground
// truth, not effort). That oracle reopens each visited route's URL COLD (a deep
// link) and diffs the structure, so it needs a URL the harness can read off the
// current screen and re-open. This anchor is a foreground activity / bundle /
// window name, NOT a per-screen address: a native screen reached by tapping
// exposes no URL, and Appium can only fire a deep link (`mobile: deepLink` /
// openURL) for a scheme+path the app declared in its manifest -- a private
// mapping the fuzzer cannot infer from a tapped screen. So there is no
// derivable deep link for an arbitrary reached screen. Web, where the address
// bar IS the route, is where this oracle applies.
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

async function snapshot(driver, valueNodeSelectors, insets) {
  const xml = await driver.getPageSource();
  const xmlRoot = parseXml(xml);
  let activity = null;
  try {
    if (typeof driver.getCurrentActivity === 'function') activity = await driver.getCurrentActivity();
  } catch { /* iOS / unsupported: anchor stays best-effort */ }
  const out = {
    labels: [], elements: [], texts: [], seenLabel: new Set(), perRole: {},
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
    // contentBugs / brokenAssets / tapRects: oracle findings
    // accumulated during the tree walk (raw tuples; reduced + sorted below).
    // walkSeq numbers every element's DFS enter/exit so tapRects carry the
    // intervals the safe-area reducer's ancestor exclusion needs. screenRect: the
    // application/window frame used by blank-screen and safe-area checks.
    contentBugs: [], brokenAssets: [], tapRects: [], walkSeq: 0, screenRect: null,
  };
  // The top application/window element's frame is the blank-screen scan's
  // non-zero-window guard. Both drivers wrap the page
  // source in a geometry-less root (iOS `AppiumAUT`, Android `hierarchy`), so
  // walk down the first-child spine to the first element that exposes a frame
  // (the application/window element).
  out.screenRect = (() => {
    let el = xmlRoot.children[0];
    while (el) {
      const r = rectOfEl((n) => (el.attrs[n] != null ? el.attrs[n] : ''));
      if (r) return r;
      el = el.children[0];
    }
    return null;
  })();
  // The canonical root is a single `screen` node; the parsed app subtree hangs
  // under it (parallels the SDKs forcing the root role to "screen"). parentRect
  // starts null at the app root (the screen frame is the VIEWPORT reference, not
  // a SPILL container, so the topmost app element never self-spills).
  const screen = { role: 'screen', children: buildNodes(xmlRoot, out, null) };
  const reconciled = reconcileComposeControls(out.elements, out.nativeCandidates);
  out.elements = reconciled.elements;
  out.nativeCandidates = reconciled.nativeCandidates;
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
    texts: out.texts.slice(0, 48),
    nativeCandidates: out.nativeCandidates,
    // Reduced + sorted oracle items ready for the corresponding markers.
    contentBugs: contentBugItems(out.contentBugs),
    brokenAssets: brokenAssetItems(out.brokenAssets),
    blank: blankScreenItems(out.labels, out.elements, out.roleSeen, out.screenRect),
    // SAFE-AREA: tappables whose frame intersects a device inset. `insets` is
    // resolved once per session (Android getSystemBars; iOS has no driver source,
    // so it is empty and this stays silent). Same {key,edge,by} shape the Flutter
    // explorer / the Rust parser expect.
    safeArea: safeAreaItems(out.tapRects, insets, out.screenRect),
  };
}

// Resolve the device safe-area insets once per session, in the SAME px space as
// the page-source frames. Android exposes the status/navigation bar geometry via
// getSystemBars(); the status bar is the top inset and the navigation bar the
// bottom inset (left/right stay 0: Appium exposes no landscape-cutout inset).
// iOS (XCUITest) exposes NO safe-area inset source, so this returns zeros and the
// safe-area scan stays silent on iOS-via-Appium -- the Flutter path is the iOS
// safe-area ground truth. Best-effort: any driver/parse failure yields zeros.
async function readSafeAreaInsets(driver) {
  const zero = { top: 0, bottom: 0, left: 0, right: 0 };
  try {
    if (isAndroid() && typeof driver.getSystemBars === 'function') {
      const bars = await driver.getSystemBars();
      const sb = bars && (bars.statusBar || bars.statusBars);
      const nb = bars && (bars.navigationBar || bars.navigationBars);
      return {
        top: sb && sb.visible !== false ? Number(sb.height || 0) : 0,
        bottom: nb && nb.visible !== false ? Number(nb.height || 0) : 0,
        left: 0,
        right: 0,
      };
    }
  } catch { /* unsupported / parse failure: no inset ground truth */ }
  return zero;
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

// Re-pump a fresh starting screen BETWEEN batch seeds so each replay begins from
// the same clean root (matching the web runner's resetToRoot contract). A prior
// seed may have navigated deep or CRASHED the app (left the foreground), so we
// terminate then relaunch the target: `noReset` keeps app data, so this is the
// cheap in-session equivalent of a cold start. Best-effort; a failure just leaves
// the next seed to start wherever the app is (still bracketed by SEED markers).
async function resetToRoot(driver) {
  const appId = isAndroid() ? androidPkg() : targetAppId();
  if (!appId) return;
  try { await driver.execute('mobile: terminateApp', isAndroid() ? { appId } : { bundleId: appId }); }
  catch { try { if (typeof driver.terminateApp === 'function') await driver.terminateApp(appId); } catch { /* best-effort */ } }
  try { await driver.execute('mobile: activateApp', isAndroid() ? { appId } : { bundleId: appId }); }
  catch { try { if (typeof driver.activateApp === 'function') await driver.activateApp(appId); } catch { /* best-effort */ } }
  await driver.pause(1200);
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
//              DOCUMENTED GAP: no clean, non-flaky, sim-attributable per-frame
//              trace exists for an iOS-SIMULATOR app, which is the only iOS target
//              available here (real-device frame telemetry is out of scope). We do
//              NOT fake an iOS jank signal: drainGfxinfoJank returns null on iOS and
//              no iOS JANK marker is ever emitted. Unlike the iOS LEAK signal
//              (DONE(coarse): a sim app is a host process, so its RSS is a real,
//              deterministic, monotonic session-level number; see sampleIosHeap),
//              frame timing has NO equivalent host-readable source on the simulator.
//
//              FRAME-TIMING SOURCES TRIED ON THE BOOTED SIM, AND WHY EACH FAILS
//              (verified empirically against a booted iOS 26.2 sim, xctrace 26.0):
//                - `xcrun xctrace record --template 'Animation Hitches'` (the
//                  proper frame-pacing instrument): errors at record time with
//                  "Hitches is not supported on this platform." Hitches read the
//                  device render-server's hitch telemetry, which the simulator does
//                  not emit; it works only on a REAL device. -> no data at all.
//                - `xcrun xctrace record --template 'Metal System Trace' --device
//                  <simUDID> --all-processes`: records, but the export TOC shows it
//                  captured HOST macOS processes (the sim app's GPU work is fused
//                  into the host GPU via the SimMetalHost XPC service), NOT the sim
//                  app's per-frame display timing. There is no per-sim-app frame /
//                  display / vsync table to bucket, and the data is host-wide, so it
//                  is neither attributable to the app nor false-positive-free.
//                - `xctrace ... --attach <pid|name>` for the sim app: fails with
//                  "Cannot find process for provided pid" / "Cannot find process
//                  matching name": xctrace cannot target an in-simulator process,
//                  and the sim app's HOST pid (the one simctl launchctl list / the
//                  LEAK path resolves) is invisible to xctrace attach. So even the
//                  pid we CAN resolve for the leak signal does not open a frame
//                  trace.
//                - Appium `mobile: startPerfRecord` / `driver.getPerformanceData`:
//                  Android-only (no iOS frame-timing surface).
//              A session-level CA-commit / FPS capture would also be nondeterministic
//              to bucket over a host-shared sim GPU (no clean floor mapping to a
//              stable finding id), so even a coarse session-level iOS jank verdict
//              would risk false positives. We leave it silent rather than guess.
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

// ====================================================================
//  --record CLIP CAPTURE (iOS simulator + Android emulator)
//
//  When REPROIT_VIDEO_DIR is set AND the fuzz config carries a clip plan
//  ({"replay":[...],"clip":{"sel","label","oracle"}}), film the DEVICE screen for
//  the whole replay and, once it settles, resolve the finding's element to a rect
//  + a time window, writing $REPROIT_VIDEO_DIR/box-spec.json next to clip.mov so
//  the host box-overlay step draws the finding box uniformly -- the same contract
//  as macOS-ax.swift (startClipCapture/stopClipCapture + finalize) and the web
//  runner's FINDING:BOXED handling. The element rect is read from the SAME snapshot
//  element list the replay tapped (bounds are page-source geometry: POINTS on iOS,
//  physical px on Android); videoW/videoH are the device's LOGICAL screen size in
//  the same units (driver.getWindowRect()), and box-overlay scales the recorded
//  pixel size against that automatically. Platform is detected from the Appium caps
//  (platformName) via isAndroid().
//
//  iOS films with `xcrun simctl io <udid> recordVideo` (a child process finalized
//  with SIGINT, exactly as Control-C would); Android films with Appium
//  start/stopRecordingScreen (base64 mp4 written to clip.mov). Both save to
//  $REPROIT_VIDEO_DIR/clip.mov.
// ====================================================================

// Arm a clip plan from the (single-seed) fuzz config: only in replay mode with a
// clip plan AND REPROIT_VIDEO_DIR set. Returns null (disarmed) otherwise.
function armClipCapture(fuzz) {
  const dir = process.env.REPROIT_VIDEO_DIR;
  const plan = fuzz && fuzz.clip;
  if (!dir || !plan || !plan.sel || !fuzz.replay) return null;
  return {
    dir,
    sel: plan.sel,
    label: plan.label || plan.oracle || 'finding',
    oracle: plan.oracle || '',
    mov: resolve(dir, 'clip.mov'),
    rect: null,       // [x,y,w,h] captured at the triggering tap
    actionAt: 0,      // seconds since capture start of the triggering tap
    startAt: 0,
    recording: null,  // 'ios' | 'android' | null (start failed)
    proc: null,       // the simctl recordVideo child (iOS)
  };
}

// The booted iOS simulator's udid: the caps' udid if pinned, else the first
// Booted device from `simctl list`, else the literal "booted" (simctl accepts it
// when exactly one sim is booted). Never throws.
function bootedUdid() {
  const capUdid = CAPS['appium:udid'] || CAPS.udid;
  if (capUdid) return capUdid;
  try {
    const out = execFileSync('xcrun', ['simctl', 'list', 'devices', 'booted', '-j'], { encoding: 'utf8' });
    const j = JSON.parse(out);
    for (const list of Object.values(j.devices || {})) {
      for (const d of list || []) { if (d && d.state === 'Booted' && d.udid) return d.udid; }
    }
  } catch { /* fall through to the literal */ }
  return 'booted';
}

// Start filming. iOS: spawn `simctl io <udid> recordVideo` (finalized on SIGINT).
// Android: Appium startRecordingScreen (base64 mp4 drained at stop). Best-effort;
// a failure leaves clip.recording null so finalize still emits FINDING:BOXED.
async function startClipCapture(driver, clip) {
  try { mkdirSync(clip.dir, { recursive: true }); } catch { /* ignore */ }
  clip.startAt = Date.now();
  if (isAndroid()) {
    try {
      await driver.startRecordingScreen({ forceRestart: true });
      clip.recording = 'android';
    } catch { clip.recording = null; }
    return;
  }
  const udid = bootedUdid();
  try { rmSync(clip.mov, { force: true }); } catch { /* ignore */ }
  try {
    // --codec=h264 for broad ffmpeg/QuickTime compatibility; --force overwrites a
    // stale file. Records until it receives SIGINT (see stopClipCapture).
    clip.proc = spawn('xcrun', ['simctl', 'io', udid, 'recordVideo', '--codec=h264', '--force', clip.mov], {
      stdio: 'ignore',
    });
    clip.recording = 'ios';
  } catch { clip.recording = null; }
}

// Stop filming and finalize clip.mov. iOS: SIGINT the recordVideo child so it
// flushes+closes the .mov (bounded wait for exit). Android: stopRecordingScreen
// returns base64 mp4 which we write to clip.mov. Never throws.
async function stopClipCapture(driver, clip) {
  if (clip.recording === 'android') {
    try {
      const b64 = await driver.stopRecordingScreen();
      if (b64) writeFileSync(clip.mov, Buffer.from(b64, 'base64'));
    } catch { /* leave whatever exists */ }
    return;
  }
  if (clip.recording === 'ios' && clip.proc) {
    try { clip.proc.kill('SIGINT'); } catch { /* already gone */ }
    await new Promise((res) => {
      let done = false;
      const finish = () => { if (!done) { done = true; res(); } };
      clip.proc.on('exit', finish);
      clip.proc.on('error', finish);
      setTimeout(finish, 8000); // never hang the run on a stuck finalize
    });
  }
}

// Record the finding's element rect + tap timestamp when the replay taps the
// clip.sel control (mirrors the macOS runner grabbing clipEl at the triggering
// press). Called for every replayed tap; a no-op unless the sel matches.
function noteClipTap(clip, sel, snap) {
  if (!clip || clip.sel !== sel) return;
  const el = (snap.elements || []).find((e) => e.sel === sel);
  if (el && el.bounds) clip.rect = el.bounds; // freshest geometry at the tap
  clip.actionAt = (Date.now() - clip.startAt) / 1000;
}

// After the replay: resolve the element rect (fallback to the final snapshot),
// read the LOGICAL window size, stop+finalize the recording, write box-spec.json,
// and emit FINDING:BOXED. drew:false when the element never resolved to a rect.
async function finalizeClipCapture(driver, clip, snap) {
  if (!clip.rect) {
    const el = (snap.elements || []).find((e) => e.sel === clip.sel);
    if (el && el.bounds) clip.rect = el.bounds;
  }
  // The device's logical screen size, in the SAME units as the element rect
  // (points on iOS, physical px on Android) -- box-overlay scales the recorded
  // pixel size against this so the box lands regardless of Retina/sim scale.
  let win = null;
  try { win = await driver.getWindowRect(); } catch { /* try size */ }
  if (!win || !(win.width > 0)) {
    try { const s = await driver.getWindowSize(); if (s) win = { width: s.width, height: s.height }; } catch { /* none */ }
  }
  await stopClipCapture(driver, clip);
  let drew = false;
  if (clip.rect && win && win.width > 0 && win.height > 0) {
    const [x, y, w, h] = clip.rect;
    const spec = {
      videoW: win.width,
      videoH: win.height,
      boxes: [{
        x, y, w, h,
        tStart: Math.max(0, (clip.actionAt || 0) - 0.3),
        tEnd: 1e9,
        label: clip.label,
        color: 'red',
      }],
    };
    try {
      writeFileSync(resolve(clip.dir, 'box-spec.json'), JSON.stringify(spec));
      drew = true;
    } catch { drew = false; }
  }
  log('FINDING:BOXED ' + JSON.stringify({ oracle: clip.oracle, sel: clip.sel, drew }));
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
let mobileShellDenied = false;
async function mobileShell(driver, command, args) {
  if (mobileShellDenied) return null;
  try {
    if (typeof driver.execute !== 'function') return null;
    const r = await driver.execute('mobile: shell', { command, args: args || [] });
    if (r == null) return null;
    if (typeof r === 'string') return r;
    if (typeof r === 'object' && typeof r.stdout === 'string') return r.stdout;
    return String(r);
  } catch (error) {
    // Appium logs a rejected extension before this catch runs. Remember the
    // server policy denial so every optional oracle does not repeat the same
    // noisy, ~1.5s round-trip for the rest of the journey.
    const message = String(error && error.message ? error.message : error);
    if (message.includes('adb_shell') || message.includes('Potentially insecure feature')) {
      mobileShellDenied = true;
    }
    return null;
  }
}

// ====================================================================
//  APP-INVARIANT ORACLE (SDK-self-triggered)
//
//  The native fuzzer drives the app and cannot call the app's own predicates, so
//  the RN / iOS / Android SDKs evaluate their OWN registered invariants on each
//  settled state and, only when they detect the fuzzer (REPROIT_FUZZ env on iOS,
//  the debug.reproit.fuzz prop on Android, a stable global on RN), log a marker
//  on the platform diagnostic channel (console.log -> logcat/syslog, os_log /
//  NSLog, android.util.Log):
//      REPROIT_INVARIANT {"sig":"","items":[{"id":"...","message":"..."}]}
//  We scrape that channel every settle and map any NEW marker into the
//  EXPLORE:INVARIANT line the Rust core already parses (model/map.rs), with the
//  sig we are currently on substituted for the SDK's empty sig. This is the
//  runner half of the same contract the web runner emits directly via
//  page.evaluate; the Rust core is unchanged.
// ====================================================================

// De-dup key set (sig|id|message) so revisiting a state across settles does not
// re-emit the same violation. Module-scoped for the whole walk.
const invariantEmitted = new Set();
const deviceLogEmitted = new Set();

// Read new device-log lines since the last call. Prefers the Appium log API
// (getLogs('logcat'|'syslog'), which streams entries incrementally), falling back
// to an adb logcat dump on Android. Returns an array of message strings; never
// throws (a driver/platform without a readable log channel yields []).
async function readDeviceLog(driver) {
  const out = [];
  // Android's Appium log API can return the pre-session ring buffer on its first
  // call. Read logcat for only the current app PID so a prior process can never
  // inject stale CAPSULE/EXCHANGE markers into this run.
  if (isAndroid()) {
    try {
      const pkg = typeof driver.getCurrentPackage === 'function' ? await driver.getCurrentPackage() : null;
      const pidRaw = pkg ? await mobileShell(driver, 'pidof', [pkg]) : null;
      const pid = String(pidRaw || '').trim().split(/\s+/)[0];
      if (/^\d+$/.test(pid)) {
        const dump = await mobileShell(driver, 'logcat', ['-d', `--pid=${pid}`, '-t', '400']);
        if (dump) for (const line of dump.split('\n')) {
          if (line && !deviceLogEmitted.has(line)) { deviceLogEmitted.add(line); out.push(line); }
        }
        return out;
      }
    } catch { /* fall back to Appium's log stream */ }
  }
  const type = isAndroid() ? 'logcat' : 'syslog';
  try {
    if (typeof driver.getLogs === 'function') {
      const entries = await driver.getLogs(type);
      if (Array.isArray(entries)) {
        for (const e of entries) {
          const m = e && e.message != null ? e.message : e;
          // Appium's log API is a draining stream. Do not globally suppress a
          // repeated marker here: the same invariant in a different state is a
          // distinct finding. The per-(sig,id,message) set below suppresses
          // duplicate settles of one state. Only the non-draining adb dump
          // fallback needs raw-line de-duplication.
          if (m != null) out.push(String(m));
        }
      }
    }
  } catch { /* fall through to the adb path on Android */ }
  if (!out.length && isAndroid()) {
    const dump = await mobileShell(driver, 'logcat', ['-d', '-t', '400']);
    if (dump) for (const line of dump.split('\n')) if (line && !deviceLogEmitted.has(line)) {
      deviceLogEmitted.add(line); out.push(line);
    }
  }
  return out;
}

// Extract the marker JSON object from one log line, tolerant of a log-framing
// prefix (timestamp/tag) before the token and trailing content after the object.
// Returns the parsed object or null.
function parseInvariantMarker(line) {
  const at = line.indexOf('REPROIT_INVARIANT ');
  if (at < 0) return null;
  const braceStart = line.indexOf('{', at);
  if (braceStart < 0) return null;
  const jsonStr = line.slice(braceStart);
  try {
    return JSON.parse(jsonStr);
  } catch {
    const end = jsonStr.lastIndexOf('}');
    if (end < 0) return null;
    try { return JSON.parse(jsonStr.slice(0, end + 1)); } catch { return null; }
  }
}

// Scrape the device log for REPROIT_INVARIANT markers and emit an
// EXPLORE:INVARIANT line (carrying THIS state's sig) for any NEW violations.
// De-duped per sig|id|message so the same violation is not re-emitted across
// settles of the same state. Best-effort; never throws.
async function scrapeInvariants(driver, sig, anchor) {
  let lines;
  try { lines = await readDeviceLog(driver); } catch { return; }
  const fresh = [];
  for (const line of lines) {
    for (const marker of ['REPROIT:EXCHANGE ', 'REPROIT:CAPABILITIES ', 'CAPSULE:HIT ', 'CAPSULE:MISS ']) {
      const at = line.indexOf(marker);
      if (at >= 0) log(line.slice(at));
    }
    const obj = parseInvariantMarker(line);
    const items = obj && Array.isArray(obj.items) ? obj.items : null;
    if (!items) continue;
    for (const it of items) {
      if (!it || it.id == null) continue;
      const id = String(it.id);
      const message = it.message != null ? String(it.message) : '';
      const key = sig + '|' + id + '|' + message;
      if (invariantEmitted.has(key)) continue;
      invariantEmitted.add(key);
      fresh.push({ id, message });
    }
  }
  if (fresh.length) {
    log('EXPLORE:INVARIANT ' + JSON.stringify({ sig, ...(anchor ? { route: anchor } : {}), items: fresh }));
  }
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
// BASELINE-RELATIVE jank (FP guard for the software compositor). The absolute
// 30% floor is right for real hardware, where a settled/trivial transition drops
// ~0 frames. But under an emulator's SOFTWARE GPU (swiftshader/angle) even a
// zero-work Activity transition drops tens of percent of frames purely from the
// software compositor, so the absolute floor false-positives on trivial screens.
// We therefore raise the effective floor by the DEVICE BASELINE (frame jank of a
// representative cheap render, measured once at launch) plus a margin. When
// a software renderer is detected, clamp it up to a near-total-drop floor so
// only a genuine main-thread stall (which drops nearly EVERY frame) still fires.
// On real hardware the baseline is ~0 and no software floor applies, so the
// behavior is unchanged (>= 30% still fires); a planted long-task jank storm sits
// near 100% and clears every floor.
const JANK_BASELINE_MARGIN = 25;    // a transition must beat the device baseline by this much
const JANK_SOFTWARE_FLOOR = 80;     // under a software GPU, only a near-total frame-drop storm counts
// Parse the raw "Janky frames: <n> (<pct>%)" summary from `dumpsys gfxinfo`.
// Returns { pct, count } or null. Shared by the calibration read and the
// per-transition verdict so both key off the SAME number.
function jankyPctFromGfxinfo(text) {
  if (!text) return null;
  // "Janky frames: 42 (35.00%)". Read the count and the percentage.
  const m = text.match(/Janky frames:\s*(\d+)\s*\(([\d.]+)%\)/);
  if (!m) return null;
  const count = parseInt(m[1], 10);
  const pct = parseFloat(m[2]);
  if (!Number.isFinite(pct)) return null;
  return { pct, count: Number.isFinite(count) ? count : 0 };
}
// Classify Android render jank against an effective floor. `floorPct` defaults to
// the absolute floor (real-hardware behavior); callers raise it by the device
// baseline / software-renderer clamp to stay honest under a software compositor.
// The marker still carries the fixed JANK_BUCKET so the finding id is
// deterministic across replays.
function jankFromGfxinfo(text, floorPct = JANK_PCT_FLOOR) {
  const r = jankyPctFromGfxinfo(text);
  if (!r || r.pct < floorPct) return null;
  return { bucket: JANK_BUCKET, count: r.count };
}
// The per-transition jank floor for THIS device: the absolute floor, raised over
// the measured baseline (+margin) and clamped to the software-GPU floor when a
// software renderer is present. Pure, so it is unit-tested.
function jankFloorFor(baselinePct, softwareRenderer) {
  let floor = JANK_PCT_FLOOR;
  if (Number.isFinite(baselinePct)) floor = Math.max(floor, baselinePct + JANK_BASELINE_MARGIN);
  if (softwareRenderer) floor = Math.max(floor, JANK_SOFTWARE_FLOOR);
  return floor;
}

// BACK-TRAP decision (pure, unit-tested). The NARROW, FP-safe slice of the removed
// general dead-end/sink oracle: an Android screen that SWALLOWS the system back.
// The engine-wide dead-end oracle was pulled as crawler-budget FP-prone (a budget
// -limited crawl mistook an unexhausted screen for a sink), so this deliberately
// does NOT resurrect it -- it fires only on the environment-anchored ground truth
// that the runner ITSELF performed `back` and the screen did not move.
//
// Inputs are snapshots {sig, content, anchor}: `before` = the state the back was
// pressed on, `first` = the observation right after the first press, `retry` = the
// observation after ONE retry press, `launch` = {sig, anchor} of the root/home
// screen. Returns true only when ALL hold:
//   1. NON-ROOT: `before` is neither the launch signature nor the launch activity.
//      On the home/root activity `back` is EXPECTED to be a no-op or to exit the
//      app, so a self-loop there is normal, never a trap.
//   2. FIRST press was a PURE self-loop: BOTH the structural signature AND the
//      content fingerprint are unchanged. A back that dismissed a dialog/sheet
//      moves the signature (or at least the content), so requiring both unchanged
//      excludes the legitimate "back closed an overlay" case.
//   3. RETRY press ALSO self-looped identically: a back pressed mid-transition /
//      mid-animation can read as a momentary self-loop on the first observe, so we
//      give it one more frame; only a screen still pinned after the retry is a trap.
function isBackTrap(before, first, retry, launch) {
  const nonRoot = before.sig !== launch.sig
    && !!before.anchor && before.anchor !== launch.anchor;
  const swallowed = (o) => o.sig === before.sig && o.content === before.content;
  return nonRoot && swallowed(first) && swallowed(retry);
}
// The software-rasterizer renderer names: a SwiftShader / Mesa-pipe pipeline
// really does drop frames on trivial transitions, so we raise the jank floor when
// one is in use. Shared by the primary (GL renderer string) and fallback (render
// property) probes below.
const SOFTWARE_RENDERER_RE = /swiftshader|llvmpipe|softpipe|softwarepipe|software rasteriz|mesa offscreen/;
// Whether this device renders on a SOFTWARE GPU (e.g. the emulator's SwiftShader
// pipe). Under a software compositor trivial transitions drop frames, so we raise
// the jank floor there. Best-effort: an unavailable shell channel reports hardware
// (no FP suppression, no missed real finding on a real device).
//
// The DISCRIMINATOR is the actual GL renderer NAME, not a render-driver property:
// on the Android emulator `ro.hardware.egl` is "emulation" for EVERY gpu mode,
// INCLUDING `-gpu host` (which translates GLES to the host GPU / Metal on Apple
// Silicon and is genuinely hardware-accelerated). Keying on that property misread
// a hardware host-GPU emulator as software and wrongly raised the floor to 80. The
// renderer string tells them apart: a software pipe names SwiftShader / llvmpipe,
// while the host path names a real GPU ("Apple M1 ... Metal", "Adreno", "Mali").
async function detectSoftwareRenderer(driver) {
  if (!isAndroid()) return false;
  // PRIMARY: SurfaceFlinger's "GLES:" line carries GL_RENDERER (the resolved
  // renderer name). Present on emulators and real devices alike.
  const sf = (await mobileShell(driver, 'dumpsys', ['SurfaceFlinger']) || '');
  const gles = (sf.split('\n').find((l) => /GLES:/i.test(l)) || '').toLowerCase();
  if (SOFTWARE_RENDERER_RE.test(gles)) return true;
  if (gles) return false; // a named hardware renderer (host GPU) -> not software.
  // FALLBACK (no SurfaceFlinger GLES line): the render-driver properties, matched
  // ONLY against unambiguous software-rasterizer names. The generic "emulation" /
  // "goldfish" / "angle" tokens are deliberately NOT here: they are present under
  // `-gpu host` too and would misclassify a hardware-accelerated emulator.
  for (const prop of ['ro.hardware.egl', 'ro.hardware.gpu', 'debug.hwui.renderer']) {
    const v = (await mobileShell(driver, 'getprop', [prop]) || '').trim().toLowerCase();
    if (SOFTWARE_RENDERER_RE.test(v)) return true;
  }
  return false;
}
// Measure the device's baseline frame jank: read the gfxinfo window accumulated
// over a representative cheap render (the launch + first settle), before the walk
// resets it per action. Returns the janky-frame percentage of that window, or
// null when unavailable. This is the "first settled idle period" calibration: it
// captures the software compositor's inherent per-frame cost on a NON-pathological
// render, which the per-transition floor is then measured relative to.
async function calibrateJankBaseline(driver, pkg) {
  if (!isAndroid() || !pkg) return null;
  const text = await mobileShell(driver, 'dumpsys', ['gfxinfo', pkg]);
  const r = jankyPctFromGfxinfo(text);
  return r ? r.pct : null;
}
// Reset the gfxinfo framestats window so the NEXT read reflects only the frames
// rendered by the action under test (otherwise jank accumulates across the run
// and every later action inherits it -> not per-transition). Best-effort.
async function resetGfxinfo(driver, pkg) {
  if (!isAndroid() || !pkg) return;
  await mobileShell(driver, 'dumpsys', ['gfxinfo', pkg, 'reset']);
}
// Read + classify the Android render jank for the action that just ran, against
// the device's effective floor (baseline + software-renderer aware). Null on
// iOS / no shell channel / clean render.
async function drainGfxinfoJank(driver, pkg, floorPct = JANK_PCT_FLOOR) {
  if (!isAndroid() || !pkg) return null;
  const text = await mobileShell(driver, 'dumpsys', ['gfxinfo', pkg]);
  return jankFromGfxinfo(text, floorPct);
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

// ====================================================================
//  WAKELOCK LEAK (ANDROID ONLY): a wakelock / window FLAG_KEEP_SCREEN_ON held on
//  a screen must be released when the user leaves that screen. Ground truth is
//  `dumpsys power` (the app-owned held wake locks) plus the focused window's
//  keep-screen-on flag, read LIVE while the app displays each screen. A lock
//  ACQUIRED on screen X that is STILL held after navigating to a structurally
//  different screen Y is a battery-drain leak (the CPU/screen stays awake off the
//  video/map/call screen that needed it). Sequence-dependent (a before/after
//  comparison across a navigation), so it belongs to the fuzz/soak walk, not the
//  single-screen scan crawl.
//
//  DETERMINISTIC + FP-SAFE:
//    - baseline: locks held at the LAUNCH screen are app-global; never flagged.
//    - only locks OWNED BY THE TARGET PACKAGE count (the held line names the pkg
//      in its tag or WorkSource); system/framework locks (PowerManagerService.*,
//      *:launch, *alarm*, *job*, GnssLocationProvider, ...) are ignored.
//    - each leak is attributed to the ORIGIN screen (where the lock was first
//      seen held) and reported ONCE, so a lock that legitimately persists is not
//      re-flagged on every later screen; a released lock is forgotten so a fresh
//      re-acquire is judged anew.
//    - short-lived locks released before the next screen never appear in the
//      after-sample, so they never fire.
//
//  iOS is EXCLUDED (ground-truth impossible, not effort): iOS exposes NO public
//  API to enumerate another process's held wakelocks or its
//  UIApplication.isIdleTimerDisabled state; there is no `dumpsys power`
//  equivalent and no host-readable source on the simulator. Web / desktop / TUI
//  have no wakelock concept at all. So this oracle is Android/Appium only, the
//  same shell path as the gfxinfo JANK / meminfo LEAK probes; when the
//  `mobile: shell` channel is absent every read returns an empty set and the
//  oracle degrades to silence, never a false positive.
// ====================================================================

// Wake-lock TYPES that hold the device/CPU awake (the leak-relevant ones); a
// PROXIMITY_SCREEN_OFF / DRAW lock is not a battery-drain-by-staying-awake lock,
// so it is not matched.
const WAKELOCK_TYPE_RE = /(PARTIAL_WAKE_LOCK|FULL_WAKE_LOCK|SCREEN_BRIGHT_WAKE_LOCK|SCREEN_DIM_WAKE_LOCK)/;

// Parse the app-owned held wakelock tags from `dumpsys power`. The output has a
// "Wake Locks: size=N" block whose held entries look like
//   PARTIAL_WAKE_LOCK 'com.app:Video' ON_AFTER_RELEASE ACQ=-2s (uid=10234 pid=.. ws=WorkSource{10234 com.app})
// We keep only lines that (a) name an awake-holding TYPE, (b) carry a quoted tag,
// and (c) reference the target package (in the tag or the WorkSource), so a
// system lock of the same type is excluded. Returns a Set of tag strings.
// Version-tolerant (no reliance on the block header); never throws.
export function wakelocksFromDumpsysPower(text, pkg) {
  const held = new Set();
  if (!text || !pkg) return held;
  for (const raw of String(text).split('\n')) {
    const line = raw.replace(/\r$/, '');
    if (!WAKELOCK_TYPE_RE.test(line)) continue;
    if (!line.includes(pkg)) continue;   // only locks owned by the target package
    const m = line.match(/'([^']+)'/);
    if (!m) continue;
    held.add(m[1]);
  }
  return held;
}

// Parse the focused target-package window's FLAG_KEEP_SCREEN_ON from
// `dumpsys window windows`. Windows are listed as blocks headed by a
// `Window{<hash> u0 <pkg>/<activity>}` line; a video/map screen that keeps the
// display on carries KEEP_SCREEN_ON inside its block. Returns true when the
// target package's window keeps the screen on. Version-tolerant; false when
// absent/unknown. Represented downstream as a synthetic KEEP_SCREEN_ON lock so
// the leak reducer treats a stuck screen-on flag exactly like a stuck wakelock.
export function keepScreenOnFromDumpsys(text, pkg) {
  if (!text || !pkg) return false;
  let inPkgWindow = false;
  for (const raw of String(text).split('\n')) {
    const line = raw;
    if (/Window\{/.test(line)) inPkgWindow = line.includes(pkg); // entered/left a window block
    if (inPkgWindow && /KEEP_SCREEN_ON/.test(line)) return true;
  }
  return false;
}

// The reported kind for a held id: the synthetic screen-on flag vs a real lock.
export function wakelockKind(id) { return id === 'KEEP_SCREEN_ON' ? 'keep-screen-on' : 'wakelock'; }
// EXPLORE:WAKELOCK `items` entry for a leaked id (tag + kind), sorted upstream.
export function wakelockItem(id) { return { tag: id, kind: wakelockKind(id) }; }

// PURE reducer (no device): advance the wakelock-leak state across one
// transition. `state` is { origin: Map<id,sig>, reported: Set<id> }; `baseline`
// is the app-global held set (locks held at launch, never flagged); `heldBefore`
// / `heldAfter` are the held id sets sampled on X (before the action) and on Y
// (after the transition); `fromSig`/`toSig` are the transition endpoints.
// Returns { leaks: string[] (ids acquired on X still held on a DIFFERENT Y,
// sorted), origin, reported } for the next step. See the doc block above for the
// determinism + FP-safety rules this encodes.
export function wakelockLeakStep(state, baseline, heldBefore, heldAfter, fromSig, toSig) {
  const origin = new Map(state && state.origin ? state.origin : []);
  const reported = new Set(state && state.reported ? state.reported : []);
  // Record the acquisition screen for non-baseline locks currently held on X
  // (captures locks acquired mid-dwell on X, e.g. tapping play).
  for (const id of heldBefore) {
    if (baseline.has(id) || reported.has(id)) continue;
    if (!origin.has(id)) origin.set(id, fromSig);
  }
  const leaks = [];
  if (toSig !== fromSig) {
    // A released lock (gone from the after-sample) is healthy: forget it so a
    // later re-acquire is attributed + judged afresh, and it never fires.
    for (const id of [...origin.keys()]) if (!heldAfter.has(id)) origin.delete(id);
    for (const id of [...reported]) if (!heldAfter.has(id)) reported.delete(id);
    for (const id of heldAfter) {
      if (baseline.has(id) || reported.has(id)) continue;
      if (origin.get(id) === fromSig) {
        // Acquired on X, still held after leaving X -> a leak. Report once.
        leaks.push(id);
        reported.add(id);
        origin.delete(id);
      } else if (!origin.has(id)) {
        origin.set(id, toSig);   // first seen on arrival at Y
      }
    }
  }
  leaks.sort();
  return { leaks, origin, reported };
}

// Sample the app's live held wakelock id set (Android only). Unions the
// app-owned `dumpsys power` wake locks with a synthetic KEEP_SCREEN_ON id when
// the focused package window holds FLAG_KEEP_SCREEN_ON. Empty set on iOS / no
// shell channel, so the leak reducer stays silent there (documented exclusion).
async function sampleWakelocks(driver, pkg) {
  if (!isAndroid() || !pkg) return new Set();
  const power = await mobileShell(driver, 'dumpsys', ['power']);
  const held = wakelocksFromDumpsysPower(power, pkg);
  const win = await mobileShell(driver, 'dumpsys', ['window', 'windows']);
  if (keepScreenOnFromDumpsys(win, pkg)) held.add('KEEP_SCREEN_ON');
  return held;
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

export { jankFromGfxinfo, jankyPctFromGfxinfo, jankFloorFor, isBackTrap, pssFromMeminfo, contentBugItems, contentBugReason, rectOfEl, hangBucket, tofuReason, brokenAssetItems, blankScreenItems, safeAreaItems, snapshot, loadBatch };
export { parseInvariantMarker, scrapeInvariants, invariantEmitted };

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
  // debug builds. WebdriverIO surfaces it through two execute entry points. We
  // try both documented call shapes and accept the first that returns our
  // { ok, records } shape.
  const tryRun = async (fn) => {
    try {
      const r = await fn();
      if (r && typeof r === 'object' && Array.isArray(r.records)) return r;
    } catch (e) { /* transport unavailable: fall through */ }
    return null;
  };
  // UiAutomator2 cannot execute code in the app's JS runtime. Calling these
  // optional commands there only makes WebdriverIO print a scary server ERROR
  // before our fallback catches it, so go directly to native ground truth.
  if (!isAndroid() && typeof driver.executeScript === 'function') {
    result = await tryRun(() => driver.executeScript(FIBER_PROBE_SRC, []));
  }
  if (!isAndroid() && !result && typeof driver.execute === 'function') {
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

// ── Multi-actor scenario client (the conductor protocol) ────────────────────
// Same wire protocol as the web/electron/tauri runners, the flutter explorer
// and the tui backend: the host conductor (modes/barrier.rs) owns identity
// (`GET /claim`) and ordering (`GET /next?device=` + `POST /done?device=`);
// this process plays ONE actor over its OWN Appium session/device and only
// executes actions. Each actor is a separate device (the orchestrator boots N
// sims/emulators and hands each runner its own REPROIT_APPIUM_CAPS), so no
// input isolation is needed: the conductor serializes actions globally.

// Substitute ${VAR} from the environment (same contract as the web runner):
// journeys encode `secret:` fills as ${REPROIT_SECRET_<ACCT>_<FIELD>}
// placeholders so plaintext credentials never touch disk. Unset vars expand
// to "" (a missing credential types blank, which the app rejects).
function expandEnv(s) {
  return String(s).replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (_, name) => process.env[name] || '');
}

// Locator strategies for a structural selector, in the same order tap() tries
// them: accessibility id first, then resource-id / name / content-desc. A
// `role:<role>#<idx>` selector resolves through THIS snapshot's elements list
// (the same structural index basis as the signature), then locates by its
// key/label.
function locatorsFor(sel, snap) {
  if (sel.startsWith('key:')) {
    const id = sel.slice('key:'.length);
    return [
      `~${id}`,
      `//*[@resource-id="${id}"]`,
      `//*[contains(@resource-id,"/${id}")]`,
      `//*[@name="${id}"]`,
      `//*[@content-desc="${id}"]`,
    ];
  }
  if (sel.startsWith('role:')) {
    const el = ((snap && snap.elements) || []).find((e) => e.sel === sel);
    if (!el) return [];
    const out = [];
    if (el.key) out.push(`~${el.key}`, `//*[@resource-id="${el.key}"]`, `//*[@name="${el.key}"]`);
    if (el.label) out.push(`~${el.label}`, `//*[@label="${el.label}"]`, `//*[@text="${el.label}"]`, `//*[@content-desc="${el.label}"]`);
    return out;
  }
  return [];
}

// Resolve a structural selector to a live element, or null. Never throws.
async function findEl(driver, sel, snap) {
  for (const s of locatorsFor(sel, snap)) {
    try {
      const el = await driver.$(s);
      if (await el.isExisting()) return el;
    } catch { /* next strategy */ }
  }
  return null;
}

// Fill a field located by the same key:/role: grammar as tap(). setValue clears
// existing content and types via the platform input path, so framework change
// handlers fire. A missing/unreachable target returns false so the caller
// reports a MISS rather than silently passing.
async function typeInto(driver, sel, value, snap) {
  const el = await findEl(driver, sel, snap);
  if (!el) return false;
  if (value != null && String(value).length > 0) INJECTED_VALUES.add(String(value));
  try { await el.setValue(value); } catch { return false; }
  return true;
}

// Count elements matching a journey finder, for `expect: count`. A `key:<id>`
// finder counts live matches of its first non-empty locator strategy (the same
// strategies tap resolves through); any other finder counts occurrences across
// this snapshot's visible display text (labels + texts), the same substring
// semantics the tui runner uses for its text-only surface.
async function countMatching(driver, finder, snap) {
  if (finder.startsWith('key:')) {
    for (const s of locatorsFor(finder, snap)) {
      try {
        const els = await driver.$$(s);
        if (els.length > 0) return els.length;
      } catch { /* next strategy */ }
    }
    return 0;
  }
  const blob = visibleTextBlob(snap);
  return finder ? blob.split(finder).length - 1 : 0;
}

// The visible display text of a snapshot: labels + captured text nodes, joined.
// Feeds assert:text= / assert:count: with the same substring semantics as tui.
// texts entries are {text, bounds} records (the EXPLORE:STATE shape); only the
// text participates, on every platform alike.
function visibleTextBlob(snap) {
  const parts = [
    ...((snap && snap.labels) || []),
    ...((snap && snap.texts) || []).map((t) => (t && t.text != null ? t.text : '')),
  ];
  return parts.join('\n');
}

// Execute ONE scenario action, emitting the same FUZZ:ACT/MISS/ASSERT markers
// as the other runners' scenario paths. `who` is this runner's role letter,
// for log attribution. A fresh snapshot is taken per action so role:<role>#<idx>
// selectors and asserts see the CURRENT screen (a peer's action may have moved
// this device's UI, e.g. an incoming message).
async function execScenarioAction(driver, act, who, valueNodeSelectors) {
  log('FUZZ:ACT ' + who + ' ' + act);
  await advanceCausalAction(driver);
  if (act.startsWith('shoot:')) {
    // Appium devices are captured orchestrator-side (simctl/adb) from the SHOOT
    // marker; the runner only names the point (same contract as fuzz replay).
    log('SHOOT:' + act.slice('shoot:'.length));
    return;
  }
  const snap = await snapshot(driver, valueNodeSelectors).catch(() => null);
  if (act.startsWith('assert:')) {
    const body = act.slice('assert:'.length);
    if (body.startsWith('text=')) {
      const want = body.slice('text='.length);
      const ok = visibleTextBlob(snap).includes(want);
      log('FUZZ:ASSERT ' + (ok ? 'pass' : 'fail') + ' text=' + JSON.stringify(want) + ' actor=' + who);
    } else if (body.startsWith('count:')) {
      const rest = body.slice('count:'.length);
      const eq = rest.lastIndexOf('=');
      const finder = eq >= 0 ? rest.slice(0, eq) : rest;
      const want = eq >= 0 ? parseInt(rest.slice(eq + 1), 10) : 0;
      const got = await countMatching(driver, finder, snap);
      log('FUZZ:ASSERT ' + (got === want ? 'pass' : 'fail') + ' count ' + finder + ' want=' + want + ' got=' + got + ' actor=' + who);
    } else {
      log('FUZZ:ASSERT fail unsupported ' + body + ' actor=' + who);
    }
    await driver.pause(300);
    return;
  }
  if (act === 'back') {
    try { await driver.back(); } catch { /* iOS: no hardware back; harmless */ }
    await driver.pause(500);
    return;
  }
  if (act.startsWith('auth:')) {
    // Session-restore login is not wired on the Appium runner; use a
    // `login(<account>)` actor prelude (UI flow) for multi-user auth. No-op so
    // ordering still advances, but flag it loudly.
    log('JOURNEY[a] step: auth-restore unsupported on appium runner; use login() for ' + act);
    await driver.pause(200);
    return;
  }
  if (act.startsWith('type:')) {
    const b = act.slice('type:'.length);
    const eq = b.lastIndexOf('=');
    const sel = eq >= 0 ? b.slice(0, eq) : b;
    const value = expandEnv(eq >= 0 ? b.slice(eq + 1) : '');
    const ok = await typeInto(driver, sel, value, snap);
    if (!ok) log('FUZZ:MISS ' + who + ' ' + act);
    await driver.pause(800);
    return;
  }
  if (act.startsWith('tap:')) {
    const sel = act.slice('tap:'.length);
    const ok = snap ? await tap(driver, sel, snap) : false;
    if (!ok) log('FUZZ:MISS ' + who + ' ' + act);
    await driver.pause(800);
    return;
  }
  // A key:<Name> or other cross-surface action authored for a different
  // backend: fail loudly instead of silently passing.
  log('FUZZ:MISS ' + who + ' ' + act);
}

// Multi-actor: this runner is ONE actor. It drives its own Appium session and
// pulls its next action from the host conductor (the strict step-order
// barrier), so N runners across N devices interleave exactly as the journey
// specifies. Universal wire protocol; only execScenarioAction is
// Appium-specific. Crash detection is the same oracle as fuzzing (the target
// app leaving the foreground); a crashed actor deliberately does NOT ack its
// step, so the conductor's diagnose() names this actor and action.
async function runScenarioActor(driver, valueNodeSelectors) {
  const base = process.env.REPROIT_SCENARIO_BARRIER;
  // Role identity: an explicit label wins (each runner process gets its own
  // env), else claim a distinct role from the conductor, which hands out `a`,
  // `b`, ... atomically so two actors can never collide.
  let who = process.env.REPROIT_DEVICE;
  if (!who) {
    try { who = (await (await fetch(base + '/claim')).text()).trim(); } catch { who = ''; }
    if (!who || who.startsWith('ERR')) who = 'a';
  }
  log('JOURNEY claimed role=' + who);
  await driver.pause(1200);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  let crashed = false;
  for (let guard = 0; guard < 100000; guard++) {
    let body = 'WAIT';
    try { body = (await (await fetch(base + '/next?device=' + who)).text()).trim(); }
    catch { await sleep(100); continue; }
    if (body === 'DONE') break;
    if (body === 'WAIT') { await sleep(40); continue; }
    const act = body.startsWith('ACT\t') ? body.slice(4) : body;
    await execScenarioAction(driver, act, who, valueNodeSelectors);
    if (await appCrashed(driver)) { emitCrash(act); crashed = true; break; }
    try { await fetch(base + '/done?device=' + who, { method: 'POST' }); } catch { /* retry via next poll */ }
  }
  log('JOURNEY DONE');
  log(crashed ? 'Some tests failed' : 'All tests passed');
}

export { runScenarioActor, execScenarioAction, locatorsFor };

async function main() {
  const url = new URL(APPIUM);
  // Session creation can legitimately take minutes on a cold host: the first
  // XCUITest session builds WebDriverAgent from source (several minutes on a
  // stock CI runner). REPROIT_APPIUM_CONNECT_TIMEOUT_MS raises the webdriverio
  // client's cap without changing the local default.
  const connectMs = Number(process.env.REPROIT_APPIUM_CONNECT_TIMEOUT_MS) || 120000;
  // Arm the SDK-self-triggered app-invariant oracle. The RN/iOS/Android SDKs
  // evaluate the app's OWN registered invariants only when they detect the
  // fuzzer, then log a REPROIT_INVARIANT marker we scrape (see scrapeInvariants).
  // iOS reads REPROIT_FUZZ from the app process env, which XCUITest sets via
  // `processArguments.env`; Android has no app-env channel under UiAutomator2, so
  // we set the unprivileged `debug.reproit.fuzz` system property once the session
  // exists (below). RN (JS) reads a stable global its reproit E2E build sets,
  // since Appium cannot inject a JS global into the RN VM (documented in the RN
  // SDK README).
  // PERMISSION-WALK sweep: when REPROIT_DENY_PERMISSION names a permission, DON'T
  // auto-grant (Appium would otherwise pre-approve everything), so the app takes
  // its denied branch; the explicit denial below forces the "not allowed" state.
  const denyPermission = process.env.REPROIT_DENY_PERMISSION || '';
  const caps = { 'appium:autoGrantPermissions': !denyPermission, ...CAPS };
  const androidCausalStaged = stageAndroidCausalBeforeLaunch(caps);
  const androidPackage = caps['appium:appPackage'] || caps.appPackage;
  // A remote farm normally does not expose adb to the runner. In replay mode,
  // keep the app stopped until pushFile/setprop have installed the capsule.
  // Without an appPackage Appium cannot deterministically activate it, so fail
  // before claiming hermetic replay instead of accepting a bootstrap race.
  const delayedAndroidLaunch = isAndroid() && !!process.env.REPROIT_CAPSULE && !androidCausalStaged;
  if (delayedAndroidLaunch && !androidPackage) {
    throw new Error('Hermetic Android replay on a remote device requires appium:appPackage (or pre-launch adb access)');
  }
  if (delayedAndroidLaunch) caps['appium:autoLaunch'] = false;
  if (!isAndroid()) {
    const pa =
      caps['appium:processArguments'] && typeof caps['appium:processArguments'] === 'object'
        ? { ...caps['appium:processArguments'] }
        : {};
    let capsuleJson;
    if (process.env.REPROIT_CAPSULE) {
      try { capsuleJson = readFileSync(process.env.REPROIT_CAPSULE, 'utf8'); } catch { /* capability gate will explain */ }
    }
    pa.env = {
      REPROIT_FUZZ: '1',
      REPROIT_CAUSAL: '1',
      REPROIT_DEVICE: process.env.REPROIT_DEVICE || 'a',
      ...(capsuleJson ? { REPROIT_CAPSULE_JSON: capsuleJson } : {}),
      ...(pa.env || {}),
    };
    caps['appium:processArguments'] = pa;
  }
  const driver = await remote({
    hostname: url.hostname,
    port: Number(url.port) || 4723,
    path: url.pathname && url.pathname !== '/' ? url.pathname : '/',
    capabilities: caps,
    logLevel: 'error',
    connectionRetryTimeout: connectMs,
  });
  // Android fuzz signal (see caps note): set the unprivileged debug.* prop the
  // SDK reads. Best-effort over the relaxed-security shell; a session without
  // that channel simply leaves the app-invariant oracle inert (never a false
  // positive). Set early so the SDK sees it on subsequent state settles.
  if (isAndroid()) {
    await mobileShell(driver, 'setprop', ['debug.reproit.fuzz', '1']);
    await mobileShell(driver, 'setprop', ['debug.reproit.action', '0']);
    if (process.env.REPROIT_CAPSULE) {
      try {
        const destination = '/data/local/tmp/reproit-capsule.json';
        const encoded = Buffer.from(readFileSync(process.env.REPROIT_CAPSULE)).toString('base64');
        await driver.pushFile(destination, encoded);
        await mobileShell(driver, 'chmod', ['0644', destination]);
        await mobileShell(driver, 'setprop', ['debug.reproit.capsule', destination]);
      } catch (error) {
        log('REPROIT:CAPABILITIES {"http_replay":{"status":"unsupported","detail":"could not inject Android capsule"}}');
        if (delayedAndroidLaunch) throw new Error(`Could not inject Android replay capsule before launch: ${error}`);
      }
    } else {
      await mobileShell(driver, 'setprop', ['debug.reproit.capsule', '__reproit_none__']);
    }
    if (delayedAndroidLaunch) await driver.activateApp(String(androidPackage));
  }

  // Multi-actor scenario: this process plays one actor, pulling from the
  // conductor; the fuzz walk and its oracles do not run. The value-node
  // selectors still apply so scenario snapshots sign identically to fuzz.
  if (process.env.REPROIT_SCENARIO_BARRIER) {
    log('JOURNEY[a] step: scenario actor=' + (process.env.REPROIT_DEVICE || 'a'));
    await runScenarioActor(driver, loadValueNodes());
    await driver.deleteSession();
    return;
  }

  log('JOURNEY claimed role=a');
  await driver.pause(1500);

  // SAFE-AREA: resolve the device insets once (Android getSystemBars; iOS has no
  // driver source, so this is zeros and the safe-area scan stays silent on iOS).
  const safeAreaInsets = await readSafeAreaInsets(driver);

  // JANK CALIBRATION (device-level, once per session): the render pipeline's
  // frame-jank baseline on a representative CHEAP render (the launch + first
  // settle), plus whether this device uses a SOFTWARE GPU. The per-transition
  // jank floor is measured relative to these so a software compositor's inherent
  // frame drops on trivial transitions don't false-positive, while a real
  // main-thread stall still clears the floor. On real hardware the baseline is
  // ~0 and no software floor applies, so behavior is unchanged. Read BEFORE the
  // walk resets the gfxinfo window per action.
  const jankPkgId = androidPkg();
  const softwareRenderer = await detectSoftwareRenderer(driver);
  const jankBaselinePct = await calibrateJankBaseline(driver, jankPkgId);
  const jankFloor = jankFloorFor(jankBaselinePct, softwareRenderer);
  if (isAndroid()) {
    log(`JOURNEY[a] step: jank-floor=${jankFloor}` +
        (softwareRenderer ? ' (software GPU)' : '') +
        (Number.isFinite(jankBaselinePct) ? ` baseline=${jankBaselinePct}%` : ''));
  }

  // PERMISSION-WALK: explicitly DENY the named permission so every screen the app
  // reaches next is on the denied branch. Android: `mobile: changePermissions`
  // (or resetPermission) sets it to denied. iOS has no reliable Appium primitive
  // to deny a specific permission post-launch, so the sweep is Android-first here
  // (Flutter's mocked platform channel covers both); a failure leaves the flag
  // off so no PERMISSIONWALK marker is ever emitted for an ungated run.
  let permissionDenied = false;
  if (denyPermission) {
    try {
      if (isAndroid()) {
        const pkg = targetAppId();
        await driver.execute('mobile: changePermissions', {
          permissions: 'all',
          appPackage: pkg,
          action: 'revoke',
        });
        permissionDenied = true;
        log(`JOURNEY[a] step: denied permission=${denyPermission}`);
      } else {
        log(`JOURNEY[a] step: permission-walk unsupported on iOS-via-Appium (use Flutter); permission=${denyPermission}`);
      }
    } catch (e) {
      log(`JOURNEY[a] step: permission denial failed (${e && e.message ? e.message : e})`);
    }
  }

  // Drive every seed in this session. A multi-seed BATCH ({"batch":[...]}, written
  // by `reproit check` when gate.runs > 1 and by multi-seed fuzz) wraps each seed's
  // walk in SEED:BEGIN <seed> ... SEED:END <seed> so the Rust core (fuzz.rs
  // split_log_segments) splits the one drive log into one segment per replay;
  // between seeds re-pump a clean root so each begins identically. A single
  // {"seed":..}/{"replay":..} run emits NO SEED markers and runs exactly as before.
  const { seeds, isBatch } = loadBatch();
  let anyCrashed = false;
  for (let seedIdx = 0; seedIdx < seeds.length; seedIdx++) {
   const fuzz = seeds[seedIdx];
   if (isBatch) {
     if (seedIdx > 0) await resetToRoot(driver);
     log(`SEED:BEGIN ${Number(fuzz.seed || 0)}`);
   }

  const seenStates = new Set();
  const triedEdges = new Set();
  const actionsByState = new Map();
  const graph = new Map();
  let launchSig = null;
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
    const snap = await snapshot(driver, valueNodeSelectors, safeAreaInsets);
    snap.sig = effectiveSig(snap);
    if (!seenStates.has(snap.sig)) {
      seenStates.add(snap.sig);
      // sig: CANONICAL STRUCTURAL signature (anchor + normalized Node tree),
      //      locale-invariant.
      // labels: DISPLAY-ONLY visible text (map show), never in the sig.
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
          if (e.purpose) o.inputPurpose = e.purpose;
          if (e.bounds) o.bounds = e.bounds;
          if (e.nokey) o.nokey = true;
          return o;
        }),
        texts: (snap.texts || []).slice(0, 48),
      }));
      // GRAPH 1 vs GRAPH 2: once per newly-seen state, probe the React fiber
      // tree for press handlers + exported a11y props and emit EXPLORE:GROUNDTRUTH
      // so the engine can diff the operable set against the a11y tree. Joined to
      // the native page source by the stable ids it just saw. Best-effort.
      const nativeIds = new Set(snap.elements.map((e) => e.key).filter((k) => k != null));
      await emitGroundtruth(driver, snap.sig, nativeIds, snap.nativeCandidates);
      // CONTENT-BUG for this newly-seen state, keyed by the SAME sig. Pure label
      // scan (no pixels, no timing), so it reproduces on replay; only emitted
      // when a broken-content artifact is actually rendered (clean app stays
      // silent).
      if (snap.contentBugs.length) {
        log('EXPLORE:CONTENTBUG ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: snap.contentBugs }));
      }
      // STUCK-KEYBOARD for this newly-seen state, keyed by the SAME sig.
      // Ground truth from the driver: the IME is visible (isKeyboardShown)
      // while the active element is not an editable. iOS tags are
      // XCUIElementType roles, Android tags are widget classes; TextView only
      // counts as editable on iOS but matching it on Android merely suppresses
      // a finding (safe direction). Only emitted on a violation; any driver
      // hiccup stays silent so a flaky bridge can never mint a false positive.
      try {
        if (await driver.isKeyboardShown()) {
          let editableFocused = false;
          try {
            const active = await driver.getActiveElement();
            const elId = active && (active['element-6066-11e4-a52e-4f735466cecf'] || active.ELEMENT);
            if (elId) {
              const tag = String((await driver.getElementTagName(elId)) || '');
              editableFocused = /TextField|SecureTextField|SearchField|TextView|EditText|AutoComplete|Input/i.test(tag);
            }
          } catch (_) { /* no active element => nothing focused */ }
          if (!editableFocused) {
            log('EXPLORE:STUCKKEYBOARD ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}) }));
          }
        }
      } catch (_) { /* driver without IME introspection stays silent */ }
      // SAFE-AREA for this newly-seen state, keyed by the SAME sig. Pure
      // inset-vs-frame geometry (Android insets from getSystemBars; iOS is
      // silent for lack of a driver source), no pixels, so it reproduces on
      // replay; only emitted when a tappable actually sits in an inset.
      if (snap.safeArea.length) {
        log('EXPLORE:SAFEAREA ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: snap.safeArea }));
      }
      // PERMISSION-WALK: under a denial sweep, mark each newly-seen screen as
      // reached AFTER the denial. The Rust invariant fires only for a marked
      // screen that is ALSO a graph dead end. Silent outside a denial sweep.
      if (permissionDenied) {
        log('EXPLORE:PERMISSIONWALK ' + JSON.stringify({ sig: snap.sig, permission: denyPermission, ...(snap.anchor ? { route: snap.anchor } : {}) }));
      }
      // BROKEN-ASSET (tofu only on native; img/font reasons stay web-only) for
      // this newly-seen state, keyed by the SAME sig. Pure label scan, so it
      // reproduces on replay; silent when every label decodes cleanly.
      if (snap.brokenAssets.length) {
        log('EXPLORE:BROKENASSET ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: snap.brokenAssets }));
      }
      // BLANK-SCREEN (white-screen-of-death) for this newly-seen state, keyed
      // by the SAME sig. A just-launched app can expose a transiently empty
      // a11y tree (boot timing), so a blank verdict is CONFIRMED against a
      // second snapshot after a short settle: only a still-blank tree emits.
      // Any driver hiccup stays silent so a flaky bridge can never mint a
      // false positive.
      if (snap.blank.length) {
        try {
          await driver.pause(1500);
          const again = await snapshot(driver, valueNodeSelectors, safeAreaInsets);
          if (again.blank.length) {
            log('EXPLORE:BLANKSCREEN ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), items: snap.blank }));
          }
        } catch (_) { /* cannot confirm => never guess-and-flag */ }
      }
    }
    // APP-INVARIANT: scrape the SDK's self-emitted markers for this state. Runs
    // every settle (not only newly seen states) so a marker logged after the
    // first observation is still caught.
    // Markers are de-duplicated per state, id, and message.
    await scrapeInvariants(driver, snap.sig, snap.anchor);
    return snap;
  }

  let current = await observe();
  // A just-launched app can expose a not-yet-populated a11y tree on the very
  // first snapshot (boot-timing dependent; observed with Settings on CI iOS
  // simulators: valid signature, zero elements). One short settle + re-observe
  // so the walk starts from the real launch state instead of an empty one.
  // Cross-platform and cheap; a same-sig retry is a no-op in observe().
  if (current.elements.length === 0) {
    await driver.pause(2000);
    current = await observe();
  }
  launchSig = current.sig;
  // BACK-TRAP: the root/home activity anchor, so a back self-loop THERE (expected:
  // back exits or no-ops on the launch screen) is never mistaken for a trap.
  const launchAnchor = current.anchor;
  let stuck = 0;
  let crashed = false;
  const prefix = fuzz.prefix || null;
  const replay = fuzz.replay || null;
  const prefixLen = prefix ? prefix.length : 0;
  const mapMode = !replay && !prefix && !fuzz.seed;
  const budget = replay
    ? replay.length
    : (((mapMode && !FUZZ_CONFIGURED) ? Number.MAX_SAFE_INTEGER : (fuzz.budget || ACTION_BUDGET)) + prefixLen);

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

  // ROTATION / BACKGROUND-RESTORE (lifecycle-metamorphic): each distinct state
  // sig is transform-tested once. Native device lifecycle via the Appium driver.
  const rotChecked = new Set();
  const bgChecked = new Set();
  // ROTATION-stability: rotate the device to the opposite orientation, settle,
  // then rotate BACK to the original orientation and re-observe. A correct screen
  // reflows but rebuilds the SAME structure once the original orientation is
  // restored; an app that mishandles the configuration change (Android activity
  // recreation, iOS trait-collection change) and loses content/state that never
  // comes back regresses the STRUCTURAL signature (value-state excluded).
  // Round-trip identity is false-positive-free; an app that LOCKS orientation
  // makes setOrientation a no-op, so the check silently reports nothing. Guarded
  // on the pre-transform state having content; self-restoring. Returns the
  // re-observed state.
  async function rotationCheck(snap) {
    const expected = snap.structuralSig;
    let orig = null;
    try {
      orig = await driver.getOrientation();
      const other = orig === 'LANDSCAPE' ? 'PORTRAIT' : 'LANDSCAPE';
      await driver.setOrientation(other);
      await driver.pause(700);
    } catch (_) { orig = null; }
    if (orig) {
      try { await driver.setOrientation(orig); await driver.pause(700); } catch (_) {}
    }
    const after = await observe();
    if (snap.elements && snap.elements.length > 0 && after.structuralSig !== expected) {
      log('EXPLORE:ROTATION ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), expected, got: after.structuralSig }));
    }
    return after;
  }
  // BACKGROUND-RESTORE-stability: send the app to the background then bring it
  // back to the foreground (driver.background(seconds) backgrounds for N seconds
  // then auto-restores), and re-observe. A correct app returns to the SAME
  // screen with state intact; one that drops you elsewhere or loses state across
  // the lifecycle regresses the STRUCTURAL signature. No size change; guarded on
  // the pre-transform state having content. Any driver hiccup stays silent so a
  // flaky bridge can never mint a false positive. Returns the re-observed state.
  async function backgroundCheck(snap) {
    const expected = snap.structuralSig;
    let ok = false;
    try { await driver.background(2); ok = true; } catch (_) { ok = false; }
    if (!ok) return snap;
    await driver.pause(700);
    const after = await observe();
    if (snap.elements && snap.elements.length > 0 && after.structuralSig !== expected) {
      log('EXPLORE:BGRESTORE ' + JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}), expected, got: after.structuralSig }));
    }
    return after;
  }
  // WAKELOCK-LEAK state (Android only; see the doc block by
  // wakelocksFromDumpsysPower). wlBaseline = the app-global locks held at the
  // launch screen (never flagged); wlState threads the per-lock origin screen +
  // already-reported set through the walk so each leak fires once, attributed to
  // the screen that acquired it. Empty/no-op on iOS (documented exclusion).
  const wlBaseline = await sampleWakelocks(driver, pkg);
  let wlState = { origin: new Map(), reported: new Set() };
  // Emit an EXPLORE:WAKELOCK finding for any lock acquired on `fromSig` that is
  // still held after landing on `toSig` (a real navigation away). No-op when the
  // sets are empty (iOS / clean release), so nothing is faked off-Android.
  const checkWakelocks = async (fromSig, toSig, heldBefore) => {
    if (fromSig === toSig) return;
    const heldAfter = await sampleWakelocks(driver, pkg);
    const step = wakelockLeakStep(wlState, wlBaseline, heldBefore, heldAfter, fromSig, toSig);
    wlState = { origin: step.origin, reported: step.reported };
    if (step.leaks.length) {
      log('EXPLORE:WAKELOCK ' + JSON.stringify({ sig: fromSig, items: step.leaks.map(wakelockItem) }));
    }
  };

  // --record clip capture: film the device for the whole replay, then box the
  // finding's element once it settles (iOS simctl recordVideo / Android Appium
  // screen recording). Armed only in replay mode with a clip plan + video dir.
  const clip = replay ? armClipCapture(fuzz) : null;
  if (clip) {
    await startClipCapture(driver, clip);
    await driver.pause(400); // lead-in so the first frames exist before the tap
  }

  for (let actions = 0; actions < budget && stuck < 3; actions++) {
    // LEAK sampler: in replay mode, sample memory once per action (BEFORE acting,
    // so action k's sample reflects the heap after the previous action settled);
    // together with the start + final samples it forms the monotonic series the
    // soak slope is read from. No-op outside replay; per-platform inside sampleHeap.
    if (replay && actions > 0) await sampleHeap(Date.now() - t0);
    // LIFECYCLE-metamorphic oracles (rotation, background-restore): once per
    // distinct state, apply a native device-lifecycle transform and assert the
    // structural signature survives it. Self-restoring, so `current` is refreshed
    // to the (restored) reality; never in replay (a recorded clip must reproduce
    // the walk without extra lifecycle events).
    if (!replay) {
      if (!rotChecked.has(current.sig)) { rotChecked.add(current.sig); current = await rotationCheck(current); }
      if (!bgChecked.has(current.sig)) { bgChecked.add(current.sig); current = await backgroundCheck(current); }
    }
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
      const actions = current.elements.map((el) => 'tap:' + el.sel).sort().concat(['back']);
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
    await advanceCausalAction(driver);
    if (act === 'back') {
      const before = current.sig;
      const beforeAnchor = current.anchor;
      triedEdges.add(edgeKey(before, 'back'));
      const beforeContent = current.content;
      // WAKELOCK: the locks held ON this screen, sampled just before leaving it.
      const wlBefore = await sampleWakelocks(driver, pkg);
      const tHang0 = Date.now();
      try { await driver.back(); } catch { /* ignore */ }
      await driver.pause(700);
      // HANG watchdog on the back transition (same floor + keying as the tap path).
      const hb = hangBucket((Date.now() - tHang0) - 700);
      if (hb != null) {
        const confirmed = await androidAnrSeen(driver, pkg);
        log('EXPLORE:HANG ' + JSON.stringify({ from: before, action: 'back', bucket: hb, ...(confirmed ? { anr: true } : {}) }));
      }
      let next = await observe();
      // BACK-TRAP (Android, narrow): the back press left the structural signature
      // AND the content fingerprint unchanged -- a pure self-loop, i.e. back was
      // SWALLOWED (a dialog/sheet dismissal would move one of them). On a NON-root
      // activity that is a trapped screen. This is the FP-safe, runner-observed
      // slice of the removed general dead-end oracle; it never fires on the
      // launch/home activity (back is expected to exit there) and requires the SAME
      // self-loop to survive ONE retry (an in-flight animation gets another frame).
      const beforeSnap = { sig: before, content: beforeContent, anchor: beforeAnchor };
      const launchSnap = { sig: launchSig, anchor: launchAnchor };
      const firstSwallowed = next.sig === before && next.content === beforeContent;
      const nonRoot = before !== launchSig && !!beforeAnchor && beforeAnchor !== launchAnchor;
      if (isAndroid() && firstSwallowed && nonRoot) {
        // Retry once for animation/transition settle, then let the pure decision
        // (isBackTrap) make the final call over before/first/retry/launch.
        try { await driver.back(); } catch { /* ignore */ }
        await driver.pause(700);
        const retry = await observe();
        if (isBackTrap(beforeSnap, next, retry, launchSnap)) {
          // ESCAPE: relaunch the target (terminate + activate) so the walk continues
          // from a clean root instead of ramming the trap until the stuck-counter
          // kills the walk (the audit's starvation). Reset stuck: escaping is progress.
          await resetToRoot(driver);
          current = await observe();
          stuck = 0;
          continue;
        }
        // The retry moved: it was a slow transition, not a trap. Continue with the
        // post-retry snapshot as the observed result.
        next = retry;
      }
      if (next.sig !== before) {
        log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'back', to: next.sig }));
        rememberEdge(graph, before, 'back', next.sig);
        // WAKELOCK: leaving `before` for a different screen; flag locks acquired
        // on `before` that are still held now (Android only, no-op otherwise).
        await checkWakelocks(before, next.sig, wlBefore);
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
    // --record: the tap on the finding's element is the moment to box. Grab its
    // rect + the capture-relative timestamp from THIS snapshot, before the press
    // may mutate the tree (finalize falls back to the final snapshot).
    if (clip) noteClipTap(clip, sel, current);
    triedEdges.add(edgeKey(current.sig, 'tap:' + sel));
    const before = current.sig;
    const beforeContent = current.content;
    // JANK: reset the gfxinfo framestats window so the read after this tap counts
    // only the frames this action rendered (per-transition, not run-cumulative).
    await resetGfxinfo(driver, pkg);
    // WAKELOCK: the locks held ON this screen, sampled before the tap (outside the
    // HANG timing window below so it doesn't inflate the blocked-time measure).
    const wlBefore = await sampleWakelocks(driver, pkg);
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
    const jk = await drainGfxinfoJank(driver, pkg, jankFloor);
    if (jk) {
      log('EXPLORE:JANK ' + JSON.stringify({ from: before, action: 'tap:' + sel, bucket: jk.bucket, count: jk.count }));
    }
    const next = await observe();
    if (next.sig !== before) {
      log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'tap:' + sel, to: next.sig }));
      rememberEdge(graph, before, 'tap:' + sel, next.sig);
      // WAKELOCK: this tap navigated away from `before`; flag locks acquired on
      // `before` that are still held on `next` (Android only, no-op otherwise).
      await checkWakelocks(before, next.sig, wlBefore);
      stuck = 0;
    } else if (next.content !== beforeContent) {
      // Layer-1 effect detection: the tap changed displayed content (a calculator
      // keypress / counter on a capped display) without a structural move.
      // EFFECTIVE, so reset stuck and keep driving; no self-edge is recorded.
      stuck = 0;
    } else stuck++;
    current = next;
  }

  // LEAK sampler: a final sample after the last action, so the series spans the
  // whole soak (start ... last action). No-op outside replay; per-platform inside.
  if (replay) await sampleHeap(Date.now() - t0);
  // --record clip finalize: resolve the finding's element rect, write box-spec.json
  // next to clip.mov, finalize the recording, and emit FINDING:BOXED. The host
  // gates on drew + runs box-overlay.mjs to draw the box (the uniform post-capture
  // path for every backend that cannot inject a live overlay).
  if (clip) await finalizeClipCapture(driver, clip, current);
  log(`JOURNEY[a] step: explored ${seenStates.size} states`);
   if (crashed) anyCrashed = true;
   if (isBatch) log(`SEED:END ${Number(fuzz.seed || 0)}`);
  }

  log('JOURNEY DONE');
  log(anyCrashed ? 'Some tests failed' : 'All tests passed');
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
