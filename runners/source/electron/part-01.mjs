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
// Canonical signature + scenario plumbing shared with the Tauri runner.
// The specifiers are OUTPUT-relative (this file ships as runners/electron.mjs
// next to shared/), like the './web/*.mjs' oracle imports.
import {
  signatureOf,
  descriptorOf,
  valueClass,
  fnv1a,
  loadValueNodes,
} from './shared/signature.mjs';
import { loadFuzz, rng, INJECTED_VALUES, expandEnv } from './shared/fuzz.mjs';
// The in-page DOM predicates, the selector resolver and the content-bug oracle,
// SHARED with the web and Tauri runners. This runner used to carry its own
// copies, and they disagreed with each other: snapshot() indexed one tappable
// set while gtCollect() indexed a wider one, so the same `role:textfield#N`
// named two different elements in one run. See shared/dom-walk.mjs.
import {
  detectContentBugs,
  resolveStructuralTarget,
} from './shared/dom-walk.mjs';
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

export { signatureOf, descriptorOf, valueClass };

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
