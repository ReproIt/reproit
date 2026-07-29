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
//   REPROIT_ELECTRON_DISABLE_SANDBOX=1
//                        disable Chromium's sandbox in an already-contained worker
//
// Status: validated end-to-end against a real Electron app (dev-dir mode).

// playwright is imported dynamically inside main() so this module stays
// import-safe (the parity test imports the host-pure signature functions
// below without needing the heavy runtime dependency installed).
import {
  readFileSync,
  statSync,
  existsSync,
  mkdirSync,
  writeFileSync,
  rmSync,
  appendFileSync,
} from 'node:fs';
import { createRequire } from 'node:module';
import { resolve as resolvePath, join as joinPath } from 'node:path';
import { spawnSync } from 'node:child_process';
// CHOICE-ANOMALY oracle, shared with the web runner. choiceAnomalyInPage is the
// self-contained in-page pass (it works over page.evaluate here because the
// Electron renderer is Chromium, exactly like the web runner's CDP path); the
// constants are the single source of truth for the outlier thresholds. Host-pure
// + dependency-free, so a static import keeps this module import-safe (the parity
// test that imports the signature functions pulls this in without side effects).
import {
  choiceAnomalyInPage,
  CHOICE_OUTLIER_RATIO,
  CHOICE_MIN_MAGNITUDE,
  CHOICE_ROLES,
} from './web/choice-oracle.mjs';
import {
  redactNetworkHeaders,
  redactNetworkValue,
  parseNetworkBody,
  redactSse,
} from './web/runner.mjs';
import {
  occlusionScan,
  confirmOcclusions,
  securityScan,
  dupSubmitEligible,
  focusLossArm,
  focusLossCheck,
  blankScreenScan,
  brokenAssetScan,
  zoomTappableKeys,
  zoomReflowScan,
  scrollRoundTripScan,
  installListenerLeakCounter,
  listenerLeakSample,
} from './web/hygiene-oracles.mjs';
import { layoutOverflowScan, confirmLayoutOverflow } from './web/overflow-oracle.mjs';
import { zeroContrastScan } from './web/zero-contrast-oracle.mjs';
import { deadInputProbe } from './web/dead-input-oracle.mjs';
import { inspectPlatformStep } from './inspect-control.mjs';
// Shared FP-hardening helpers, imported from the web runner so the exact SAME
// stabilization/guards apply to the Electron (Chromium) backend (fix across all
// platforms): DOM-quiescence settle, the deep-link/metamorphic content-divergence
// gate, the SPA soft-404 guard, the route-link filter, and the bot-wall detector.
// runner.mjs's main() is guarded by an import.meta check, so importing it is inert.
import {
  settleForSignature,
  soft404View,
  isSoftHandled,
  collectRouteLinks,
  normalizePathname,
  detectBotWall,
  ASSET_EXT_SOURCE,
} from './web/runner.mjs';

const APP = process.env.REPROIT_APP_DIR || process.env.REPROIT_APP;
const VIDEO_DIR = process.env.REPROIT_VIDEO_DIR || undefined;
const ACTION_BUDGET = 36;
const MAX_LABEL_LEN = 40;
// Layer-1 value-class cap (docs/signature.md "Value-state"): once a structural
// node has shown more than this many DISTINCT value-class combinations, the
// runner drops it to structural-only so an adversarial value generator cannot
// explode the graph. The oracle is stateless; the cap is purely runner-local.
const VALUE_CLASS_CAP = 8;

const NETWORK_FILE = process.env.REPROIT_NETWORK_FILE || undefined;
const NETWORK_ACTOR = process.env.REPROIT_DEVICE || 'a';
let causalActionIndex = 0;
let causalOrdinal = 0;
function log(line) {
  if (String(line).startsWith('FUZZ:ACT ')) {
    causalActionIndex++;
    causalOrdinal = 0;
  }
  process.stdout.write(line + '\n');
}
function appendNetworkFact(value) {
  if (!NETWORK_FILE) return;
  try {
    appendFileSync(NETWORK_FILE, JSON.stringify(value) + '\n', { encoding: 'utf8', mode: 0o600 });
  } catch (_) {}
}
function canonicalNetworkUrl(raw) {
  try {
    const u = new URL(raw);
    const pairs = [...u.searchParams.entries()].sort(
      ([ak, av], [bk, bv]) => ak.localeCompare(bk) || av.localeCompare(bv),
    );
    u.search = '';
    for (const [k, v] of pairs) u.searchParams.append(k, v);
    return u.toString();
  } catch (_) {
    return String(raw);
  }
}
function wsFrame(message) {
  if (typeof message !== 'string') return null;
  try {
    return redactNetworkValue(JSON.parse(message));
  } catch (_) {
    return null;
  }
}
export async function installElectronWebSockets(context, capsulePath) {
  const exchanges = capsulePath
    ? (JSON.parse(readFileSync(capsulePath, 'utf8')).exchanges || []).filter(
        (e) => e.required && /^(ws|wss)$/.test(e.protocol),
      )
    : [];
  const used = new Set();
  await context.routeWebSocket(/.*/, (socket) => {
    const url = socket.url();
    const next = () =>
      exchanges
        .map((exchange, index) => ({ exchange, index }))
        .filter(
          ({ exchange, index }) =>
            !used.has(index) &&
            exchange.actor === NETWORK_ACTOR &&
            exchange.actionIndex === causalActionIndex &&
            canonicalNetworkUrl(exchange.url) === canonicalNetworkUrl(url),
        )
        .sort((a, b) => a.exchange.ordinal - b.exchange.ordinal)[0];
    if (capsulePath) {
      const deliver = () => {
        for (;;) {
          const item = next();
          if (!item || item.exchange.method !== 'RECV') break;
          used.add(item.index);
          socket.send(JSON.stringify(item.exchange.responseBody));
          log(`CAPSULE:HIT ${item.exchange.id}`);
        }
      };
      queueMicrotask(deliver);
      socket.onMessage((message) => {
        const item = next();
        const value = wsFrame(message);
        if (
          !item ||
          item.exchange.method !== 'SEND' ||
          value == null ||
          JSON.stringify(value) !== JSON.stringify(item.exchange.requestBody)
        ) {
          log(`CAPSULE:MISS WS SEND ${url} action=${causalActionIndex}`);
          socket.close({ code: 1008, reason: 'reproit capsule miss' });
          return;
        }
        used.add(item.index);
        log(`CAPSULE:HIT ${item.exchange.id}`);
        deliver();
      });
      return;
    }
    const server = socket.connectToServer();
    const capture = (method, message, forward) => {
      const value = wsFrame(message);
      if (value == null) {
        log(
          'REPROIT:CAPABILITIES {"websocket":{"status":"unsupported","detail":' +
            '"non-JSON frame"},"websocket_replay":{"status":"unsupported"}}',
        );
        forward(message);
        return;
      }
      const ordinal = causalOrdinal++;
      appendNetworkFact({
        id: `${NETWORK_ACTOR}-${causalActionIndex}-${ordinal}`,
        actor: NETWORK_ACTOR,
        actionIndex: causalActionIndex,
        ordinal,
        protocol: new URL(url).protocol.replace(':', ''),
        method,
        url,
        requestHeaders: {},
        requestBody: method === 'SEND' ? value : undefined,
        status: 101,
        responseHeaders: {},
        responseBody: method === 'RECV' ? value : undefined,
        required: true,
      });
      forward(message);
    };
    socket.onMessage((message) => capture('SEND', message, (value) => server.send(value)));
    server.onMessage((message) => capture('RECV', message, (value) => socket.send(value)));
  });
  log(
    'REPROIT:CAPABILITIES {"websocket":{"status":"captured"},' +
      '"websocket_replay":{"status":"captured"},"sse":{"status":"captured"},' +
      '"sse_replay":{"status":"captured"}}',
  );
}

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
    } catch (e) {
      /* capture is best-effort; still emit the marker below */
    }
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
// STRUCTURAL signature (fed a structure descriptor) and for hashing long labels
// in clipLabel. Matches the web runner / Rust oracle.
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
//  Spec: docs/signature.md. This block is host-pure (no DOM) so the parity
//  test imports it directly; the browser-side snapshot() builds a Node tree in
//  page context and feeds it here in Node.
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

function loadFuzz() {
  const p = process.env.REPROIT_FUZZ_CONFIG;
  if (!p) return {};
  try {
    return JSON.parse(readFileSync(p, 'utf8'));
  } catch {
    return {};
  }
}

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

// Determine launch mode: dev directory vs packaged executable.
// A dev directory has a package.json and its own node_modules/electron.
// A packaged executable is a regular file (or .app bundle on macOS).
function resolveElectronLaunch(app) {
  if (!app) return null;
  let isDir = false;
  try {
    isDir = statSync(app).isDirectory();
  } catch {
    return null;
  }
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
    } catch {
      /* try next */
    }
  }
  // Fallback: try resolving 'electron' from the app dir directly.
  try {
    const req = createRequire(resolvePath(app, 'package.json'));
    const electronBin = req('electron');
    if (typeof electronBin === 'string') {
      return { executablePath: electronBin, args: [app] };
    }
  } catch {
    /* not found */
  }
  return null;
}

// Snapshot the DOM: a STRUCTURAL, locale-invariant signature plus display-only
// labels and the structural selectors for each tappable. Electron's renderer is
// Chromium, so this is identical to runners/web/runner.mjs: the signature is a
// hash of the canonical role tree + stable developer identifiers (data-testid,
// id, name, aria role, input type) + structural position, with ALL user-facing
// text excluded. Visible text is kept only as a display label for `map show`,
// never folded into the hash or a selector. Elements are addressed by stable
// selector preference (data-testid > id > name > aria-role + structural index);
// a tappable lacking any stable id falls back to role+index and is flagged
// `nokey`.
async function snapshot(page, valueNodeSelectors) {
