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
import { inspectPlatformStep } from './inspect-control.mjs';

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

function log(line) {
  process.stdout.write(line + '\n');
}
function stageAndroidCausalBeforeLaunch(caps) {
  if (!isAndroid()) return true;
  const serial = caps['appium:udid'] || caps.udid;
  const adb = (args) =>
    execFileSync('adb', [...(serial ? ['-s', String(serial)] : []), ...args], { stdio: 'ignore' });
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
  try {
    return JSON.parse(readFileSync(p, 'utf8'));
  } catch {
    return {};
  }
}

// The multi-seed BATCH contract shared with every other runner (runners/web
// loadBatch, the Flutter scaffold's FuzzCfg.loadBatch, runners/linux-
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

function edgeKey(sig, action) {
  return sig + '|' + action;
}
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
  for (const sig of actionsByState.keys())
    if (firstUntriedAction(actionsByState, tried, sig)) return true;
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
  if (!p) {
    const def = resolve(process.cwd(), 'reproit.yaml');
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
  let s = seed >>> 0 || 1;
  return (n) => {
    s ^= s << 13;
    s >>>= 0;
    s ^= s >> 17;
    s ^= s << 5;
    s >>>= 0;
    return (s & 0x7fffffff) % n;
  };
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
//  Byte-identical to crates/reproit/src/model/signature.rs, the RN/web SDKs,
//  and signature_vectors.json. Spec: docs/signature.md.
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
