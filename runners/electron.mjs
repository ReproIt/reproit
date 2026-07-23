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
  const snap = await page.evaluate(
    ({ maxLen, valueNodeSelectors }) => {
      const labels = []; // DISPLAY-ONLY visible text
      const rawTaps = []; // tappable nodes in document order
      const textNodes = []; // (stable-key, trimmed text) for the Layer-1 fingerprint

      // Fixed canonical role vocabulary (docs/signature.md "Roles").
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
      const TRANSIENT_ROLES = {
        toast: 1,
        snackbar: 1,
        spinner: 1,
        progress: 1,
        tooltip: 1,
        badge: 1,
      };

      // DOM -> canonical role, from tag + aria role + input type, NEVER text.
      const roleOf = (el) => {
        const tag = el.tagName.toLowerCase();
        const ariaRole = (el.getAttribute('role') || '').toLowerCase();
        if (ariaRole) {
          if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox')
            return 'textfield';
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

      // Elements running an INFINITE animation, computed ONCE per snapshot from a
      // single document.getAnimations() call (a per-node call is O(nodes) on a large
      // DOM and dominates the crawl; mirrors the web runner).
      const infiniteAnimEls = new Set();
      try {
        const all = document.getAnimations ? document.getAnimations() : [];
        for (const a of all) {
          if (a.playState !== 'running') continue;
          const t = a.effect && a.effect.getComputedTiming ? a.effect.getComputedTiming() : null;
          if (t && t.iterations === Infinity && a.effect && a.effect.target)
            infiniteAnimEls.add(a.effect.target);
        }
      } catch (_) {}

      // Transient heuristic: role / aria-live / class flag a flickering node.
      const isTransientEl = (el) => {
        const ariaRole = (el.getAttribute('role') || '').toLowerCase();
        if (TRANSIENT_ROLES[ariaRole]) return true;
        if (ariaRole === 'alert' || ariaRole === 'status') return true;
        const live = (el.getAttribute('aria-live') || '').toLowerCase();
        if (live === 'assertive' || live === 'polite') return true;
        const cls = (el.getAttribute('class') || '').toLowerCase();
        if (/\b(toast|snackbar|spinner|progress|loader|loading|tooltip|badge)\b/.test(cls))
          return true;
        if (el.hasAttribute('data-transient')) return true;
        // A node mid-INFINITE-animation samples a different frame every capture, so
        // exclude it. Membership in a per-snapshot precomputed Set (one
        // document.getAnimations() call) instead of a per-node call (mirrors web).
        if (infiniteAnimEls.has(el)) return true;
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
        if (
          ar === 'status' ||
          ar === 'log' ||
          ar === 'progressbar' ||
          ar === 'meter' ||
          ar === 'timer'
        )
          return ar;
        if (tag === 'output' || ar === 'output') return 'output';
        const live = (el.getAttribute('aria-live') || '').toLowerCase();
        if (live === 'polite' || live === 'assertive') return 'status';
        if (tag === 'input') {
          const t = (el.getAttribute('type') || 'text').toLowerCase();
          if (
            [
              'checkbox',
              'radio',
              'range',
              'button',
              'submit',
              'reset',
              'image',
              'hidden',
              'file',
              'password',
            ].includes(t)
          )
            return null;
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
        if (tag === 'input' || tag === 'textarea' || tag === 'select')
          return el.value != null ? String(el.value) : '';
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
            const got = (
              el.getAttribute('data-testid') ||
              el.getAttribute('data-test-id') ||
              el.getAttribute('id') ||
              el.getAttribute('name') ||
              ''
            ).trim();
            if (id && got === id) return true;
          } else if (sel.indexOf('role:') === 0) {
            const hash = sel.indexOf('#');
            if (hash < 0) continue;
            const role = sel.slice(5, hash);
            const idx = parseInt(sel.slice(hash + 1), 10);
            if (!(idx >= 0)) continue;
            let seen = -1,
              target = null;
            const root = document.body || document.documentElement;
            (function walk(node) {
              if (target || !node) return;
              if (roleOf(node) === role) {
                seen++;
                if (seen === idx) {
                  target = node;
                  return;
                }
              }
              for (const c of node.children) walk(c);
            })(root);
            if (target === el) return true;
          } else {
            try {
              if (el.matches && el.matches(sel)) return true;
            } catch (e) {}
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
        if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role))
          return true;
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
      const visible = (el) => {
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0) return false;
        const st = getComputedStyle(el);
        return st.visibility !== 'hidden' && st.display !== 'none';
      };
      const fnvLbl = (name) => {
        let h = 0x811c9dc5;
        for (let i = 0; i < name.length; i++) {
          h ^= name.charCodeAt(i);
          h = Math.imul(h, 0x01000193) >>> 0;
        }
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
        const id = idOf(el);
        if (id != null) node.id = id;
        const type = typeOf(el, role);
        if (type != null) node.type = type;
        const icon = iconOf(el);
        if (icon != null) node.icon = icon;
        if (valueBearing) {
          node.value = valueOf(el);
          // The flag makes the canonical is_value_bearing accept the node even
          // when roleOf normalized its raw value-role (status/output/...) to node.
          node.value_node = true;
          // Layer-1 content fingerprint: a value node's stable key + its raw value.
          const fkey = id != null ? 'key:' + id : 'vrole:' + (vrole || 'opt');
          textNodes.push([fkey, node.value]);
        }
        if (transient) {
          node.transient = true;
          node.children = [];
          return node;
        }

        // Layer-1 content fingerprint over text-bearing nodes (runner-local, NOT
        // canonical): any keyed element's own (non-child) trimmed text contributes
        // (stable-key, text). This catches a display whose textContent changes
        // without any structural move (a calculator/counter), so the action is seen
        // as EFFECTIVE even when the value node itself was not detected as a
        // value-role. The raw text never enters the canonical key.
        if (!isRoot && id != null && !valueBearing) {
          let own = '';
          for (const c of el.childNodes) {
            if (c.nodeType === 3) own += c.textContent;
          }
          own = own.trim();
          if (own) textNodes.push(['text:' + id, own]);
        }

        // labels + tappables (display/elements list; never in the hash)
        if (!isRoot) {
          const name = nameOf(el);
          if (name) labels.push(clipLabel(name));
          if (interactive(el, role)) {
            rawTaps.push({
              role,
              key: keyOf(el),
              label: name ? clipLabel(name) : '',
            });
          }
        }

        node.children = [];
        collectChildren(el, node.children);
        return node;
      };
      const collectChildren = (el, out) => {
        for (const child of el.children) {
          if (!visible(child)) {
            collectChildren(child, out);
            continue;
          }
          out.push(buildNode(child, false));
        }
      };

      const root = document.body || document.documentElement;
      const tree = root ? buildNode(root, true) : { role: 'screen', children: [] };

      // Structural selectors for replay (key, else role+per-role index).
      const perRole = {};
      const tappables = rawTaps.map((tn) => {
        const idx = perRole[tn.role] || 0;
        perRole[tn.role] = idx + 1;
        const sel = tn.key ? 'key:' + tn.key : 'role:' + tn.role + '#' + idx;
        return { sel, role: tn.role, index: idx, key: tn.key, label: tn.label };
      });

      // Anchor: route/path of the current screen.
      let anchor = null;
      try {
        if (location && location.pathname) {
          let pth = location.pathname;
          // Trailing-slash route normalization: /a/ and /a are the same screen.
          if (pth.length > 1) pth = pth.replace(/\/+$/, '') || '/';
          anchor = pth;
        }
      } catch (e) {}

      // Layer-1 content fingerprint source: sorted (stable-key, trimmed text) over
      // value + keyed-text nodes. Sorted here so it is order-independent.
      textNodes.sort((a, b) =>
        a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0,
      );

      return { tree, anchor, labels: [...new Set(labels)], tappables, textNodes };
    },
    { maxLen: MAX_LABEL_LEN, valueNodeSelectors: valueNodeSelectors || [] },
  );

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

// PARITY: keep in sync with runners/web/runner.mjs (operability + flicker oracle)
//
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
const ANCHOR_SEL =
  'header,nav,main,footer,aside,' +
  '[role=banner],[role=navigation],[role=main],[role=contentinfo],' +
  '[role=complementary],[role=region],[role=search],[role=listbox],' +
  '[role=list],[role=tablist],[role=toolbar],[role=dialog],[id]';

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
      key: keyOf(el),
      node: el,
      text: (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 256),
      x: Math.round(r.x),
      y: Math.round(r.y),
      w: Math.round(r.width),
      h: Math.round(r.height),
    });
  }
  window.__reproitAnchors = anchors;
  window.__reproitAnchorDoc = document;
  return anchors.length;
}

function churnedAnchors(sel) {
  const old = window.__reproitAnchors;
  // No mark, or the document was replaced (navigation): not a flicker candidate.
  if (!old || window.__reproitAnchorDoc !== document) {
    window.__reproitAnchors = null;
    return null;
  }
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
    if (cur.has(k)) {
      dup.add(k);
      continue;
    }
    cur.set(k, el);
  }
  const churned = [];
  for (const a of old) {
    if (dup.has(a.key)) continue; // ambiguous key -> skip
    const now = cur.get(a.key);
    if (!now) continue; // gone in the new state -> a real removal, not flicker
    if (now === a.node) continue; // same node survived -> reconciled, no churn (good)
    const r = now.getBoundingClientRect();
    const sameBox =
      Math.round(r.x) === a.x &&
      Math.round(r.y) === a.y &&
      Math.round(r.width) === a.w &&
      Math.round(r.height) === a.h;
    const sameText = (now.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 256) === a.text;
    if (sameBox && sameText) churned.push(a.key); // unchanged yet rebuilt = flicker
  }
  window.__reproitAnchors = null;
  return churned;
}

// PARITY: keep in sync with runners/web/runner.mjs (overflow oracle).
//
// CONTENT-BUG oracle (deterministic, DOM/label-based). Fires ONLY on a GROUND-
// TRUTH artifact impossible to render as legitimate copy: [object Object] (an
// object coerced to a string) or an unrendered {{...}}/${...} template placeholder.
// The bare words undefined/null/NaN are NOT matched (they occur in real copy and
// code samples -- a false positive), and text inside a CODE context (<code>/<pre>/
// <script>/<style>/<textarea>/[contenteditable]) is skipped (docs show template
// syntax legitimately). Scans only the OWN text of keyed, visible elements so the
// finding is addressed by a stable, locale-invariant key (never the text). Pure
// substring/structure test, no pixel or timing read, so the same DOM yields the
// same finding on every run/replay.
function detectContentBugs(injectedValues) {
  // Fuzzer provenance (mirrors the web tier + brokenAssetScan): a reflected fuzzer
  // probe is not the app's own broken content.
  const injected = (Array.isArray(injectedValues) ? injectedValues : [])
    .map((v) => String(v == null ? '' : v).toLowerCase())
    .filter((v) => v.length > 0);
  const fromFuzzInjection = (text) => {
    const n = String(text || '').toLowerCase();
    if (!n) return false;
    if (injected.some((v) => n.indexOf(v) !== -1 || (v.length >= 3 && v.indexOf(n) !== -1)))
      return true;
    // Fragmented reflection: the browser parsed markup out of the probe, so the
    // visible text is a fragment; check the specific artifact tokens for provenance.
    const arts = [];
    const tm = n.match(/\{\{[^}]*\}\}/g);
    if (tm) arts.push(...tm);
    const dm = n.match(/\$\{[^}]*\}/g);
    if (dm) arts.push(...dm);
    if (n.indexOf('[object object]') !== -1) arts.push('[object object]');
    return arts.some((a) => injected.some((v) => v.indexOf(a) !== -1));
  };
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };
  const CODE_TAGS = new Set(['code', 'pre', 'script', 'style', 'textarea']);
  const inCodeContext = (el) => {
    if (el.isContentEditable) return true;
    for (let n = el; n && n !== document.body; n = n.parentElement) {
      if (CODE_TAGS.has(n.tagName.toLowerCase())) return true;
    }
    return false;
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
    return t.replace(/\s+/g, ' ').trim();
  };
  // Prose guard for BOTH artifact kinds: fire only when the artifact IS the label,
  // never when docs prose merely mentions "[object Object]" or the "{{ }}" syntax.
  const dominates = (s) => s.length <= 24 && !/[.!?]/.test(s);
  const reasonOf = (text) => {
    if (!text) return null;
    if (text.includes('[object Object]')) {
      const s = text
        .replace(/\[object Object\]/g, ' ')
        .replace(/\s+/g, ' ')
        .trim();
      if (dominates(s)) return 'object-object';
    }
    if (/\{\{[^}]*\}\}/.test(text) || /\$\{[^}]*\}/.test(text)) {
      const s = text
        .replace(/\{\{[^}]*\}\}/g, ' ')
        .replace(/\$\{[^}]*\}/g, ' ')
        .replace(/\s+/g, ' ')
        .trim();
      if (dominates(s)) return 'unrendered-template';
    }
    return null;
  };
  const out = [];
  const seen = new Set();
  const all = document.body ? document.body.querySelectorAll('*') : [];
  for (const el of all) {
    if (!visible(el)) continue;
    if (inCodeContext(el)) continue;
    const key = keyOf(el);
    if (!key) continue;
    const text = ownText(el);
    const reason = reasonOf(text);
    if (!reason) continue;
    if (fromFuzzInjection(text)) continue;
    const dedup = key + '|' + reason;
    if (seen.has(dedup)) continue;
    seen.add(dedup);
    out.push({ key, reason, text: text.slice(0, 80) });
  }
  out.sort((a, b) =>
    a.key < b.key ? -1 : a.key > b.key ? 1 : a.reason < b.reason ? -1 : a.reason > b.reason ? 1 : 0,
  );
  return out;
}

// PARITY: keep in sync with runners/web/runner.mjs (jank/hang watchdog).
//
// JANK / HANG watchdog (deterministic, recorded-trace based). We key off the
// browser's own Long Tasks trace, never a wall-clock duration sample: a
// `longtask` PerformanceObserver entry is emitted for any task that blocks the
// main thread > 50ms, buffered and delivered after the blocking task finishes.
// We classify by the MAX blocked duration into coarse, well-separated floors so
// timing jitter can never flip the verdict. Electron's renderer is Chromium, so
// the Long Tasks API is present and this is verbatim with the web runner.
const JANK_FLOOR_MS = 200;
const HANG_FLOOR_MS = 2000;
async function installLongTaskObserver(page) {
  await page
    .addInitScript(() => {
      try {
        window.__reproitLongTasks = [];
        const obs = new PerformanceObserver((list) => {
          for (const e of list.getEntries()) window.__reproitLongTasks.push(Math.round(e.duration));
        });
        obs.observe({ entryTypes: ['longtask'] });
      } catch (_) {
        /* no Long Tasks API: jank/hang silent on this engine */
      }
    })
    .catch(() => {});
}
async function drainJank(page) {
  const tasks = await page
    .evaluate(() => {
      const t = window.__reproitLongTasks || [];
      window.__reproitLongTasks = [];
      return t;
    })
    .catch(() => []);
  if (!tasks || !tasks.length) return null;
  const max = Math.max(...tasks);
  if (max >= HANG_FLOOR_MS) {
    return { kind: 'hang', bucket: HANG_FLOOR_MS, count: tasks.length };
  }
  if (max >= JANK_FLOOR_MS) {
    return { kind: 'jank', bucket: JANK_FLOOR_MS, count: tasks.length };
  }
  return null;
}

// PARITY: keep in sync with runners/web/runner.mjs (leak heap sampler).
//
// LEAK sampler (deterministic, v8 heap). The CDP `Runtime.getHeapUsage` reports
// the REAL, unrounded v8 used-heap size (performance.memory is quantized and
// useless for a multi-MB leak), so we use that when a CDP session is available
// and force a GC first (`HeapProfiler.collectGarbage`) so the reading is the
// RETAINED (live) heap. We emit a MEMORY:SAMPLE marker per cycle; the soak side
// reconstructs the series. Electron's renderer is Chromium with full CDP, so
// this is the precise (non-fallback) path, byte-identical to the web runner.
async function sampleHeap(page, cdp, tMs) {
  let used = null;
  if (cdp) {
    try {
      await cdp.send('HeapProfiler.collectGarbage').catch(() => {});
      const r = await cdp.send('Runtime.getHeapUsage');
      if (r && typeof r.usedSize === 'number') used = Math.round(r.usedSize);
    } catch (_) {
      used = null;
    }
  }
  if (used == null) {
    try {
      used = await page.evaluate(() => {
        if (performance.memory && typeof performance.memory.usedJSHeapSize === 'number') {
          return performance.memory.usedJSHeapSize;
        }
        return null;
      });
    } catch (_) {
      used = null;
    }
  }
  if (used == null) return;
  log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: used }));
}

// PARITY: keep in sync with runners/web/runner.mjs (Tier-2 pixel-flicker oracle).
//
// Tier-2 flicker oracle (gated, Chromium/CDP only). Records the frames the
// renderer presented during a transition (CDP Page.startScreencast) and scores
// the sequence for a transient divergence: a middle frame that diverges from the
// settled FINAL frame far more than the endpoints (flicker-oracle.mjs
// transientDivergence). OFF by default; only emits when REPROIT_FLICKER_PIXELS=1,
// same gate as the web runner. The pngjs decoder + the host-pure probe/flicker
// helpers are imported lazily inside main() so this module stays import-safe for
// the parity test; if any of them is unavailable the oracle stays silent.
const FLICKER_PIXELS = process.env.REPROIT_FLICKER_PIXELS === '1';
// Probe mode (REPROIT_PROBE=1): the web tier's destructive probe pass. This
// runner has no probe of its own, but the flag still gates the viewport-
// swapping zoom-reflow check below, matching the web runner's guard.
const PROBE = process.env.REPROIT_PROBE === '1';
// Filled in by main() via dynamic import when FLICKER_PIXELS is on. Null until
// then (and on any import failure), which keeps startScreencastCapture a no-op.
let PIXEL = null;
function pngToRgba(buf) {
  const png = PIXEL.PNG.sync.read(buf);
  return { data: png.data, width: png.width, height: png.height };
}
async function startScreencastCapture(cdp) {
  if (!FLICKER_PIXELS || !PIXEL || !cdp) return null;
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
    try {
      cdp.off('Page.screencastFrame', onFrame);
    } catch (_) {}
    return null;
  }
  return {
    async stop() {
      try {
        await cdp.send('Page.stopScreencast');
      } catch (_) {}
      try {
        cdp.off('Page.screencastFrame', onFrame);
      } catch (_) {}
      return frames;
    },
  };
}
async function finishScreencastCapture(cap, from, action) {
  if (!cap) return;
  let frames;
  try {
    frames = await cap.stop();
  } catch (_) {
    return;
  }
  if (!frames || frames.length < 3) return;
  let rgbas;
  try {
    rgbas = frames.map(pngToRgba);
  } catch (_) {
    return;
  }
  const final = rgbas[rgbas.length - 1];
  const diffs = [];
  for (const f of rgbas) {
    if (
      f.width !== final.width ||
      f.height !== final.height ||
      f.data.length !== final.data.length
    ) {
      continue;
    }
    diffs.push(PIXEL.changedFraction(f.data, final.data));
  }
  const fl = PIXEL.transientDivergence(diffs);
  if (fl) {
    log('EXPLORE:FLICKER ' + JSON.stringify({ from, action, peak: fl.peak, frames: fl.frames }));
  }
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
    const roleOf = (el) => {
      const tag = el.tagName.toLowerCase();
      const ariaRole = (el.getAttribute('role') || '').toLowerCase();
      if (ariaRole) {
        if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox')
          return 'textfield';
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
      if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(role))
        return true;
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
      'banner',
      'complementary',
      'contentinfo',
      'form',
      'main',
      'navigation',
      'region',
      'search',
      // document structure
      'article',
      'definition',
      'directory',
      'document',
      'feed',
      'figure',
      'group',
      'heading',
      'img',
      'list',
      'listitem',
      'math',
      'none',
      'note',
      'presentation',
      'separator',
      'table',
      'term',
      'toolbar',
      'tooltip',
      'caption',
      'rowgroup',
      'row',
      'cell',
      'columnheader',
      'rowheader',
      // containers + live regions / status
      'dialog',
      'alertdialog',
      'alert',
      'log',
      'marquee',
      'status',
      'timer',
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
    // reachable: on-screen AND hit-testable, so a real pointer user can operate
    // it. The operable gate below uses this so an off-screen/occluded control is
    // not a phantom pointer-only/keyboard gap.
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

    // Clear any stale tags from a prior state, then re-tag in document order.
    for (const e of document.querySelectorAll('[data-reproit-gt]'))
      e.removeAttribute('data-reproit-gt');
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
      if (!isRoot && !visible(el)) {
        for (const c of el.children) walk(c, false);
        return;
      }
      if (!isRoot) {
        const role = roleOf(el);
        const inTappableWalk = interactive(el, role);
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
            sel,
            role,
            native,
            cursor,
            deleg,
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
      const { listeners } = await cdp.send('DOMDebugger.getEventListeners', {
        objectId: result.objectId,
      });
      if (
        (listeners || []).some(
          (l) => l.type === 'click' || l.type === 'pointerdown' || l.type === 'mousedown',
        )
      )
        return true;
    } catch (e) {
      /* CDP best-effort */
    }
  }
  return false;
}

// Does this tagged element have its OWN real click/pointer listener? CDP-only.
// `pointer` = a real click/pointer handler (graph-1 operability); `key` = a real
// keydown/keypress/keyup handler. The key signal catches "focusable but
// keyboard-dead" controls (a click-only div) WITHOUT pressing a key.
async function gtElementListeners(cdp, i) {
  try {
    const { result } = await cdp.send('Runtime.evaluate', {
      expression: 'document.querySelector(\'[data-reproit-gt="' + i + '"]\')',
    });
    if (!result || !result.objectId) return { pointer: false, key: false };
    const { listeners } = await cdp.send('DOMDebugger.getEventListeners', {
      objectId: result.objectId,
    });
    const ls = listeners || [];
    return {
      pointer: ls.some(
        (l) => l.type === 'click' || l.type === 'pointerdown' || l.type === 'mousedown',
      ),
      key: ls.some((l) => l.type === 'keydown' || l.type === 'keypress' || l.type === 'keyup'),
    };
  } catch (e) {
    return { pointer: false, key: false };
  }
}

// GRAPH 2 part A: a real Tab traversal from document.body. Press Tab up to
// `steps` times, recording the tagged index of document.activeElement each time
// (untagged focus stops record -1). An element's inTabOrder = its index appeared.
// Focus trap: Tab cycled through a set of elements that never returned focus to
// body (the active element kept changing among a bounded subset and body was
// never reached again after leaving it). Returns { inTab:Set<int>, focusTrap }.
async function gtTabOrder(page, count, steps) {
  // Start from a clean baseline: blur whatever is focused onto body.
  await page.evaluate(() => {
    try {
      if (document.activeElement) document.activeElement.blur();
      document.body.focus();
    } catch (e) {}
  });
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
    for (let k = firstReal + 1; k < visited.length; k++)
      if (visited[k] === -2) {
        returnedToBody = true;
        break;
      }
  }
  const focusTrap = firstReal >= 0 && !returnedToBody && inTab.size > 0 && inTab.size < count;
  return { inTab, focusTrap };
}

// Build and emit the EXPLORE:GROUNDTRUTH record for the current state. `sig` is
// the SAME signature the EXPLORE:STATE for this state carried. `cdp` may be null
// (no listener-based operability then). Best-effort throughout: any probe that
// fails is simply omitted, so we never emit a dimension we did not measure.
async function emitGroundtruth(page, cdp, sig) {
  let els;
  try {
    els = await gtCollect(page);
  } catch (e) {
    return;
  }
  if (!els || !els.length) {
    log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap: false, elements: [] }));
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
  let inTab = new Set(),
    focusTrap = false;
  try {
    ({ inTab, focusTrap } = await gtTabOrder(page, els.length, 60));
  } catch (e) {}

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
    // keyboardActivatable, derived WITHOUT firing the control (pressing Enter/
    // Space would trigger the app's real handler as a side effect). A native
    // control or one with a real key listener is keyboard-activatable; a
    // focusable, operable element that is NEITHER native NOR has a key listener
    // (a click-only div) is keyboard-DEAD -> a WCAG 2.1.1 gap. Without CDP we
    // can't see key handlers, so fall back to focusable && reachable.
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
  try {
    await page.evaluate(() => {
      for (const el of document.querySelectorAll('[data-reproit-gt]'))
        el.removeAttribute('data-reproit-gt');
    });
  } catch (e) {}

  log('EXPLORE:GROUNDTRUTH ' + JSON.stringify({ sig, focusTrap, elements: records }));
}

// STRUCTURAL tap: resolve a locale-invariant selector and click it. Returns
// true on success. Mirrors runners/web/runner.mjs's tap(). No visible text is
// ever used to locate the element.
//   key:testid:<v> -> [data-testid="v"] (or data-test-id)
//   key:id:<v>     -> #<v>
//   key:name:<v>   -> [name="v"]
//   role:<role>#<idx> -> the idx-th visible tappable of that role, document order
async function tap(page, sel) {
  const ok = await page
    .evaluate(
      ({ s }) => {
        const visible = (el) => {
          const r = el.getBoundingClientRect();
          if (r.width === 0 || r.height === 0) return false;
          const st = getComputedStyle(el);
          return st.visibility !== 'hidden' && st.display !== 'none';
        };
        const cssEscape = (v) =>
          window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\]/g, '\\$&');

        const doClick = (el) => {
          // Stash the clicked element for the post-tap oracle probes (the
          // duplicate-submit eligibility check and the focus-loss guards read it
          // in-page). A window ref only, never a DOM mutation, so the signature/
          // content/mutation oracles are untouched.
          try {
            window.__reproitLastTap = el;
            // FOCUS-LOSS probe: a real user click gives the control keyboard focus
            // before activating it; el.click() alone does not. When the walk armed
            // the probe pre-tap (focusLossArm), focus first (no scroll, so the
            // viewport-dependent snapshot is untouched) so the oracle can observe
            // whether the app's re-render then drops focus back to <body>.
            if (window.__reproitFocusProbe) {
              try {
                el.focus({ preventScroll: true });
              } catch (_) {}
              window.__reproitTapFocused = document.activeElement === el;
            }
          } catch (_) {}
          el.click();
          return true;
        };

        if (s.startsWith('key:')) {
          const body = s.slice(4);
          const ci = body.indexOf(':');
          if (ci < 0) return false;
          const kind = body.slice(0, ci);
          const val = body.slice(ci + 1);
          let el = null;
          if (kind === 'testid') {
            el =
              document.querySelector('[data-testid="' + cssEscape(val) + '"]') ||
              document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
          } else if (kind === 'id') {
            el = document.getElementById(val);
          } else if (kind === 'name') {
            el = document.querySelector('[name="' + cssEscape(val) + '"]');
          }
          if (!el) return false;
          return doClick(el);
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
          const roleOf = (el) => {
            const tag = el.tagName.toLowerCase();
            const ariaRole = (el.getAttribute('role') || '').toLowerCase();
            if (ariaRole) {
              if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox')
                return 'textfield';
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
            if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(r))
              return true;
            if (el.hasAttribute('onclick') || el.tabIndex >= 0) return true;
            return false;
          };
          let seen = -1,
            target = null;
          const walk = (el) => {
            if (target) return;
            if (!visible(el)) {
              for (const c of el.children) walk(c);
              return;
            }
            const r = roleOf(el);
            if (interactive(el, r) && r === role) {
              seen++;
              if (seen === idx) {
                target = el;
                return;
              }
            }
            for (const c of el.children) walk(c);
          };
          const root = document.body || document.documentElement;
          if (root) walk(root);
          if (!target) return false;
          return doClick(target);
        }

        return false;
      },
      { s: sel },
    )
    .catch(() => false);
  return !!ok;
}

// ── --record clip capture (route B: host film + box-spec) ───────────────────
// Electron's renderer is Chromium, so we ALREADY film the window with
// Playwright's recordVideo (window-only by construction: it captures the
// renderer surface, never the desktop -- the hard privacy rule). To match the
// uniform native host path (record_native_clips wants clip.mov + box-spec.json,
// then draws the box with box-overlay.mjs), we resolve the finding's element to
// a viewport-relative rect in CSS-px logical space, write box-spec.json, and
// remux the recorded .webm to clip.mov. box-overlay scales the rect by
// recordedPixels/logical (DPR-safe) and draws the same red box + caption chip
// the live web overlay draws.

// Resolve the finding's element (by the SAME key:/role: selector grammar tap()
// uses) to a viewport-relative box in CSS px, scrolling it into view and letting
// the scroll settle first (so the rect matches the frames filmed after this
// returns). Returns { x, y, w, h, videoW, videoH } or null if unresolved.
async function resolveClipBox(page, sel) {
  return await page
    .evaluate(
      async ({ s }) => {
        const visible = (el) => {
          const r = el.getBoundingClientRect();
          if (r.width === 0 || r.height === 0) return false;
          const st = getComputedStyle(el);
          return st.visibility !== 'hidden' && st.display !== 'none';
        };
        const cssEscape = (v) =>
          window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\]/g, '\\$&');
        // Locate the element with the identical grammar tap() resolves, so the box
        // lands on exactly the control the replay tapped.
        let el = null;
        if (s.startsWith('key:')) {
          const body = s.slice(4);
          const ci = body.indexOf(':');
          const kind = ci >= 0 ? body.slice(0, ci) : '';
          const val = ci >= 0 ? body.slice(ci + 1) : body;
          if (kind === 'testid') {
            el =
              document.querySelector('[data-testid="' + cssEscape(val) + '"]') ||
              document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
          } else if (kind === 'id') {
            el = document.getElementById(val);
          } else if (kind === 'name') {
            el = document.querySelector('[name="' + cssEscape(val) + '"]');
          }
        } else if (s.startsWith('role:')) {
          const hash = s.indexOf('#');
          if (hash >= 0) {
            const role = s.slice('role:'.length, hash);
            const idx = parseInt(s.slice(hash + 1), 10);
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
            const roleOf = (n) => {
              const tag = n.tagName.toLowerCase();
              const ariaRole = (n.getAttribute('role') || '').toLowerCase();
              if (ariaRole) {
                if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox')
                  return 'textfield';
                if (ariaRole === 'heading') return 'header';
                if (ariaRole === 'img') return 'image';
                if (ariaRole === 'switch') return 'switch';
                if (ariaRole === 'link') return 'link';
                if (ariaRole === 'button') return 'button';
                if (ROLES[ariaRole]) return ariaRole;
              }
              if (tag === 'input') {
                const t = (n.getAttribute('type') || 'text').toLowerCase();
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
            const interactive = (n, r) => {
              const tag = n.tagName.toLowerCase();
              if (['a', 'button', 'select'].includes(tag)) return true;
              if (tag === 'input') {
                const t = (n.getAttribute('type') || 'text').toLowerCase();
                return !['text', 'password', 'email', 'number', 'search'].includes(t);
              }
              if (['button', 'link', 'menuitem', 'tab', 'checkbox', 'switch', 'radio'].includes(r))
                return true;
              if (n.hasAttribute('onclick') || n.tabIndex >= 0) return true;
              return false;
            };
            let seen = -1;
            const walk = (n) => {
              if (el) return;
              if (!visible(n)) {
                for (const c of n.children) walk(c);
                return;
              }
              const r = roleOf(n);
              if (interactive(n, r) && r === role) {
                seen++;
                if (seen === idx) {
                  el = n;
                  return;
                }
              }
              for (const c of n.children) walk(c);
            };
            const root = document.body || document.documentElement;
            if (root && idx >= 0) walk(root);
          }
        }
        if (!el) return null;
        // Bring the element into the recorded frame INSTANTLY (not smooth): a
        // smooth animation is still moving when we measure, so the rect would
        // diverge from the settled frame the video holds -- the box lands off the
        // element. An instant scroll settles in one frame, so the measured rect
        // equals the held frame. Wait a couple of frames for any reflow.
        try {
          el.scrollIntoView({ behavior: 'instant', block: 'center', inline: 'center' });
        } catch (_) {
          try {
            el.scrollIntoView({ block: 'center', inline: 'center' });
          } catch (__) {}
        }
        let lastY = -1,
          stable = 0;
        for (let i = 0; i < 20; i++) {
          await new Promise((r) => setTimeout(r, 50));
          const y = window.scrollY;
          if (y === lastY) {
            if (++stable >= 2) break;
          } else {
            stable = 0;
            lastY = y;
          }
        }
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0) return null;
        const vw = window.innerWidth || document.documentElement.clientWidth || 1;
        const vh = window.innerHeight || document.documentElement.clientHeight || 1;
        // Clamp the box inside the viewport (an inset) so a box always lands on
        // camera even when the element sits flush to an edge -- mirrors the web
        // overlay's clamp. box-overlay draws exactly this rect (scaled to pixels).
        const ins = 4;
        const left = Math.min(Math.max(r.left - 2, ins), Math.max(ins, vw - ins - 8));
        const top = Math.min(Math.max(r.top - 2, ins), Math.max(ins, vh - ins - 8));
        const w = Math.max(8, Math.min(r.width + 4, vw - left - ins));
        const h = Math.max(8, Math.min(r.height + 4, vh - top - ins));
        return { x: left, y: top, w, h, videoW: vw, videoH: vh };
      },
      { s: sel },
    )
    .catch(() => null);
}

// Remux the Playwright-recorded .webm to a clip.mov the host box-overlay step
// reads (record_native_clips looks for `clip.mov` by name). box-overlay
// re-encodes to h264 mp4 anyway, so a straight transcode to h264/mov is enough;
// returns true on success.
function remuxToMov(webm, mov) {
  if (!webm || !existsSync(webm)) return false;
  const r = spawnSync(
    'ffmpeg',
    [
      '-hide_banner',
      '-loglevel',
      'error',
      '-y',
      '-i',
      webm,
      '-c:v',
      'libx264',
      '-pix_fmt',
      'yuv420p',
      '-an',
      mov,
    ],
    { stdio: ['ignore', 'inherit', 'inherit'] },
  );
  return r.status === 0 && existsSync(mov);
}

// ── Multi-actor scenario client (the conductor protocol) ────────────────────
// Same wire protocol as the web runner / flutter explorer / tui backend: the
// host conductor owns identity (`GET /claim`) and ordering (`GET /next` +
// `POST /done`); this process plays ONE actor and only executes actions.

// Substitute ${VAR} from the environment (same contract as the web runner):
// journeys encode `secret:` fills as ${REPROIT_SECRET_<ACCT>_<FIELD>}
// placeholders so plaintext credentials never touch disk. Unset vars expand
// to "" (a missing credential types blank, which the app rejects).
function expandEnv(s) {
  return String(s).replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (_, name) => process.env[name] || '');
}

// Count VISIBLE elements matching a journey finder, for `expect: count`. Runs
// in the renderer (passed to page.evaluate). Same key grammar as tap(); any
// other finder is treated as a raw CSS selector. Byte-identical to the web
// runner's countMatching so `expect:` means the same thing on both surfaces.
function countMatching(finder) {
  const esc = (v) => (window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\]/g, '\\$&'));
  let sel = finder;
  if (finder.startsWith('key:')) {
    const body = finder.slice(4);
    const ci = body.indexOf(':');
    const kind = ci >= 0 ? body.slice(0, ci) : '';
    const val = ci >= 0 ? body.slice(ci + 1) : body;
    if (kind === 'testid')
      sel = '[data-testid="' + esc(val) + '"],[data-test-id="' + esc(val) + '"]';
    else if (kind === 'id') sel = '#' + esc(val);
    else if (kind === 'name') sel = '[name="' + esc(val) + '"]';
  }
  let els;
  try {
    els = document.querySelectorAll(sel);
  } catch (_) {
    return -1;
  }
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

// Fill a field located by the same key:/role: grammar as tap(), typing via the
// real keyboard so framework input handlers fire (port of the web runner's
// typeInto; Electron's renderer is a Playwright Page, so the same API drives
// it). A missing/unreachable/non-text target returns false so the caller
// reports a MISS rather than silently passing.
// Provenance ledger for the broken-asset oracle: every value the fuzzer types is
// recorded so brokenAssetScan can exclude an asset that only exists because a
// fuzzer-injected value was reflected into the DOM (mirrors the web runner).
const INJECTED_VALUES = new Set();
async function typeInto(page, sel, value) {
  if (value != null && String(value).length > 0) INJECTED_VALUES.add(String(value));
  const found = await page
    .evaluate(
      ({ s }) => {
        const visible = (el) => {
          const r = el.getBoundingClientRect();
          if (r.width === 0 || r.height === 0) return false;
          const st = getComputedStyle(el);
          return st.visibility !== 'hidden' && st.display !== 'none';
        };
        const cssEscape = (v) =>
          window.CSS && CSS.escape ? CSS.escape(v) : v.replace(/["\\]/g, '\\$&');
        let el = null;
        if (s.startsWith('key:')) {
          const body = s.slice(4);
          const ci = body.indexOf(':');
          if (ci < 0) return false;
          const kind = body.slice(0, ci);
          const val = body.slice(ci + 1);
          if (kind === 'testid') {
            el =
              document.querySelector('[data-testid="' + cssEscape(val) + '"]') ||
              document.querySelector('[data-test-id="' + cssEscape(val) + '"]');
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
          const roleOf = (el) => {
            const tag = el.tagName.toLowerCase();
            const ariaRole = (el.getAttribute('role') || '').toLowerCase();
            if (ariaRole === 'textbox' || ariaRole === 'searchbox' || ariaRole === 'combobox')
              return 'textfield';
            if (tag === 'input') {
              const t = (el.getAttribute('type') || 'text').toLowerCase();
              if (['checkbox', 'radio', 'range', 'button', 'submit', 'reset', 'image'].includes(t))
                return t;
              return 'textfield';
            }
            if (tag === 'textarea' || tag === 'select') return 'textfield';
            return ariaRole || tag;
          };
          let seen = -1,
            target = null;
          const walk = (el) => {
            if (target) return;
            if (!visible(el)) {
              for (const c of el.children) walk(c);
              return;
            }
            if (roleOf(el) === role) {
              seen++;
              if (seen === idx) {
                target = el;
                return;
              }
            }
            for (const c of el.children) walk(c);
          };
          const root = document.body || document.documentElement;
          if (root) walk(root);
          el = target;
        }
        if (!el || !visible(el)) return false;
        // Only type into things that hold text; a non-text target is a miss so the
        // caller treats it like a failed action rather than silently no-op'ing.
        const tag = el.tagName.toLowerCase();
        const isText =
          tag === 'textarea' ||
          (el.getAttribute &&
            (el.getAttribute('role') || '').toLowerCase().match(/textbox|searchbox|combobox/)) ||
          el.isContentEditable ||
          (tag === 'input' &&
            !['checkbox', 'radio', 'range', 'button', 'submit', 'reset', 'image'].includes(
              (el.getAttribute('type') || 'text').toLowerCase(),
            ));
        if (!isText) return false;
        try {
          el.focus();
        } catch (e) {}
        el.setAttribute('data-reproit-typed', '1');
        return true;
      },
      { s: sel },
    )
    .catch(() => false);
  if (!found) return false;
  // Type via the real keyboard so framework input handlers fire, then commit
  // with Enter. Clear any existing content first for determinism.
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
  } catch (e) {
    return false;
  }
  return true;
}

// Execute ONE scenario action, emitting the same FUZZ:ACT/MISS/ASSERT markers
// as the web runner's scenario path. `who` is this runner's role letter, for
// log attribution. `type:` values are env-expanded literals (secrets arrive
// resolved from the host); the web runner's adversarial-class tokens do not
// apply to authored scenario fills.
async function execScenarioAction(page, act, who) {
  log('FUZZ:ACT ' + who + ' ' + act);
  if (act.startsWith('shoot:')) {
    await shoot(page, act.slice('shoot:'.length));
    return;
  }
  if (act.startsWith('assert:')) {
    const body = act.slice('assert:'.length);
    if (body.startsWith('text=')) {
      const want = body.slice('text='.length);
      const ok = await page
        .evaluate((t) => !!(document.body && document.body.innerText.includes(t)), want)
        .catch(() => false);
      log(
        'FUZZ:ASSERT ' + (ok ? 'pass' : 'fail') + ' text=' + JSON.stringify(want) + ' actor=' + who,
      );
    } else if (body.startsWith('count:')) {
      const rest = body.slice('count:'.length);
      const eq = rest.lastIndexOf('=');
      const finder = eq >= 0 ? rest.slice(0, eq) : rest;
      const want = eq >= 0 ? parseInt(rest.slice(eq + 1), 10) : 0;
      const got = await page.evaluate(countMatching, finder).catch(() => -1);
      log(
        'FUZZ:ASSERT ' +
          (got === want ? 'pass' : 'fail') +
          ' count ' +
          finder +
          ' want=' +
          want +
          ' got=' +
          got +
          ' actor=' +
          who,
      );
    } else {
      log('FUZZ:ASSERT fail unsupported ' + body + ' actor=' + who);
    }
    await page.waitForTimeout(300);
    return;
  }
  if (act === 'back') {
    await page.goBack({ timeout: 3000 }).catch(() => {});
    await page.waitForTimeout(400);
    return;
  }
  if (act.startsWith('auth:')) {
    // Session-restore login is not wired on the Electron runner; use a
    // `login(<account>)` actor prelude (UI flow) for multi-user auth. No-op so
    // ordering still advances, but flag it loudly.
    log(
      'JOURNEY[a] step: auth-restore unsupported on electron runner; use ' + 'login() for ' + act,
    );
    await page.waitForTimeout(200);
    return;
  }
  if (act.startsWith('type:')) {
    const b = act.slice('type:'.length);
    const eq = b.lastIndexOf('=');
    const sel = eq >= 0 ? b.slice(0, eq) : b;
    const value = expandEnv(eq >= 0 ? b.slice(eq + 1) : '');
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

// Multi-actor: this runner is ONE actor. It drives the already-launched app
// window and pulls its next action from the host conductor (the strict
// step-order barrier), so N runners across N processes interleave exactly as
// the journey specifies. Universal wire protocol; only execScenarioAction is
// Electron-specific.
async function runScenarioActor(page) {
  const base = process.env.REPROIT_SCENARIO_BARRIER;
  // Role identity: an explicit label wins (each process gets its own env),
  // else claim a distinct role from the conductor, which hands out `a`, `b`,
  // ... atomically so two actors can never collide.
  let who = process.env.REPROIT_DEVICE;
  if (!who) {
    try {
      who = (await (await fetch(base + '/claim')).text()).trim();
    } catch (_) {
      who = '';
    }
    if (!who || who.startsWith('ERR')) who = 'a';
  }
  log('JOURNEY claimed role=' + who);
  await page.waitForTimeout(1200);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  for (let guard = 0; guard < 100000; guard++) {
    let body = 'WAIT';
    try {
      body = (await (await fetch(base + '/next?device=' + who)).text()).trim();
    } catch {
      await sleep(100);
      continue;
    }
    if (body === 'DONE') break;
    if (body === 'WAIT') {
      await sleep(40);
      continue;
    }
    const act = body.startsWith('ACT\t') ? body.slice(4) : body;
    await execScenarioAction(page, act, who);
    try {
      await fetch(base + '/done?device=' + who, { method: 'POST' });
    } catch (_) {}
  }
  await page.waitForTimeout(500); // flush a trailing pageerror before teardown
  log('JOURNEY DONE');
  log('All tests passed');
}

async function main() {
  if (!APP) {
    log('EXCEPTION CAUGHT BY REPROIT');
    log('REPROIT_APP (executable path or dev dir) required');
    log('═'.repeat(8));
    process.exit(0);
  }
  const launch = resolveElectronLaunch(APP);
  if (!launch) {
    log('EXCEPTION CAUGHT BY REPROIT');
    log('Could not resolve Electron binary from: ' + APP);
    log('═'.repeat(8));
    process.exit(0);
  }
  const fuzz = loadFuzz();
  // --record clip capture (route B): arm when this is a replay with a clip plan
  // {sel,label,oracle} + REPROIT_VIDEO_DIR. Playwright's recordVideo films the
  // renderer window (window-only, never the desktop -- the hard privacy rule).
  const clipPlan = fuzz.clip && typeof fuzz.clip.sel === 'string' ? fuzz.clip : null;
  const clipArmed = !!(VIDEO_DIR && fuzz.replay && clipPlan);
  // Pin the recorded video to a FIXED size AND emulate the renderer to the SAME
  // size (below) when filming a clip: without this, Playwright's Electron video
  // defaults to 800x600 and LETTERBOXES the renderer into it (uniform scale +
  // bottom padding), but the host box-overlay scales the box's x/y independently
  // (no padding model) -- so the box lands off the element. Equal capture and
  // renderer sizes give a 1:1 mapping (no letterbox), so the box lands exactly.
  const CLIP_W = 1200,
    CLIP_H = 800;
  const launchOpts = {
    executablePath: launch.executablePath,
    recordVideo: VIDEO_DIR
      ? { dir: VIDEO_DIR, ...(clipArmed ? { size: { width: CLIP_W, height: CLIP_H } } : {}) }
      : undefined,
  };
  if (launch.args) launchOpts.args = launch.args;
  // The release and native-gate workflows install the shared browser runtime
  // from runners/web/package-lock.json. Resolve from that package boundary so
  // ESM lookup does not depend on an accidental runners/node_modules hoist.
  const webRuntime = createRequire(new URL('./web/package.json', import.meta.url));
  const { _electron: electron } = webRuntime('playwright');
  const app = await electron.launch(launchOpts);
  // Install causal routing on Electron's browser context BEFORE waiting for the
  // first window. This includes renderer bootstrap traffic; attaching to the
  // page afterwards can miss startup config/API calls and cannot support a
  // hermetic claim.
  const electronContext = app.context();
  const causalRequests = new WeakMap();
  const capsulePath = process.env.REPROIT_CAPSULE;
  await installElectronWebSockets(electronContext, capsulePath);
  if (capsulePath) {
    const capsule = JSON.parse(readFileSync(capsulePath, 'utf8'));
    const exchanges = (capsule.exchanges || []).filter(
      (e) => e.required && /^(https?|sse)$/.test(e.protocol),
    );
    const used = new Set();
    await electronContext.route('**/*', async (route) => {
      const req = route.request();
      if (!['xhr', 'fetch', 'eventsource'].includes(req.resourceType())) return route.continue();
      const wanted = canonicalNetworkUrl(req.url());
      const idx = exchanges.findIndex(
        (e, i) =>
          !used.has(i) &&
          e.actor === NETWORK_ACTOR &&
          e.actionIndex === causalActionIndex &&
          String(e.method).toUpperCase() === req.method().toUpperCase() &&
          canonicalNetworkUrl(e.url) === wanted,
      );
      if (idx < 0) {
        log(`CAPSULE:MISS ${req.method()} ${req.url()} action=${causalActionIndex}`);
        return route.abort('blockedbyclient');
      }
      used.add(idx);
      const e = exchanges[idx];
      const headers = { ...(e.responseHeaders || {}) };
      const body =
        typeof e.responseBody === 'string' ? e.responseBody : JSON.stringify(e.responseBody ?? '');
      if (typeof e.responseBody !== 'string' && !headers['content-type'])
        headers['content-type'] = 'application/json';
      log(`CAPSULE:HIT ${e.id}`);
      return route.fulfill({ status: e.status, headers, body });
    });
    log(`CAPSULE:READY ${capsule.id || ''} exchanges=${exchanges.length}`);
  }
  log(`REPROIT:CAPABILITIES {"http":{"status":"captured"},"http_replay":{"status":"captured"}}`);
  electronContext.on('request', (req) => {
    if (
      !NETWORK_FILE ||
      capsulePath ||
      !['xhr', 'fetch', 'eventsource'].includes(req.resourceType())
    )
      return;
    try {
      const u = new URL(req.url());
      if (!/^https?:$/.test(u.protocol)) return;
      const headers = req.headers();
      const ordinal = causalOrdinal++;
      causalRequests.set(req, {
        id: `${NETWORK_ACTOR}-${causalActionIndex}-${ordinal}`,
        actionIndex: causalActionIndex,
        ordinal,
        headers: redactNetworkHeaders(headers),
        body: parseNetworkBody(req.postData(), headers['content-type'] || ''),
      });
    } catch (_) {}
  });
  electronContext.on('response', async (resp) => {
    try {
      const req = resp.request();
      const causal = causalRequests.get(req);
      if (!causal || !NETWORK_FILE || capsulePath) return;
      const headers = await resp.allHeaders().catch(() => ({}));
      const contentType = headers['content-type'] || '';
      let body;
      if (/text\/event-stream/i.test(contentType)) {
        const sse = redactSse(await resp.text().catch(() => ''));
        body = sse.body;
        if (!sse.supported)
          log(
            'REPROIT:CAPABILITIES {"sse":{"status":"unsupported","detail":"non-JSON ' +
              'event cannot be safely persisted"},"sse_replay":{"status":' +
              '"unsupported"}}',
          );
      } else if (/json/i.test(contentType))
        body = parseNetworkBody(await resp.text().catch(() => ''), contentType);
      else if (headers['content-length'])
        body = `<reproit:body:length=${headers['content-length']}>`;
      appendNetworkFact({
        id: causal.id,
        actor: NETWORK_ACTOR,
        actionIndex: causal.actionIndex,
        ordinal: causal.ordinal,
        protocol: /text\/event-stream/i.test(contentType)
          ? 'sse'
          : new URL(resp.url()).protocol.replace(':', ''),
        method: req.method(),
        url: resp.url(),
        requestHeaders: causal.headers,
        requestBody: causal.body,
        status: resp.status(),
        responseHeaders: redactNetworkHeaders(headers),
        responseBody: body,
        required: true,
      });
    } catch (_) {}
  });
  const page = await app.firstWindow();
  const clipVideo = clipArmed ? page.video() : null;
  const recordStart = Date.now();
  if (clipArmed) {
    // Emulate the renderer at the capture size (CDP viewport emulation, the same
    // mechanism the zoom-reflow check uses on Electron) so the film is 1:1 with
    // the element rects we measure. Best-effort: if it does not take, the box is
    // still drawn, just with the framework's own scaling.
    try {
      await page.setViewportSize({ width: CLIP_W, height: CLIP_H });
    } catch (_) {}
    // Small lead-in so the first frames exist before the replay drives the app.
    await page.waitForTimeout(400);
  }
  page.on('pageerror', (err) => {
    log('EXCEPTION CAUGHT BY ELECTRON RENDERER');
    log('The following error was thrown:');
    log(String(err && err.message ? err.message : err));
    for (const line of String(err && err.stack ? err.stack : '')
      .split('\n')
      .slice(0, 8))
      log(line);
    log('═'.repeat(8));
  });

  // Capture determinism: ask the renderer for prefers-reduced-motion: reduce
  // (page.emulateMedia drives the same CDP media emulation the web tier uses on
  // its context; Electron's renderer is Chromium), pinning animation-dependent
  // layout so snapshots/pixels are stable across runs. Best-effort.
  try {
    await page.emulateMedia({ reducedMotion: 'reduce' });
  } catch (e) {
    /* best-effort */
  }

  // Multi-actor scenario: this process plays one actor, pulling from the
  // conductor; the fuzz walk and its oracles do not run.
  if (process.env.REPROIT_SCENARIO_BARRIER) {
    log('JOURNEY[a] step: scenario actor=' + (process.env.REPROIT_DEVICE || 'a'));
    await runScenarioActor(page);
    await app.close();
    return;
  }

  // BROKEN-ROUTE oracle (ported from the web runner): record the HTTP status of
  // main-frame DOCUMENT navigations, keyed by URL pathname. A document that came
  // back 404 / 410 / 5xx is a dead route the app linked to. NOT 401/403 (auth
  // gates) or 429 (rate limit), which are intentional >= 400 responses, never a
  // broken link. The status is structural + locale-invariant, so this is
  // false-positive-free. Same-origin only; the app origin is pinned from the
  // first document response (an Electron app loads its own http(s) origin or a
  // file:// bundle -- both have a stable origin). A file:// origin is "null", so
  // the same-origin filter naturally limits the probe to http(s) apps; a packaged
  // file:// app has no server status to read and stays an honest gap there.
  const navStatus = {};
  const seenLinks = new Map(); // pathname -> source sig (first wins)
  let appOrigin = null;
  page.on('response', async (resp) => {
    try {
      const req = resp.request();
      if (req.frame() !== page.mainFrame() || req.resourceType() !== 'document') return;
      const u = new URL(resp.url());
      if (u.protocol !== 'http:' && u.protocol !== 'https:') return;
      if (appOrigin == null) appOrigin = u.origin; // pin from the first document
      if (u.origin !== appOrigin) return;
      navStatus[normalizePathname(u.pathname)] = resp.status();
    } catch (e) {
      /* ignore */
    }
  });

  // DUPLICATE-SUBMIT probe support, OPT-IN per run via REPROIT_DUPSUBMIT=1
  // (same contract as the web runner): double-firing real submit actions during
  // a walk changes exploration semantics (an order really is placed twice), so
  // the probe never runs unless the operator asked for it. While a tap probe is
  // armed (dupReqLog non-null, set in the tap branch), every first-party
  // non-GET request in the window between the first click and the settle is
  // recorded as "METHOD url"; the tap branch groups them and reports a pair
  // that fired twice. First-party: same origin as the pinned app origin for an
  // http(s)-served app; a file:// app has no origin to pin, and every request
  // its renderer fires is the app's own code, so any http(s) non-GET counts
  // there. A page-level listener (not in-page patching) so plain form
  // submissions count exactly like fetch/XHR. null = disarmed, zero overhead
  // on a normal walk.
  const DUPSUBMIT = process.env.REPROIT_DUPSUBMIT === '1';
  // LISTENER-LEAK probe support (opt-in, REPROIT_LISTENERLEAK=1): same contract
  // as the web runner -- an init-script wrap on add/removeEventListener plus an
  // immediate install on the already-loaded renderer document (the app launched
  // before we attached, so addInitScript alone would only cover later reloads).
  const LISTENERLEAK = process.env.REPROIT_LISTENERLEAK === '1';
  let dupReqLog = null;
  page.on('request', (req) => {
    if (!dupReqLog) return;
    try {
      const method = req.method();
      if (method === 'GET') return;
      const u = new URL(req.url());
      if (u.protocol !== 'http:' && u.protocol !== 'https:') return;
      if (appOrigin && u.origin !== appOrigin) return;
      dupReqLog.push(method + ' ' + req.url());
    } catch (e) {
      /* ignore */
    }
  });

  // Install the Long Tasks observer (jank/hang watchdog) BEFORE the renderer
  // settles so it is live for every action. addInitScript re-runs it on every
  // document, so it survives in-app navigations and reloads.
  await installLongTaskObserver(page);
  if (LISTENERLEAK) {
    await page.addInitScript(installListenerLeakCounter);
    // Wrap the CURRENT document too (idempotent): the Electron app is already
    // loaded, so the init script would otherwise only take effect after a reload.
    await page.evaluate(installListenerLeakCounter).catch(() => {});
  }

  // Tier-2 pixel-flicker oracle (gated): lazily load the pngjs decoder + the
  // host-pure probe/flicker helpers only when REPROIT_FLICKER_PIXELS=1, so this
  // module stays import-safe for the parity test and never hard-depends on pngjs.
  // Any import failure leaves PIXEL null, which keeps the oracle a silent no-op.
  if (FLICKER_PIXELS) {
    try {
      const [{ PNG }, probe, flick] = await Promise.all([
        import('pngjs'),
        import('./web/probe.mjs'),
        import('./web/flicker-oracle.mjs'),
      ]);
      PIXEL = {
        PNG,
        changedFraction: probe.changedFraction,
        transientDivergence: flick.transientDivergence,
      };
    } catch (_) {
      PIXEL = null; /* pixel-flicker unavailable: stays silent */
    }
  }

  log('JOURNEY claimed role=a');
  await page.waitForTimeout(1200);
  // BOT-WALL guard (defensive, mirrors the web runner): a local Electron shell is
  // not normally WAF-fronted, but if the app loads a remote URL that returns a
  // challenge interstitial the runner never reached the app -- report UNSCANNABLE
  // with zero findings rather than flagging the interstitial.
  {
    const wall = await detectBotWall(page);
    if (wall) {
      const diag =
        `target is behind a ${wall.vendor} bot-challenge (${wall.marker}); ` +
        'reproit could not reach the app.';
      log(
        'EXPLORE:UNSCANNABLE ' +
          JSON.stringify({
            reason: 'bot-wall',
            vendor: wall.vendor,
            marker: wall.marker,
            diagnostic: diag,
          }),
      );
      log('JOURNEY[a] step: UNSCANNABLE - ' + diag);
      log('JOURNEY DONE');
      log('All tests passed');
      try {
        await app.close();
      } catch (_) {}
      return;
    }
  }
  const seen = new Set(),
    tried = new Set();
  const pick = rng(fuzz.seed || 0);
  // CDP session on the renderer (Electron's renderer is Chromium) for the
  // ground-truth operability probe: real click/pointer listeners on elements and
  // the document/body delegation pattern via DOMDebugger.getEventListeners.
  let gtCdp = null;
  try {
    gtCdp = await page.context().newCDPSession(page);
  } catch (e) {
    gtCdp = null;
  }

  // Layer-3 opt-in value-node selectors from reproit.yaml (empty if none).
  const valueNodeSelectors = loadValueNodes();
  if (valueNodeSelectors.length) log(`JOURNEY[a] step: value_nodes=${valueNodeSelectors.length}`);

  // Layer-1 hard cap (docs/signature.md "Value-state"): per structural node,
  // track the DISTINCT value-class combinations seen. Once a node exceeds
  // VALUE_CLASS_CAP, fall back to its structural-only signature for the rest of
  // the run so an adversarial value generator cannot explode the graph.
  const valueCombos = new Map(); // structuralSig -> Set of V: sections
  const cappedNodes = new Set(); // structuralSig that hit the cap
  // The EFFECTIVE signature for a snapshot, applying the runner-local cap: the
  // full value-folded sig unless this structural node is capped, then structural.
  function effectiveSig(snap) {
    if (cappedNodes.has(snap.structuralSig)) return snap.structuralSig;
    if (snap.vsection) {
      let set = valueCombos.get(snap.structuralSig);
      if (!set) {
        set = new Set();
        valueCombos.set(snap.structuralSig, set);
      }
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
    log(
      'FUZZ:OBS ' +
        JSON.stringify({
          sig: snap.sig,
          ...(snap.anchor ? { route: snap.anchor } : {}),
          labels: snap.labels.slice(0, 24),
          elements: snap.tappables.slice(0, 24).map((e) => ({ role: e.role })),
        }),
    );
    if (!seen.has(snap.sig)) {
      seen.add(snap.sig);
      // sig: STRUCTURAL (roles + tree shape + stable developer keys),
      //      locale-invariant.
      // labels: DISPLAY-ONLY visible text (map show), never in the sig.
      // elements: structural selectors for replay; `nokey` flags a tappable
      //           with no stable id (data-testid/id/name).
      log(
        'EXPLORE:STATE ' +
          JSON.stringify({
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
          }),
      );
      // DOM/layout overflow for this newly-seen state, keyed by the SAME sig.
      const overflow1 = await page.evaluate(layoutOverflowScan).catch(() => null);
      await page.waitForTimeout(120);
      const overflow2 = await page.evaluate(layoutOverflowScan).catch(() => null);
      const overflow = confirmLayoutOverflow(overflow1, overflow2);
      if (overflow.checks.length || !overflow.complete) {
        log(
          'EXPLORE:OVERFLOW ' +
            JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              ...overflow,
            }),
        );
      }
      // Operability/accessibility ground truth can mutate the DOM, so it runs
      // after every state-present layout scan.
      await emitGroundtruth(page, gtCdp, snap.sig);
      // CONTENT-BUG for this newly-seen state, keyed by the SAME sig. Pure
      // DOM/label scan (no pixels, no timing), so it reproduces on replay. Only
      // emitted when a broken-content artifact is actually rendered.
      const cbug = await page.evaluate(detectContentBugs, [...INJECTED_VALUES]).catch(() => null);
      if (cbug && cbug.length) {
        log(
          'EXPLORE:CONTENTBUG ' +
            JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              items: cbug,
            }),
        );
      }
      // ZERO-CONTRAST: text whose resolved foreground exactly equals its
      // composited backdrop is invisible where it must be read. Pure in-page
      // getComputedStyle scan, shared verbatim from the web oracle (identical
      // Chromium renderer), so it reproduces on replay.
      const zc = await page.evaluate(zeroContrastScan).catch(() => null);
      if (zc && zc.length) {
        log(
          'EXPLORE:ZEROCONTRAST ' +
            JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              items: zc,
            }),
        );
      }
      // OCCLUSION + SECURITY: same pure-DOM hygiene scans as the web runner,
      // shared from web/hygiene-oracles.mjs (Chromium renderer, identical API).
      const occ1 = await page.evaluate(occlusionScan).catch(() => null);
      let occ = occ1;
      if (occ1 && occ1.length) {
        await page.waitForTimeout(300);
        const occ2 = await page.evaluate(occlusionScan).catch(() => null);
        occ = confirmOcclusions(occ1, occ2 || []);
      }
      if (occ && occ.length) {
        log(
          'EXPLORE:OCCLUSION ' +
            JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              items: occ,
            }),
        );
      }
      const sec = await page.evaluate(securityScan).catch(() => null);
      if (sec && sec.length) {
        log(
          'EXPLORE:SECURITY ' +
            JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              items: sec,
            }),
        );
      }
      // BLANK-SCREEN: the state rendered NOTHING -- zero visible text nodes,
      // zero tappable controls, zero visible media -- in a non-empty viewport
      // (the white-screen-of-death: a renderer mount that threw before render).
      // observe() runs after the action's settle wait like every scan here,
      // and the scan itself requires a laid-out document.body, so a page
      // still loading never fires. Structural DOM emptiness, no pixels, so it
      // reproduces on replay. Silent when the state shows any content.
      let blank = await page.evaluate(blankScreenScan).catch(() => null);
      // Settle-then-recheck: a candidate-blank state may be a MID-LOAD blank frame,
      // not a WSOD. Only a state STILL blank AFTER settle fires (mirrors web runner).
      if (blank && blank.length) {
        await settleForSignature(page);
        blank = await page.evaluate(blankScreenScan).catch(() => null);
      }
      if (blank && blank.length) {
        log(
          'EXPLORE:BLANKSCREEN ' +
            JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              items: blank,
            }),
        );
      }
      // APP-INVARIANT: the app's OWN predicates, registered via the SDK
      // (ReproIt.invariant, pushed to window.__reproit_invariants). Same
      // runner-triggered model as the web runner; the Electron renderer is
      // Chromium, so page.evaluate reads the page global directly. Each test is
      // isolated; falsy/throw/{ok:false} is a violation. FP-free (the app owns
      // the ground truth); silent when none registered or all held.
      const invViolations = await page
        .evaluate(() => {
          const reg = window.__reproit_invariants || [];
          const out = [];
          for (let i = 0; i < reg.length; i++) {
            const it = reg[i];
            if (!it || typeof it.test !== 'function') continue;
            let ok = true,
              message = '';
            try {
              const r = it.test();
              if (r && typeof r === 'object') {
                ok = !!r.ok;
                message = r.message ? String(r.message) : '';
              } else {
                ok = !!r;
              }
            } catch (e) {
              ok = false;
              message = e && e.message ? String(e.message) : String(e);
            }
            if (!ok) out.push({ id: String(it.id), message });
          }
          return out;
        })
        .catch(() => null);
      if (invViolations && invViolations.length) {
        log(
          'EXPLORE:INVARIANT ' +
            JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              items: invViolations,
            }),
        );
      }
      // BROKEN-ASSET: dead subresources rendered in this state -- an img that
      // completed with no pixels, a FontFace whose load errored, rendered
      // tofu (a visible U+FFFD). Pure DOM/resource status facts; running
      // after the settle wait means loads have resolved, so a still-loading
      // asset never false-positives. Silent when every asset is healthy.
      const assets = await page.evaluate(brokenAssetScan, [...INJECTED_VALUES]).catch(() => null);
      if (assets && assets.length) {
        log(
          'EXPLORE:BROKENASSET ' +
            JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              items: assets,
            }),
        );
      }
      // DYNAMIC-TYPE clip (the OS-text-scale sibling of zoom-reflow): bump the
      // root font-size (the rem/em scale) and flag content that then clips or a
      // control that is lost or shrinks below the min target size. Self-restoring;
      // skipped under the framebuffer probe (it reloads the page). Silent when the
      // route scales cleanly. Same self-contained scan as the web tier (Electron's
      // renderer is Chromium).
      if (!PROBE) {
        // SCROLL ROUND-TRIP: scroll the primary list away and back and flag
        // content that differs at a pinned offset (a list-recycling bug).
        // Self-restoring; value-state normalized out. Silent when the list is
        // stable or there is no scroller.
        const srt = await page.evaluate(scrollRoundTripScan).catch(() => null);
        if (srt && srt.length) {
          log(
            'EXPLORE:SCROLLROUNDTRIP ' +
              JSON.stringify({
                sig: snap.sig,
                ...(snap.anchor ? { route: snap.anchor } : {}),
                items: srt,
              }),
          );
        }
        // DEAD-INPUT: a trusted wheel over a scrollable region eaten by an
        // invisible overlay is a broken input pipeline. Playwright over the
        // Electron renderer provides the same trusted page.mouse.wheel /
        // keyboard the web probe uses, so the oracle ports verbatim.
        const dead = await deadInputProbe(page).catch(() => []);
        if (dead.length) {
          log(
            'EXPLORE:DEADINPUT ' +
              JSON.stringify({
                sig: snap.sig,
                ...(snap.anchor ? { route: snap.anchor } : {}),
                items: dead,
              }),
          );
        }
      }
      // BROKEN-ROUTE: this state's document came back with a status that means the
      // resource is GENUINELY GONE -- 404 or 410 ONLY. Not 401/403 (auth gates),
      // 429 (rate limit), 3xx (redirect), 405/501 (method), or 5xx (transient
      // server error) -- none of those is a broken LINK. Looked up by bare pathname
      // (snap.anchor), keyed on the SAME sig.
      const status = snap.anchor ? navStatus[snap.anchor] : undefined;
      if (typeof status === 'number' && (status === 404 || status === 410)) {
        // SPA SOFT-404 guard: a static host can answer a deep path with 404 yet
        // still serve index.html so the client router renders the correct screen.
        // If the current screen is a real app view (filled mount, real content, no
        // not-found heading), the 404 status is not a broken route (mirrors web).
        const view = await page.evaluate(soft404View).catch(() => null);
        if (!isSoftHandled(view)) {
          log(
            'EXPLORE:BROKENROUTE ' +
              JSON.stringify({
                sig: snap.sig,
                ...(snap.anchor ? { route: snap.anchor } : {}),
                status,
              }),
          );
        }
      }
    }
    // Record same-origin APP link targets on this page (dedup by pathname, first
    // source state wins) for the end-of-crawl broken-route link check. Exclude a
    // `download` link and an href ending in a file/asset extension: the probe
    // should only test navigable app routes, never a downloadable asset.
    try {
      // Shared collector: skips rel=nofollow/external, form-submit, javascript:/
      // mailto: links, and asset extensions; honors <base href>; normalizes the
      // trailing slash (mirrors the web runner's broken-route tightening).
      const links = await page.evaluate(collectRouteLinks, ASSET_EXT_SOURCE);
      for (const p of links) if (!seenLinks.has(p)) seenLinks.set(p, snap.sig);
    } catch (_) {}
    return snap;
  };

  let current = await observe(),
    stuck = 0;
  const prefix = fuzz.prefix || null,
    replay = fuzz.replay || null;
  const prefixLen = prefix ? prefix.length : 0;
  const budget = replay ? replay.length : (fuzz.budget || ACTION_BUDGET) + prefixLen;
  const exercisedChoiceStates = new Set(); // sigs whose choice components were exercised
  // A recorded replay clip (the annotate tier replays with video): the
  // duplicate-submit double dispatch must never fire on a clip -- the clip has
  // to show the app's real single-click behavior. Matches the web runner.
  const recording = !!(replay && VIDEO_DIR);
  // DUPLICATE-SUBMIT probe: (from sig, action) pairs already double-dispatched,
  // so each submit-like control is probed (and reported) at most once.
  const dupProbed = new Set();
  // ZOOM-REFLOW (WCAG 1.4.10 Reflow, EAA-mandatory), ported from the web
  // runner: re-render the CURRENT route at 200% zoom by halving the viewport's
  // CSS size, then flag content that breaks (two-dimensional scrolling, a
  // pre-zoom-visible tappable collapsed below 1px -- see zoomReflowScan; a
  // responsively HIDDEN control is intentional adaptation and never fires).
  // An Electron window has no Playwright-pinned viewport (the window is a real
  // BrowserWindow), but page.setViewportSize() still drives CDP viewport
  // emulation on the renderer. VERIFIED live below: the scan only runs when
  // innerWidth actually halved, so a window where the emulation does not take
  // stays silent instead of scanning a full-width layout against halved
  // expectations. Once per distinct route (zoomChecked), never in replay (a
  // recorded clip must not jump viewports) or probe mode. Self-restoring: the
  // original CSS size is always put back.
  const zoomChecked = new Set();
  async function zoomReflowCheck(sig, route) {
    let vp = null;
    try {
      vp = await page.evaluate(() => ({ w: window.innerWidth, h: window.innerHeight }));
      if (!vp || !(vp.w > 0 && vp.h > 0)) {
        vp = null;
        return;
      }
      const preKeys = await page.evaluate(zoomTappableKeys);
      await page.setViewportSize({ width: Math.round(vp.w / 2), height: Math.round(vp.h / 2) });
      await page.waitForTimeout(350);
      const zw = await page.evaluate(() => window.innerWidth);
      if (Math.abs(zw - Math.round(vp.w / 2)) <= 2) {
        const items = await page.evaluate(zoomReflowScan, preKeys).catch(() => null);
        if (items && items.length) {
          log('EXPLORE:ZOOMREFLOW ' + JSON.stringify({ sig, ...(route ? { route } : {}), items }));
        }
      }
    } catch (_) {
    } finally {
      // Restore the original CSS size (layout-sensitive oracles depend on it).
      if (vp) {
        try {
          await page.setViewportSize({ width: vp.w, height: vp.h });
          await page.waitForTimeout(350);
        } catch (_) {}
      }
    }
  }
  // ZOOM-REFLOW for the start route: the walk's tap-edge check only covers
  // routes NAVIGATED to, so the launch screen gets its zoomed re-render here.
  if (!replay && !PROBE && current.anchor && !zoomChecked.has(current.anchor)) {
    zoomChecked.add(current.anchor);
    await zoomReflowCheck(current.sig, current.anchor);
  }
  // ROTATION / BACKGROUND-RESTORE (lifecycle-metamorphic), ported from the web
  // runner. The Electron renderer is Chromium, so a device rotation is emulated
  // by swapping the CDP viewport width/height and a background/foreground by the
  // visibilitychange/pagehide-pageshow lifecycle events. Each distinct state sig
  // is transform-tested once. See rotationCheck / backgroundCheck below.
  const rotChecked = new Set();
  const bgChecked = new Set();
  // ROTATION-stability: swap the viewport (portrait <-> landscape), reflow, then
  // rotate BACK to the original orientation and re-observe. A correct screen
  // rebuilds the SAME structure once the original orientation is restored; a
  // permanent loss regresses the STRUCTURAL signature (value-state excluded).
  // Round-trip identity is false-positive-free. Guarded on the pre-transform
  // state having content; self-restoring. Returns the re-observed state.
  async function rotationCheck(snap) {
    const expected = snap.structuralSig;
    let vp = null;
    try {
      vp = await page.evaluate(() => ({ w: window.innerWidth, h: window.innerHeight }));
      if (!vp || !(vp.w > 0 && vp.h > 0)) {
        vp = null;
      } else {
        await page.setViewportSize({ width: vp.h, height: vp.w });
        await page.waitForTimeout(350);
      }
    } catch (_) {}
    if (vp) {
      try {
        await page.setViewportSize({ width: vp.w, height: vp.h });
        await page.waitForTimeout(350);
      } catch (_) {}
    }
    const after = await observe();
    if (snap.tappables && snap.tappables.length > 0 && after.structuralSig !== expected) {
      log(
        'EXPLORE:ROTATION ' +
          JSON.stringify({
            sig: snap.sig,
            ...(snap.anchor ? { route: snap.anchor } : {}),
            expected,
            got: after.structuralSig,
          }),
      );
    }
    return after;
  }
  // BACKGROUND-RESTORE-stability: background the renderer (visibilitychange ->
  // hidden, pagehide, blur) then restore it (visible, pageshow, focus) and
  // re-observe. A correct app returns to the SAME screen with state intact; a
  // regression changes the STRUCTURAL signature. No size change; guarded on the
  // pre-transform state having content; self-restoring. Returns the re-observed
  // state.
  async function backgroundCheck(snap) {
    const expected = snap.structuralSig;
    try {
      await page.evaluate(() => {
        try {
          Object.defineProperty(document, 'visibilityState', {
            configurable: true,
            get: () => 'hidden',
          });
        } catch (_) {}
        try {
          Object.defineProperty(document, 'hidden', { configurable: true, get: () => true });
        } catch (_) {}
        document.dispatchEvent(new Event('visibilitychange'));
        window.dispatchEvent(new Event('pagehide'));
        window.dispatchEvent(new Event('blur'));
      });
      await page.waitForTimeout(300);
      await page.evaluate(() => {
        try {
          Object.defineProperty(document, 'visibilityState', {
            configurable: true,
            get: () => 'visible',
          });
        } catch (_) {}
        try {
          Object.defineProperty(document, 'hidden', { configurable: true, get: () => false });
        } catch (_) {}
        document.dispatchEvent(new Event('visibilitychange'));
        window.dispatchEvent(new Event('pageshow'));
        window.dispatchEvent(new Event('focus'));
      });
      await page.waitForTimeout(300);
    } catch (_) {}
    const after = await observe();
    if (snap.tappables && snap.tappables.length > 0 && after.structuralSig !== expected) {
      log(
        'EXPLORE:BGRESTORE ' +
          JSON.stringify({
            sig: snap.sig,
            ...(snap.anchor ? { route: snap.anchor } : {}),
            expected,
            got: after.structuralSig,
          }),
      );
    }
    return after;
  }
  // LISTENER-LEAK (opt-in, REPROIT_LISTENERLEAK=1), ported from the web runner:
  // drive N revisits of a route via history back/forward (client-side, the
  // init-script listener tally survives) and watch the live listener count
  // (adds - removes) and the attached DOM-node count for a MONOTONIC climb that a
  // stable route never shows. Once per route (leakChecked), never in
  // replay/probe mode. Self-restoring: back/forward net to the entry we started
  // on. Excludes the first sample as warmup (the route's one-time persistent
  // mount).
  const leakChecked = new Set();
  async function listenerLeakCheck(route) {
    const CYCLES = 5,
      MIN_RISE = 5;
    const samples = [];
    try {
      for (let i = 0; i < CYCLES; i++) {
        await page.goBack({ timeout: 3000 }).catch(() => {});
        await page.waitForTimeout(250);
        await page.goForward({ timeout: 3000 }).catch(() => {});
        await page.waitForTimeout(250);
        const snap = await snapshot(page, valueNodeSelectors).catch(() => null);
        if (!snap || snap.anchor !== route) return;
        const s = await page.evaluate(listenerLeakSample).catch(() => null);
        if (!s) return;
        samples.push(s);
      }
    } catch (_) {
      return;
    }
    if (samples.length < 3) return;
    const items = [];
    const consider = (kind, series) => {
      for (let i = 1; i < series.length; i++) if (!(series[i] > series[i - 1])) return;
      const rise = series[series.length - 1] - series[0];
      if (rise >= MIN_RISE) items.push({ kind, first: series[0], last: series[series.length - 1] });
    };
    const post = samples.slice(1);
    consider(
      'listeners',
      post.map((s) => s.live),
    );
    consider(
      'nodes',
      post.map((s) => s.nodes),
    );
    if (items.length) {
      log('EXPLORE:LISTENERLEAK ' + JSON.stringify({ route, visits: post.length, items }));
    }
  }
  // LEAK sampler: in REPLAY mode (the `--soak` tier writes {"replay":[...]}),
  // sample the v8 heap at the start and after every action, so the Rust soak
  // oracle gets a heap-vs-time series. Off outside replay. t0 anchors t_ms.
  const t0 = Date.now();
  if (replay) await sampleHeap(page, gtCdp, 0);
  for (let a = 0; a < budget && stuck < 3; a++) {
    // LEAK sampler: in replay mode, sample once per action (fires BEFORE acting,
    // so action a's sample reflects the heap after the previous action settled).
    if (replay && a > 0) await sampleHeap(page, gtCdp, Date.now() - t0);
    // LIFECYCLE-metamorphic oracles (rotation, background-restore), ported from
    // the web runner: once per distinct state, apply a device-lifecycle transform
    // and assert the structural signature survives it. Self-restoring, so
    // `current` is refreshed to the (restored) reality; never in replay/probe.
    if (!replay && !PROBE) {
      if (!rotChecked.has(current.sig)) {
        rotChecked.add(current.sig);
        current = await rotationCheck(current);
      }
      if (!bgChecked.has(current.sig)) {
        bgChecked.add(current.sig);
        current = await backgroundCheck(current);
      }
    }
    // COMPONENT-CHOICE differential (fuzz only, not replay), ported from the web
    // runner. The Electron renderer is Chromium, so the SAME self-contained in-
    // page pass the web runner uses runs here over page.evaluate: it finds the
    // page's choice components (native <select>, ARIA tab/radio groups, button-
    // cluster pickers), exercises each option, measures the global-layout effect,
    // and returns the outlier(s) using the SHARED threshold rule. Non-destructive
    // (it restores each component) and once per state per seed. Each returned
    // finding becomes an EXPLORE:CHOICEBUG keyed by the current sig.
    if (!replay && !exercisedChoiceStates.has(current.sig)) {
      exercisedChoiceStates.add(current.sig);
      const findings = await page
        .evaluate(choiceAnomalyInPage, {
          settleMs: 600,
          ratio: CHOICE_OUTLIER_RATIO,
          minMag: CHOICE_MIN_MAGNITUDE,
          choiceRoles: CHOICE_ROLES,
        })
        .catch(() => []);
      let emitted = false;
      for (const f of findings || []) {
        emitted = true;
        log(
          'EXPLORE:CHOICEBUG ' +
            JSON.stringify({
              from: current.sig,
              role: f.role,
              outlier: f.outlier,
              magnitude: f.magnitude,
              siblingMedian: f.siblingMedian,
            }),
        );
      }
      if (emitted) {
        current = await observe();
        continue;
      }
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
      const contractActions = new Set(fuzz.contractActions || []);
      const weights = options.map((o) => (contractActions.has(o) ? 4 : 1) / (1 + (ew[o] || 0)));
      const total = weights.reduce((x, y) => x + y, 0);
      let r = (pick(1 << 20) / (1 << 20)) * total;
      act = options[options.length - 1];
      for (let k = 0; k < options.length; k++) {
        r -= weights[k];
        if (r <= 0) {
          act = options[k];
          break;
        }
      }
    } else {
      act = null;
      for (const el of current.tappables) {
        if (!tried.has(current.sig + '|' + el.sel)) {
          act = 'tap:' + el.sel;
          break;
        }
      }
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
      current = next;
      continue;
    }
    const sel = act.slice('tap:'.length);
    tried.add(current.sig + '|' + sel);
    const before = current.sig;
    const beforeContent = current.content;
    const beforeAnchor = current.anchor;
    await page
      .evaluate(() => {
        window.__reproitLongTasks = [];
      })
      .catch(() => {}); // jank/hang: drop pre-action longtasks
    // FOCUS-LOSS: record the pre-tap activeElement + open dialog count and arm
    // the probe (tap()'s doClick then focuses the control before clicking, the
    // way a real user click does). Checked after the settle below.
    await page.evaluate(focusLossArm).catch(() => {});
    // DUPLICATE-SUBMIT probe (opt-in, REPROIT_DUPSUBMIT=1): when this tap
    // targets a button, dispatch a SECOND click ~120ms after the first and
    // record every first-party non-GET request over the window, so a submit
    // handler with no double-activation guard is caught firing the same
    // (method, url) twice. Armed BEFORE the first click so its request counts;
    // the in-page eligibility check between the clicks confirms the control is
    // actually submit-like. Once per (from, action); never on a recorded clip.
    // Never armed on a replay: a replay must reproduce from the RECORDED
    // action sequence alone (see the web runner's dupProbe note).
    const dupTapTarget = DUPSUBMIT && !replay ? current.tappables.find((e) => e.sel === sel) : null;
    const dupProbe =
      DUPSUBMIT &&
      !replay &&
      !recording &&
      !!dupTapTarget &&
      dupTapTarget.role === 'button' &&
      !dupProbed.has(before + '|tap:' + sel);
    let dupUrlBefore = null;
    if (dupProbe) {
      dupProbed.add(before + '|tap:' + sel);
      dupUrlBefore = page.url();
      dupReqLog = [];
    }
    const tapPix = await startScreencastCapture(gtCdp); // Tier-2 (gated): record presented frames
    if (!(await tap(page, sel))) {
      if (tapPix) await tapPix.stop();
      dupReqLog = null;
      log('FUZZ:MISS ' + act);
      stuck++;
      continue;
    }
    // DUPLICATE-SUBMIT double dispatch: the second click, ~120ms after the
    // first -- the probe's rapid double activation IN PLACE OF the walk's usual
    // single click. Skipped when the first click already changed the URL (the
    // navigation legitimately swallows a second click: no probe, no finding) or
    // when the resolved element is not submit-like in-page (a submit-type
    // control inside a form qualifies even without a matching accessible name).
    let dupDispatched = false;
    if (dupProbe && dupReqLog) {
      await page.waitForTimeout(120);
      const eligible = await page.evaluate(dupSubmitEligible).catch(() => false);
      if (eligible && page.url() === dupUrlBefore) {
        dupDispatched = await tap(page, sel).catch(() => false);
        // RECORD the second dispatch into the action sequence (FUZZ:ACT) only
        // when it actually fired: the walk continues from the post-double-click
        // state, so a kept repro must replay both clicks or it diverges.
        if (dupDispatched) log('FUZZ:ACT tap:' + sel);
      }
      if (!dupDispatched) dupReqLog = null;
    }
    await page.waitForTimeout(700);
    // DUPLICATE-SUBMIT verdict: group the captured window's first-party non-GET
    // requests by (method, url); the same pair firing twice or more while the
    // URL never changed is the bug (the handler has no double-activation
    // guard). Reported once per (from, action); the map layer dedupes again.
    if (dupProbe && dupReqLog) {
      const captured = dupReqLog;
      dupReqLog = null;
      if (dupDispatched && page.url() === dupUrlBefore) {
        const counts = new Map();
        for (const r of captured) counts.set(r, (counts.get(r) || 0) + 1);
        for (const [key, n] of counts) {
          if (n < 2) continue;
          const sp = key.indexOf(' ');
          log(
            'EXPLORE:DUPSUBMIT ' +
              JSON.stringify({
                from: before,
                action: 'tap:' + sel,
                method: key.slice(0, sp),
                url: key.slice(sp + 1),
                count: n,
              }),
          );
          break;
        }
      }
    }
    await finishScreencastCapture(tapPix, before, 'tap:' + sel);
    // JANK/HANG watchdog: did this action block the main thread past the
    // jank/hang floor? Keyed by (from, action) like the flicker oracle, so the
    // Rust side attributes it to this transition and `check` re-confirms it.
    const tapJank = await drainJank(page);
    if (tapJank) {
      log(
        'EXPLORE:' +
          (tapJank.kind === 'hang' ? 'HANG' : 'JANK') +
          ' ' +
          JSON.stringify({
            from: before,
            action: 'tap:' + sel,
            bucket: tapJank.bucket,
            count: tapJank.count,
          }),
      );
    }
    // FOCUS-LOSS: read the in-page verdict BEFORE observe() -- a new state's
    // ground-truth probe mutates the DOM and can move focus, which would
    // corrupt the reading. Whether the tap actually navigated is only known
    // after observe(), so the emit decision is just below.
    const focusLost = await page.evaluate(focusLossCheck).catch(() => false);
    const next = await observe();
    // FOCUS-LOSS: only a NON-navigating tap counts (same structural sig, or
    // the same route after settle: an in-place re-render). A navigation is
    // expected to move focus, so it never fires; the in-page check already
    // applied the dialog / removed-control / link guards.
    if (focusLost && (next.sig === before || (next.anchor && next.anchor === beforeAnchor))) {
      log('EXPLORE:FOCUSLOSS ' + JSON.stringify({ from: before, action: 'tap:' + sel }));
    }
    if (next.sig !== before) {
      log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'tap:' + sel, to: next.sig }));
      stuck = 0;
      // ZOOM-REFLOW: this tap navigated to a route not yet zoom-tested; run the
      // 200% zoom re-render BEFORE the metamorphic reload below (the check
      // restores the original size, so the reload still sees it). Never in
      // replay (a recorded clip must not jump viewports) or probe mode.
      if (!replay && !PROBE && next.anchor && !zoomChecked.has(next.anchor)) {
        zoomChecked.add(next.anchor);
        await zoomReflowCheck(next.sig, next.anchor);
      }
      // LISTENER-LEAK (opt-in): probe a newly-reached route (real history entry)
      // for a revisit leak. Once per route, non-replay/probe only.
      if (
        LISTENERLEAK &&
        !replay &&
        !PROBE &&
        next.anchor &&
        next.anchor !== beforeAnchor &&
        !leakChecked.has(next.anchor)
      ) {
        leakChecked.add(next.anchor);
        await listenerLeakCheck(next.anchor);
      }
    } else if (next.content !== beforeContent) {
      // Layer-1 effect detection: the tap changed displayed content (a capped
      // value display) without a structural move. EFFECTIVE, so reset stuck and
      // keep driving; no self-edge is recorded.
      stuck = 0;
    }
    current = next;
  }
  // LEAK sampler: a final heap sample after the last action, so the series spans
  // the whole soak (start ... last action). No-op outside replay.
  if (replay) await sampleHeap(page, gtCdp, Date.now() - t0);
  // BROKEN-ROUTE link check (ported from the web runner): catch a dead link the
  // bounded crawl never tapped (a footer 404). Skip in replay. Two stages, since
  // a raw fetch does not match a real navigation (an SPA serves a client route on
  // navigation but 404s a bare fetch): (1) a GET filter over every un-visited
  // same-origin link -- GET not HEAD, because a CDN/server answers HEAD with
  // 405/501 while GET is 200 (a false dead route), and GET is what navigation
  // issues, (2) VERIFY each flagged candidate with a real page.goto (also GET) --
  // only a link that truly returns 404/410 ON NAVIGATION is reported. Gated on an
  // http(s) app origin (a file:// app has no server status; honest gap there).
  if (!replay && appOrigin) {
    const FETCH_CAP = 400,
      VERIFY_CAP = 20;
    const toProbe = [...seenLinks.entries()].filter(([p]) => navStatus[p] === undefined);
    const batch = toProbe.slice(0, FETCH_CAP);
    let statuses = {};
    if (batch.length) {
      try {
        statuses = await page.evaluate(
          async (paths) => {
            const origin = location.origin,
              out = {};
            let i = 0;
            const worker = async () => {
              while (i < paths.length) {
                const p = paths[i++];
                try {
                  const r = await fetch(origin + p, { method: 'GET', redirect: 'manual' });
                  out[p] = r.status;
                } catch (e) {
                  out[p] = 0;
                }
              }
            };
            await Promise.all(Array.from({ length: 8 }, worker));
            return out;
          },
          batch.map(([p]) => p),
        );
      } catch (_) {}
    }
    // DEAD only when GENUINELY GONE: 404 or 410. Never 405/501/3xx/5xx.
    const isDead = (s) => s === 404 || s === 410;
    const candidates = batch.filter(([p]) => isDead(statuses[p] || 0));
    let verified = 0;
    for (const [path, fromSig] of candidates) {
      navStatus[path] = statuses[path] || 0;
      if (verified >= VERIFY_CAP) continue;
      verified++;
      let navStat = 0;
      try {
        const r = await page.goto(appOrigin + path, { waitUntil: 'load', timeout: 7000 });
        navStat = r ? r.status() : 0;
      } catch (_) {}
      navStatus[path] = navStat;
      if (!isDead(navStat)) continue;
      // SPA SOFT-404 guard: a 404 status that still renders the real app view (the
      // client router served index.html) is not a broken route (mirrors web).
      await settleForSignature(page);
      const view = await page.evaluate(soft404View).catch(() => null);
      if (isSoftHandled(view)) {
        navStatus[path] = 200;
        continue;
      }
      log(
        'EXPLORE:BROKENROUTE ' +
          JSON.stringify({ sig: fromSig, route: path, status: navStat, from: fromSig }),
      );
    }
    const unverified = candidates.length - Math.min(candidates.length, VERIFY_CAP);
    if (unverified)
      log(`JOURNEY[a] step: broken-route: ${unverified} candidate link(s) not verified (capped)`);
  }
  // --record clip finalize: resolve the finding's element to a viewport-relative
  // rect (CSS px), write box-spec.json in the renderer's logical space, then HOLD
  // the boxed state on film. The host runs box-overlay.mjs (clip.mov + box-spec
  // -> boxed clip), the uniform post-capture path. Trust gate: FINDING:BOXED
  // drew tells the host whether the element resolved (a clip that did not is
  // saved but flagged, never shipped with a misleading caption).
  if (clipArmed) {
    await page.waitForTimeout(300); // let the post-action state settle on screen
    const box = await resolveClipBox(page, clipPlan.sel);
    let drew = false;
    if (box) {
      // The box is valid from NOW (post scroll-settle) to the end of the film;
      // hold briefly so those final frames show the boxed element.
      const shownAt = Math.max(0, (Date.now() - recordStart) / 1000 - 0.2);
      const spec = {
        videoW: box.videoW,
        videoH: box.videoH,
        boxes: [
          {
            x: box.x,
            y: box.y,
            w: box.w,
            h: box.h,
            tStart: shownAt,
            tEnd: 1e9,
            label: clipPlan.label || clipPlan.oracle || 'finding',
            color: 'red',
          },
        ],
      };
      try {
        mkdirSync(VIDEO_DIR, { recursive: true });
        writeFileSync(joinPath(VIDEO_DIR, 'box-spec.json'), JSON.stringify(spec));
        drew = true;
      } catch (_) {
        drew = false;
      }
      await page.waitForTimeout(900); // hold the boxed state on camera
    }
    log(
      'FINDING:BOXED ' +
        JSON.stringify({ oracle: clipPlan.oracle || null, sel: clipPlan.sel, drew }),
    );
  }
  log(`JOURNEY[a] step: explored ${seen.size} states`);
  log('JOURNEY DONE');
  log('All tests passed');
  await app.close();
  // Remux the recorded .webm to clip.mov so the host's box-overlay step finds it
  // by name (record_native_clips looks for exactly `clip.mov`).
  if (clipArmed && clipVideo) {
    try {
      const webm = await clipVideo.path();
      if (remuxToMov(webm, joinPath(VIDEO_DIR, 'clip.mov'))) {
        // The host reads clip.mov; drop the redundant raw .webm so the video dir
        // matches the native contract (a single clip.mov + box-spec.json).
        try {
          rmSync(webm, { force: true });
        } catch (_) {}
      }
    } catch (_) {
      /* best-effort: the finding still reports, just without a clip */
    }
  }
}

// Only auto-run when invoked as the entry point. When imported (e.g. by the
// parity test) the canonical signature is exported without launching Electron.
const INVOKED_DIRECTLY =
  process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href;
if (INVOKED_DIRECTLY) {
  main().catch((e) => {
    log('EXCEPTION CAUGHT BY ELECTRON RUNNER');
    log(String(e && e.stack ? e.stack : e));
    log('Some tests failed');
    process.exit(0);
  });
}
