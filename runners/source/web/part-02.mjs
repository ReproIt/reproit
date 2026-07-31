// Drain the rAF interval buffer and classify it. Returns the SAME shape as
// drainJank ({ kind, bucket, count }) or null. The cross-engine path.
async function drainFrameJank(page) {
  const intervals = await page
    .evaluate(() => {
      const t = window.__reproitFrameIntervals || [];
      window.__reproitFrameIntervals = [];
      return t;
    })
    .catch(() => []);
  return classifyFrameIntervals(intervals);
}
// Per-action jank/hang verdict, engine-aware. On chromium we keep the PRECISE
// Long Tasks path UNCHANGED (it is more accurate than rAF); the rAF path is the
// cross-engine fallback used on firefox/webkit, where Long Tasks is unavailable.
// This keeps chromium byte-for-byte identical (no rAF can flip its verdict) while
// closing the silence on the other two engines.
async function drainJankForEngine(page) {
  if (ENGINE === 'chromium') return drainJank(page);
  return drainFrameJank(page);
}

// LEAK sampler (deterministic, web heap). `--soak` replays a reversible cycle N
// times and reads the heap slope; the Rust soak oracle flags growth that scales
// with the cycle count. The web runner has no Dart VM service, so we read the v8
// heap directly. PRECISION MATTERS HERE: `performance.memory.usedJSHeapSize` is
// QUANTIZED by Chromium to a coarse bucket (it pins to a rounded value like 10MB
// and barely moves) to defeat fingerprinting, so it CANNOT see a multi-MB leak
// and is useless for this. The CDP `Runtime.getHeapUsage` reports the REAL,
// unrounded v8 used-heap size, so we use that when a CDP session is available
// (chromium) and force a GC first (`HeapProfiler.collectGarbage`) so the reading
// is the RETAINED (live) heap, not transient garbage: a true leak survives GC and
// grows monotonically, while a resource-neutral cycle collapses back flat. We emit
// a MEMORY:SAMPLE marker per cycle; the soak side reconstructs the series from
// these when no VM-service memory file exists. CHROMIUM-ONLY by design: the
// precise heap needs the CDP `Runtime.getHeapUsage` domain. There is deliberately
// NO `performance.memory` fallback -- it is quantized to a coarse ~10MB bucket
// (anti-fingerprinting) so it cannot see a multi-MB leak; emitting it would feed
// the slope a leak-blind number, which docs/oracles.md rightly calls worse than
// silence. Off Chromium the leak oracle is an honest `gap` (no sample emitted).
async function sampleHeap(page, cdp, tMs) {
  let used = null;
  if (cdp) {
    try {
      // Force a GC so the reading reflects RETAINED memory, then read the precise
      // v8 used-heap size. Both are CDP domains available without page changes.
      await cdp.send('HeapProfiler.collectGarbage').catch(() => {});
      const r = await cdp.send('Runtime.getHeapUsage');
      if (r && typeof r.usedSize === 'number') used = Math.round(r.usedSize);
    } catch (_) {
      used = null;
    }
  }
  if (used == null) return;
  // DETERMINISTIC leak signal alongside the bytes: the live DOM element count.
  // Heap bytes are allocator/machine-dependent, but the node count over identical
  // cycles is an integer that reproduces on any runner, so monotonic node growth
  // is a machine-invariant leak verdict. Counted AFTER the forced GC above.
  const domNodes = await page
    .evaluate(() => document.getElementsByTagName('*').length)
    .catch(() => null);
  log(
    'MEMORY:SAMPLE ' +
      JSON.stringify({
        t_ms: tMs,
        heap_used: used,
        ...(domNodes != null ? { dom_nodes: domNodes } : {}),
      }),
  );
}

const ACTION_BUDGET = 36;
// Zero-config map mode used to be unbounded and relied on the host's 300s kill.
// A deterministic work bound makes the same app produce the same explored prefix
// regardless of machine speed. Exhaustion is reported as bounded/truncated but
// the runner completes normally, leaving the observed map usable.
const MAP_ACTION_BUDGET = Math.max(1, Number(process.env.REPROIT_MAP_ACTION_BUDGET) || 72);
const MAX_LABEL_LEN = 40;
// Layer-1 value-class cap (docs/signature.md "Value-state"): once a structural
// node has shown more than this many DISTINCT value-class combinations, the
// runner drops it to structural-only so an adversarial value generator cannot
// explode the graph. The oracle is stateless; the cap is purely runner-local.
const VALUE_CLASS_CAP = 8;

// Layer-3 opt-in (docs/signature.md "Value-state"): read `value_nodes:`
// selectors from reproit.yaml. We avoid adding a YAML dependency: the block is
// a simple flat list of strings, so a tiny line parser is enough and keeps the
// runner dependency-free. Path precedence: REPROIT_CONFIG env, else
// ./reproit.yaml in the cwd. A missing/unparseable file yields an empty list,
// so value-state is strictly opt-in.
const ADVERSARIAL = [
  { id: 'empty', value: '' },
  { id: 'long', value: 'A'.repeat(512) },
  { id: 'emoji', value: '🙂🚀✨🧪🔥' },
  { id: 'rtl', value: 'مرحبا שלום ‮abc‬' },
  { id: 'inject', value: '"><img src=x onerror=alert(1)>{{7*7}}' },
  { id: 'normal', value: 'Buy milk' },
];
const ADVERSARIAL_BY_ID = Object.fromEntries(ADVERSARIAL.map((a) => [a.id, a.value]));

// Map a non-negative integer (derived from the seeded rng) to an adversarial
// entry, deterministically. Same input -> same entry on every run.
function adversarialFor(n) {
  const i = ((n % ADVERSARIAL.length) + ADVERSARIAL.length) % ADVERSARIAL.length;
  return ADVERSARIAL[i];
}

// Property-matched replay (fixture inputs). The fuzz config may carry an
// `inputs` array, each `{ field, value }`, written by the CLI's
// crate::fixture::synthesize from the cloud's fixtureSpec: a CONCRETE,
// property-matched value (a 312-char unicode name, an emoji, an empty / RTL
// field) reconstructed from production telemetry. When a `type:` action targets
// a field with a provided input value, we type THAT value instead of only the
// fixed adversarial-class token, so the data-dependent bug actually reproduces.
// The provided value is itself deterministic (synthesis uses no RNG), so this
// path is as reproducible as the adversarial-class path.
//
// Normalize the config's `inputs` into a flat [{field, value}] list. `field`
// is the field identifier, either a semantic key ("email") or a full structural
// selector ("key:id:email"). Entries with no usable field key are dropped.
// Tolerant of a missing/garbage array (returns []), so a config without
// `inputs` is unaffected.
function loadInputs(fuzz) {
  const arr = fuzz && Array.isArray(fuzz.inputs) ? fuzz.inputs : [];
  const out = [];
  for (const it of arr) {
    if (!it || typeof it !== 'object') continue;
    const field = typeof it.field === 'string' && it.field ? it.field : '';
    if (!field) continue;
    const value = it.value != null ? String(it.value) : '';
    out.push({ field, value });
  }
  return out;
}

// Resolve a `type:` selector to a provided input value, or null when no input
// matches. The fixture `field` is a semantic identifier (e.g. "name"); the
// runner's selectors are structural (`key:<kind>:<v>` or `role:<role>#<idx>`).
// A field matches when it equals the full selector OR the key VALUE of a
// `key:<kind>:<v>` selector (so `field:"name"` matches `key:id:name`,
// `key:name:name`, or `key:testid:name`). First matching entry wins (config
// order). Empty `inputs` -> null (the adversarial-class path is untouched).
function inputValueFor(sel, inputs) {
  if (!inputs || !inputs.length || !sel) return null;
  let keyVal = null;
  if (sel.startsWith('key:')) {
    const body = sel.slice(4);
    const ci = body.indexOf(':');
    keyVal = ci >= 0 ? body.slice(ci + 1) : body;
  }
  for (const inp of inputs) {
    if (inp.field === sel || (keyVal != null && inp.field === keyVal)) return inp.value;
  }
  return null;
}

function log(line) {
  if (String(line).startsWith('FUZZ:ACT ')) {
    causalActionIndex++;
    causalOrdinal = 0;
  }
  process.stdout.write(line + '\n');
}

const SECRET_FIELD_NAMES = [
  'password',
  'passwd',
  'secret',
  'token',
  'authorization',
  'cookie',
  'email',
  'phone',
  'apikey',
  'publishablekey',
  'privatekey',
  'accesskey',
  'signingkey',
  'idempotencykey',
];
function secretFieldName(name) {
  const canonical = String(name)
    .toLowerCase()
    .replace(/[^a-z0-9]/g, '');
  return SECRET_FIELD_NAMES.some((part) => canonical.includes(part));
}
function redactedMetadata(value) {
  let type = value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value;
  if (type === 'object') type = 'object';
  const length =
    type === 'string' ? [...value].length : type === 'array' ? value.length : undefined;
  return { $reproit: { redacted: true, type, ...(length == null ? {} : { length }) } };
}
function isRedactedMetadata(value) {
  const meta = value && typeof value === 'object' && value.$reproit;
  return meta && meta.redacted === true && typeof meta.type === 'string';
}
export function redactNetworkValue(value) {
  if (isRedactedMetadata(value)) return value;
  if (Array.isArray(value)) return value.map(redactNetworkValue);
  if (value && typeof value === 'object') {
    const out = {};
    for (const key of Object.keys(value).sort()) {
      const child = value[key];
      out[key] = secretFieldName(key) ? redactedMetadata(child) : redactNetworkValue(child);
    }
    return out;
  }
  return value;
}
export function redactNetworkHeaders(headers) {
  const out = {};
  for (const key of Object.keys(headers || {}).sort()) {
    out[key] =
      key.toLowerCase() === 'x-reproit-events'
        ? '<reproit:backend-events>'
        : secretFieldName(key)
          ? '<reproit:secret>'
          : String(headers[key]);
  }
  return out;
}
export function parseNetworkBody(raw, contentType = '') {
  if (raw == null || raw === '') return undefined;
  if (/json/i.test(contentType)) {
    try {
      return redactNetworkValue(JSON.parse(raw));
    } catch (_) {
      return '<reproit:invalid-json>';
    }
  }
  // Persist structure, not arbitrary production content. Exact binary/text
  // bodies require an explicit future project policy and capability.
  return `<reproit:body:length=${Buffer.byteLength(String(raw), 'utf8')}>`;
}

export function responseShape(value) {
  if (Array.isArray(value)) {
    const shapes = [...new Set(value.slice(0, 16).map(responseShape))].sort();
    return `[${shapes.join('|')}]`;
  }
  if (value && typeof value === 'object') {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${key}:${responseShape(value[key])}`)
      .join(',')}}`;
  }
  if (value === null) return 'null';
  return typeof value;
}
function appendNetworkFact(fact) {
  if (!NETWORK_FILE) return;
  try {
    appendFileSync(NETWORK_FILE, JSON.stringify(fact) + '\n', { encoding: 'utf8', mode: 0o600 });
  } catch (_) {}
}

export function backendCorrelationHeaders(
  url,
  actionIndex,
  ordinal,
  trustedOrigins = APP_ORIGIN,
  actor = NETWORK_ACTOR,
) {
  let origin;
  try {
    origin = new URL(url).origin;
  } catch (_) {
    return null;
  }
  const allowed =
    trustedOrigins instanceof Set
      ? trustedOrigins
      : new Set(Array.isArray(trustedOrigins) ? trustedOrigins : [trustedOrigins]);
  if (!allowed.has(origin)) return null;
  const safeActor =
    String(actor)
      .replace(/[^A-Za-z0-9_.-]/g, '')
      .slice(0, 32) || 'a';
  const traceId = `rpt-${safeActor}-${Math.max(0, actionIndex)}-${Math.max(0, ordinal)}`;
  return {
    'x-reproit-trace': traceId,
    'x-reproit-actor': safeActor,
    'x-reproit-action': String(Math.max(0, actionIndex)),
    ...(BACKEND_BUILD ? { 'x-reproit-build': BACKEND_BUILD } : {}),
    ...(BACKEND_CONFIG_CONTRACT ? { 'x-reproit-config-contract': BACKEND_CONFIG_CONTRACT } : {}),
  };
}

export function decodeBackendEventHeader(encoded, expectedTrace, actionIndex, actor) {
  if (!encoded || typeof encoded !== 'string' || encoded.length > 65536) return [];
  try {
    const value = JSON.parse(Buffer.from(encoded, 'base64url').toString('utf8'));
    if (!Array.isArray(value) || value.length > 256) return [];
    return value
      .filter(
        (event) =>
          event &&
          typeof event === 'object' &&
          Number.isSafeInteger(event.sequence) &&
          event.sequence >= 0 &&
          typeof event.traceId === 'string' &&
          event.traceId === expectedTrace &&
          typeof event.spanId === 'string' &&
          event.spanId.length > 0 &&
          event.spanId.length <= 128 &&
          typeof event.operation === 'string' &&
          event.operation.length > 0 &&
          event.operation.length <= 256 &&
          ['start', 'return', 'effect'].includes(event.kind),
      )
      .map((event) => {
        const rawIdentity = event.idempotencyKey == null ? undefined : String(event.idempotencyKey);
        const identity =
          rawIdentity == null
            ? undefined
            : /^sha256:[0-9a-f]{24}$/i.test(rawIdentity)
              ? rawIdentity.toLowerCase()
              : `sha256:${createHash('sha256').update(rawIdentity).digest('hex').slice(0, 24)}`;
        const safe = redactNetworkValue({
          ...event,
          actionIndex: Math.max(0, Number(actionIndex) || 0),
          actor: String(actor || 'a'),
        });
        if (identity) safe.idempotencyKey = identity;
        return safe;
      });
  } catch (_) {
    return [];
  }
}

export function encodeBackendEventHeader(events) {
  if (!Array.isArray(events) || events.length === 0 || events.length > 256) return null;
  const encoded = Buffer.from(JSON.stringify(events), 'utf8').toString('base64url');
  return encoded.length <= 60000 ? encoded : null;
}

export async function installBackendCorrelation(context, enabled = BACKEND_ENABLED, options = {}) {
  if (!enabled) return;
  const trustedOrigins =
    options.trustedOrigins || (options.appOrigin ? new Set([options.appOrigin]) : BACKEND_ORIGINS);
  const actor = options.actor || NETWORK_ACTOR;
  const currentAction = options.actionIndex || (() => causalActionIndex);
  await context.route('**/*', async (route) => {
    const req = route.request();
    if (!['xhr', 'fetch', 'eventsource'].includes(req.resourceType())) return route.fallback();
    const correlation = backendCorrelationHeaders(
      req.url(),
      currentAction(),
      backendRequestOrdinal++,
      trustedOrigins,
      actor,
    );
    if (!correlation) return route.fallback();
    return route.fallback({ headers: { ...req.headers(), ...correlation } });
  });
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

export async function installCapsuleReplay(context, path = process.env.REPROIT_CAPSULE) {
  if (!path) return;
  const capsule = JSON.parse(readFileSync(path, 'utf8'));
  const all = (capsule.exchanges || []).filter((e) => /^(https?|sse)$/.test(e.protocol));
  // Served: causally required with a captured response -- fulfilled verbatim.
  // Known-but-unserved: unresolved at capture (status 0: the original run
  // ended before the response landed) or demoted by causal reduction
  // (required:false). The capsule KNOWS these requests; the faithful hermetic
  // replay is to abort them (the failure never saw their responses), never to
  // fail-close as an unknown request.
  const exchanges = all.filter((e) => e.required && e.status > 0);
  const dropped = all.filter((e) => !(e.required && e.status > 0));
  const used = new Set();
  const usedDropped = new Set();
  // Batch replays share this process but each run replays the same action
  // clock from zero, so each run gets the full exchange budget again.
  capsuleReplayReset = () => {
    used.clear();
    usedDropped.clear();
  };
  await context.route('**/*', async (route) => {
    const req = route.request();
    if (!['xhr', 'fetch', 'eventsource'].includes(req.resourceType())) return route.continue();
    const actionIndex = Math.max(causalActionIndex, 0);
    const wantedUrl = canonicalNetworkUrl(req.url());
    const matches = (e) =>
      e.actor === NETWORK_ACTOR &&
      e.actionIndex === actionIndex &&
      String(e.method).toUpperCase() === req.method().toUpperCase() &&
      canonicalNetworkUrl(e.url) === wantedUrl;
    const idx = exchanges.findIndex((e, i) => !used.has(i) && matches(e));
    if (idx < 0) {
      const dropIdx = dropped.findIndex((e, i) => !usedDropped.has(i) && matches(e));
      if (dropIdx >= 0) {
        usedDropped.add(dropIdx);
        log(`CAPSULE:DROP ${dropped[dropIdx].id}`);
        return route.abort('blockedbyclient');
      }
      log(`CAPSULE:MISS ${req.method()} ${req.url()} action=${actionIndex}`);
      return route.abort('blockedbyclient');
    }
    used.add(idx);
    const e = exchanges[idx];
    const headers = { ...(e.responseHeaders || {}) };
    let body = '';
    if (e.responseBody !== undefined) {
      body = typeof e.responseBody === 'string' ? e.responseBody : JSON.stringify(e.responseBody);
      if (typeof e.responseBody !== 'string' && !headers['content-type'])
        headers['content-type'] = 'application/json';
    }
    log(`CAPSULE:HIT ${e.id}`);
    return route.fulfill({ status: e.status, headers, body });
  });
  log(`CAPSULE:READY ${capsule.id || ''} exchanges=${exchanges.length}`);
}

function websocketFrameValue(message) {
  if (typeof message !== 'string')
    return { supported: false, value: `<reproit:body:length=${message.length}>` };
  try {
    return { supported: true, value: redactNetworkValue(JSON.parse(message)) };
  } catch (_) {
    return {
      supported: false,
      value: `<reproit:body:length=${Buffer.byteLength(message, 'utf8')}>`,
    };
  }
}

function websocketReplayFrame(value) {
  return typeof value === 'string' ? value : JSON.stringify(value);
}

/** Ordered JSON WebSocket capture/replay. Non-JSON frames downgrade the
 * capability instead of persisting opaque user content or claiming replay. */
export async function installWebSocketCausal(context, path = process.env.REPROIT_CAPSULE) {
  let replay = [];
  if (path) {
    const capsule = JSON.parse(readFileSync(path, 'utf8'));
    replay = (capsule.exchanges || []).filter((e) => e.required && /^(ws|wss)$/.test(e.protocol));
  }
  const used = new Set();
  await context.routeWebSocket(/.*/, (socket) => {
    const url = socket.url();
    if (path) {
      const next = () =>
        replay
          .map((exchange, index) => ({ exchange, index }))
          .filter(
            ({ exchange, index }) =>
              !used.has(index) &&
              exchange.actor === NETWORK_ACTOR &&
              exchange.actionIndex === causalActionIndex &&
              canonicalNetworkUrl(exchange.url) === canonicalNetworkUrl(url),
          )
          .sort((a, b) => a.exchange.ordinal - b.exchange.ordinal)[0];
      const deliver = () => {
        for (;;) {
          const item = next();
          if (!item || item.exchange.method !== 'RECV') break;
          used.add(item.index);
          socket.send(websocketReplayFrame(item.exchange.responseBody));
          log(`CAPSULE:HIT ${item.exchange.id}`);
        }
      };
      queueMicrotask(deliver);
      socket.onMessage((message) => {
        const frame = websocketFrameValue(message);
        const item = next();
        if (
          !item ||
          item.exchange.method !== 'SEND' ||
          JSON.stringify(item.exchange.requestBody) !== JSON.stringify(frame.value)
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
      const frame = websocketFrameValue(message);
      if (!frame.supported) {
        log(
          'REPROIT:CAPABILITIES {"websocket":{"status":"unsupported","detail":' +
            '"non-JSON frame cannot be safely persisted"},"websocket_replay":' +
            '{"status":"unsupported","detail":"non-JSON frame cannot be safely ' +
            'persisted"}}',
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
        requestBody: method === 'SEND' ? frame.value : undefined,
        status: 101,
        responseHeaders: {},
        responseBody: method === 'RECV' ? frame.value : undefined,
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

export function redactSse(raw) {
  let supported = true;
  const body = String(raw)
    .split(/(\r?\n)/)
    .map((line) => {
      if (!line.startsWith('data:')) return line;
      const prefix = line.match(/^data:\s*/)[0];
      try {
        return prefix + JSON.stringify(redactNetworkValue(JSON.parse(line.slice(prefix.length))));
      } catch (_) {
        supported = false;
        return 'data:<reproit:unsupported-non-json>';
      }
    })
    .join('');
  return { body, supported };
}

// Screenshot-capture contract (drive.rs): on a named "shoot" point, capture the
// current screen to $REPROIT_SHOTS_DIR/<name>.png, then print `SHOOT:<name>` so
// the orchestrator confirms the file and logs it. `name` is restricted to
// [A-Za-z0-9_/-] (the orchestrator filters to those anyway). If REPROIT_SHOTS_DIR
// is unset we skip the capture but STILL print the marker, so non-screenshot runs
// are unaffected. Playwright's page.screenshot writes the PNG directly.
async function shoot(page, name) {
  const dir = process.env.REPROIT_SHOTS_DIR;
  if (dir) {
    try {
      mkdirSync(dir, { recursive: true });
      await page.screenshot({ path: join(dir, name + '.png'), fullPage: false });
    } catch (e) {
      /* capture is best-effort; still emit the marker below */
    }
  }
  log('SHOOT:' + name);
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

// The list of per-seed fuzz configs to run in this session. Mirrors the other
// runners' batch contract (the Flutter scaffold's FuzzCfg.loadBatch,
// runners/rn, runners/linux-atspi.py load_batch): reproit's multi-seed fuzz
// writes {"batch":[ <cfg>, ... ]} where each <cfg> is the single-seed shape
// ({seed, budget, edgeWeights, prefix, replay, ...}). A single-seed run writes
// the bare {"seed":..} object with no "batch" key. Returns
// { seeds, isBatch } where isBatch is true ONLY for the multi-seed shape; the
// caller wraps each seed in SEED:BEGIN/SEED:END only when isBatch, so the
// single-seed path stays byte-for-byte identical (no SEED markers).
function loadBatch() {
  const j = loadFuzz();
  if (j && Array.isArray(j.batch) && j.batch.length) {
    return { seeds: j.batch.map((b) => (b && typeof b === 'object' ? b : {})), isBatch: true };
  }
  return { seeds: [j || {}], isBatch: false };
}

const FUZZ_CONFIGURED = !!process.env.REPROIT_FUZZ_CONFIG;

// A scan/map coverage config is deliberately compact. Fuzz plans carry guidance
// fields (edgeWeights/contractActions/seeds/prefix), while scan writes only seed
// and budget. Keeping this decision runner-local adds honest truncation reporting
// without changing the serialized config contract or ordinary fuzz semantics.
function isCoverageWalkConfig(fuzz) {
  if (!fuzz || typeof fuzz !== 'object' || fuzz.replay || fuzz.prefix) return false;
  return Object.keys(fuzz).every((key) => key === 'seed' || key === 'budget');
}

function edgeKey(sig, action) {
  return sig + '|' + action;
}
function rememberActions(actionsByState, sig, actions) {
  const known = actionsByState.get(sig) || [];
  for (const action of actions) if (!known.includes(action)) known.push(action);
  actionsByState.set(sig, known);
}
function coverageActions(current) {
  const actions = [];
  for (const element of current.tappables) {
    if (element.external) continue;
    actions.push(
      element.role === 'textfield'
        ? 'type:' + element.sel + '=normal'
        : 'tap:' + element.sel,
    );
  }
  actions.sort();
  actions.push('back');
  return actions;
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

// xorshift32, identical to explorer.dart so seeds mean the same thing.
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
