  console.log(`JOURNEY[a] step: engine=${ENGINE}`);
  const launchOptions = { headless: HEADLESS };
  if (INSPECT && ENGINE === 'chromium') {
    // A human watches this replay, so open the window maximized. The viewport
    // follows the window (see `inspectFullWindow` below), so there is no black
    // gap whatever size the desktop gives the frame.
    launchOptions.args = ['--force-renderer-accessibility', '--start-maximized'];
  }
  const browser = await launchBrowser(launchOptions);
  // Multi-actor scenario: this process plays one actor, pulling from the conductor.
  if (process.env.REPROIT_SCENARIO_BARRIER) {
    log('JOURNEY[a] step: scenario actor=' + (process.env.REPROIT_DEVICE || 'a'));
    await runScenarioActor(browser);
    await browser.close();
    return;
  }
  // Build the context options: video (optional) plus the run locale (optional).
  // Setting `locale` makes Playwright override navigator.language/languages AND
  // send a matching Accept-Language header, so both client-side i18n and
  // server-side content negotiation render the page in the requested language.
  // Scoped to this context (and so to this run).
  // Pin the viewport, device scale, and locale so the layout-sensitive oracles
  // (overflow, content) and rendered text metrics are STABLE across machines,
  // CI runners, and Playwright-default changes: a repro captured here must not
  // appear or vanish on a customer's CI. The sibling runners
  // (differential/jank/annotate) already pin these; the main runner was the
  // outlier. Defaults match Playwright's current default viewport (so no golden
  // drift today) and a canonical en-US locale; both are env-overridable for
  // responsive / i18n runs.
  const VW = Number(process.env.REPROIT_VIEWPORT_W) || 1280;
  const VH = Number(process.env.REPROIT_VIEWPORT_H) || 720;
  const effectiveLocale = LOCALE || 'en-US';
  // Identifiable scanner UA: the real browser UA (read from a throwaway context so
  // it is never hardcoded) plus the reproit token, unless the caller overrode it
  // via --header "User-Agent: ...".
  let scannerUA = UA_OVERRIDE;
  if (!scannerUA) {
    try {
      const uaCtx = await browser.newContext();
      const uaPage = await uaCtx.newPage();
      const baseUA = await uaPage.evaluate(() => navigator.userAgent).catch(() => '');
      await uaCtx.close();
      if (baseUA) scannerUA = baseUA + ' ' + REPROIT_UA_TOKEN;
    } catch (_) {}
  }
  // INSPECTION is watched by a person, not by an oracle golden. Playwright
  // applies the pinned viewport through CDP device-metrics emulation, which is
  // DECOUPLED from the OS window: the page then renders 1280x720 in the corner
  // of whatever window the desktop actually gave us and leaves the rest black.
  // `viewport: null` makes the page use the real window instead, so a maximized
  // (or user-resized) window is filled edge to edge for the whole session. Only
  // when a human is driving and nothing is being filmed: a recorded clip must
  // stay at the pinned capture size.
  // `deviceScaleFactor` is pinned with the viewport and rejected without one.
  const inspectFullWindow = INSPECT && !HEADLESS && !VIDEO_DIR;
  const contextOpts = {
    viewport: inspectFullWindow ? null : { width: VW, height: VH },
    ...(inspectFullWindow ? {} : { deviceScaleFactor: 1 }),
    locale: effectiveLocale,
    // Accept-Language first, then any --header passthrough (which may override it
    // or add clearance/auth headers). Header names are sent as given.
    extraHTTPHeaders: {
      'Accept-Language': `${effectiveLocale},${effectiveLocale.split('-')[0]};q=0.9`,
      ...EXTRA_HEADERS,
    },
    // Capture determinism: emulate prefers-reduced-motion: reduce for the whole
    // context, pinning animation-dependent layout so snapshots/pixels are stable
    // across runs for the other oracles.
    reducedMotion: 'reduce',
  };
  if (scannerUA) contextOpts.userAgent = scannerUA;
  if (VIDEO_DIR) contextOpts.recordVideo = { dir: VIDEO_DIR, size: { width: VW, height: VH } };
  if (LOCALE) console.log(`JOURNEY[a] step: locale=${LOCALE}`);
  const context = await browser.newContext(contextOpts);
  await installCapsuleReplay(context);
  // Registered after capsule replay so Playwright's LIFO route chain adds
  // correlation first, then falls back into the hermetic capsule fulfiller.
  await installBackendCorrelation(context);
  await installWebSocketCausal(context);
  const page = await context.newPage();
  // CDP session for ground-truth operability (DOMDebugger.getEventListeners):
  // detects real click/pointer listeners on elements and the document/body
  // delegation pattern. Chromium-only; firefox/webkit have no CDP, so the
  // ground-truth falls back to native + cursor + delegation-marker signals.
  let gtCdp = null;
  if (ENGINE === 'chromium') {
    try {
      gtCdp = await context.newCDPSession(page);
    } catch (e) {
      gtCdp = null;
    }
    // JANK hardening: enable the CDP Performance domain so we can read
    // LayoutCount/RecalcStyleCount. The DELTA of forced synchronous layouts
    // around an action is a machine-INVARIANT jank signal (300 forced layouts is
    // 300 on any runner), unlike the wall-clock stall. Chromium-only; best-effort.
    if (gtCdp) {
      try {
        await gtCdp.send('Performance.enable');
      } catch (_) {}
    }
  }

  // Exception oracle: uncaught page errors (a throw in an onclick, an
  // unhandled rejection) become the same EXCEPTION block the Flutter
  // pipeline emits, so the fuzz oracle and exceptions.jsonl pick them up.
  // `replayErrorCount` lets a recorded replay know a (kept) crash fired so the
  // finding box labels the triggering element "crash".
  let replayErrorCount = 0;
  // A visually empty page is ambiguous: parser fixtures, intentionally empty
  // routes, and failed mounts can all have the same DOM shape. Keep the most
  // recent independently-authoritative application failure so BLANKSCREEN is
  // emitted only when the empty state is corroborated by a first-party
  // exception or renderer crash on this exact URL. Pure emptiness remains an
  // internal candidate and can never become a user-facing finding by itself.
  let lastAppFailure = null;
  let failureCountAtLastObservation = 0;
  const recordAppFailure = (kind) => {
    replayErrorCount++;
    let url = '';
    try {
      url = page.url();
    } catch (_) {}
    lastAppFailure = { sequence: replayErrorCount, kind, url };
  };
  const emitError = (err) => {
    const msg = String(err && err.message ? err.message : err);
    // Skip third-party-script throws and known-benign browser-policy errors.
    if (
      exceptionIsBenign(msg) ||
      exceptionThrownInTracker(err && err.stack) ||
      exceptionIsNonDeterministic(msg, err && err.stack) ||
      !exceptionIsFirstParty(err && err.stack, APP_ORIGIN)
    )
      return;
    recordAppFailure('first-party-exception');
    log('EXCEPTION CAUGHT BY WEB PAGE');
    log('The following error was thrown:');
    log(msg);
    const stack = err && err.stack ? String(err.stack) : '';
    for (const line of stack.split('\n').slice(0, 8)) log(line);
    log('\u2550\u2550\u2550\u2550\u2550\u2550\u2550\u2550');
  };
  page.on('pageerror', emitError);
  // A renderer/GPU/OOM crash raises Playwright's `crash` event, NOT `pageerror`.
  // Without this the next action throws inside the runner and is misattributed to
  // the runner ("EXCEPTION CAUGHT BY WEB RUNNER") instead of the app. Emit the
  // same app-crash block and bump the counter so a recorded replay boxes it.
  page.on('crash', () => {
    recordAppFailure('renderer-crash');
    log('EXCEPTION CAUGHT BY WEB PAGE');
    log('The following error was thrown:');
    log('the page crashed (renderer process gone -- GPU / out-of-memory / ' + 'sad-tab)');
    log('════════');
  });

  // BROKEN-ROUTE oracle: record the HTTP status of main-frame DOCUMENT
  // navigations, keyed by normalized path + query. A state whose document came back >= 400
  // is a dead route the app linked to (a 404/5xx). The status is structural and
  // locale-invariant, and a 4xx/5xx is never an intended screen, so this is
  // false-positive-free. Same-origin only (off-site links are handled elsewhere).
  const navStatus = {};
  const criticalResourceFacts = new Map();
  let criticalResourceSequence = 0;
  const causalRequests = new WeakMap();
  page.on('response', async (resp) => {
    try {
      const req = resp.request();
      const resourceType = req.resourceType();
      const responseUrl = new URL(resp.url());
      let responseHeaders;
      let backendEvents = [];
      let backendReplayHeader = null;
      if (
        BACKEND_ENABLED &&
        BACKEND_ORIGINS.has(responseUrl.origin) &&
        ['xhr', 'fetch', 'eventsource'].includes(resourceType)
      ) {
        responseHeaders = await resp.allHeaders().catch(() => ({}));
        const requestHeaders = req.headers();
        const traceId = requestHeaders['x-reproit-trace'];
        const encoded = responseHeaders['x-reproit-events'];
        if (traceId && encoded) {
          backendEvents = decodeBackendEventHeader(
            encoded,
            traceId,
            requestHeaders['x-reproit-action'],
            requestHeaders['x-reproit-actor'],
          );
          for (const event of backendEvents) log('REPROIT:BACKEND ' + JSON.stringify(event));
          if (backendEvents.length > 0) {
            backendReplayHeader = encodeBackendEventHeader(backendEvents);
            log(
              backendReplayHeader
                ? 'REPROIT:CAPABILITIES {"backend_effects":{"status":"captured","detail":' +
                    '"trace-bound structural service events"},"backend_effects_replay":' +
                    '{"status":"captured","detail":"redacted events retained in hermetic ' +
                    'HTTP response"}}'
                : 'REPROIT:CAPABILITIES {"backend_effects":{"status":"captured","detail":' +
                    '"trace-bound structural service events"},"backend_effects_replay":' +
                    '{"status":"unsupported","detail":"event envelope exceeds the safe ' +
                    'replay limit"}}',
            );
          }
        }
      }
      if (responseUrl.origin === APP_ORIGIN) {
        log('FUZZ:NETWORK ' + JSON.stringify({ status: resp.status(), url: responseUrl.pathname }));
      }
      if (resourceType === 'stylesheet' || resourceType === 'script') {
        const sequence = ++criticalResourceSequence;
        const url = new URL(resp.url());
        url.hash = '';
        if (url.origin === APP_ORIGIN) {
          criticalResourceFacts.set(url.href, {
            url: url.href,
            status: resp.status(),
            contentType: '',
            resourceType,
            optional: resourceType === 'script' && exceptionThrownInTracker(url.href),
            sequence,
          });
          const headers = await resp.allHeaders().catch(() => ({}));
          if (criticalResourceFacts.get(url.href)?.sequence === sequence) {
            criticalResourceFacts.set(url.href, {
              ...criticalResourceFacts.get(url.href),
              url: url.href,
              contentType: headers['content-type'] || '',
            });
          }
        }
      }
      const causal = causalRequests.get(req);
      if (causal && NETWORK_FILE) {
        const headers = responseHeaders || (await resp.allHeaders().catch(() => ({})));
        const contentType = headers['content-type'] || '';
        let body;
        if (/text\/event-stream/i.test(contentType)) {
          const raw = await resp.text().catch(() => '');
          const sse = redactSse(raw);
          body = sse.body;
          if (!sse.supported)
            log(
              'REPROIT:CAPABILITIES {"sse":{"status":"unsupported","detail":"non-JSON ' +
                'event cannot be safely persisted"},"sse_replay":{"status":' +
                '"unsupported","detail":"non-JSON event cannot be safely persisted"}}',
            );
        } else if (/json/i.test(contentType)) {
          const raw = await resp.text().catch(() => '');
          body = parseNetworkBody(raw, contentType);
        } else {
          const len = headers['content-length'];
          body = len ? `<reproit:body:length=${len}>` : undefined;
        }
        const safeResponseHeaders = redactNetworkHeaders(headers);
        if (backendReplayHeader) {
          safeResponseHeaders['x-reproit-events'] = backendReplayHeader;
        }
        appendNetworkFact({
          version: 1,
          type: 'exchange',
          id: causal.id,
          actor: NETWORK_ACTOR,
          actionIndex: Math.max(causal.actionIndex, 0),
          ordinal: causal.ordinal,
          protocol: /text\/event-stream/i.test(contentType)
            ? 'sse'
            : new URL(resp.url()).protocol.replace(':', ''),
          method: req.method(),
          url: resp.url(),
          requestHeaders: causal.headers,
          requestBody: causal.body,
          status: resp.status(),
          responseHeaders: safeResponseHeaders,
          responseBody: body,
          required: true,
        });
        pendingCausal.delete(causal.id);
        if (/json/i.test(contentType)) {
          log(
            'FUZZ:NETWORK ' +
              JSON.stringify({
                status: resp.status(),
                url: new URL(resp.url()).pathname,
                responseShape: responseShape(body),
              }),
          );
        }
      }
      if (req.frame() !== page.mainFrame() || req.resourceType() !== 'document') return;
      const u = new URL(resp.url());
      if (u.origin !== APP_ORIGIN) return;
      navStatus[requestRouteKey(u.pathname, u.search)] = resp.status();
    } catch (e) {
      /* ignore */
    }
  });
  page.on('requestfailed', (req) => {
    try {
      const failedCausal = causalRequests.get(req);
      if (failedCausal) flushUnresolvedCausal(failedCausal.id);
    } catch (_) {}
    try {
      const resourceType = req.resourceType();
      if (resourceType !== 'stylesheet' && resourceType !== 'script') return;
      const failure = (req.failure() && req.failure().errorText) || 'request failed';
      const cancelled = /(ERR_ABORTED|NS_BINDING_ABORTED|cancelled|canceled)/i.test(failure);
      const url = new URL(req.url());
      url.hash = '';
      if (url.origin !== APP_ORIGIN) return;
      const sequence = ++criticalResourceSequence;
      criticalResourceFacts.set(url.href, {
        ...(criticalResourceFacts.get(url.href) || {}),
        url: url.href,
        failure,
        cancelled,
        resourceType,
        optional: resourceType === 'script' && exceptionThrownInTracker(url.href),
        sequence,
      });
    } catch (_) {}
  });

  // DUPLICATE-SUBMIT probe support, OPT-IN per run via REPROIT_DUPSUBMIT=1:
  // double-firing real submit actions during a walk changes exploration
  // semantics (an order really is placed twice), so the probe never runs
  // unless the operator asked for it. While a tap probe is armed (dupReqLog
  // non-null, set in the tap branch), every first-party non-GET request in the
  // window between the first click and the settle is recorded as "METHOD url";
  // the tap branch groups them and reports a pair that fired twice. A
  // page-level listener (not in-page patching) so plain form submissions count
  // exactly like fetch/XHR. null = disarmed, zero overhead on a normal walk.
  const DUPSUBMIT = process.env.REPROIT_DUPSUBMIT === '1';
  // LISTENER-LEAK probe support, OPT-IN per run via REPROIT_LISTENERLEAK=1:
  // driving repeated route revisits (history back/forward loops) changes
  // exploration semantics and adds navigation cost, so like the duplicate-submit
  // probe it never runs unless the operator asked for it. When on, an init script
  // wraps add/removeEventListener at page load so the live listener count is
  // available for the revisit samples.
  const LISTENERLEAK = process.env.REPROIT_LISTENERLEAK === '1';
  let dupReqLog = null;
  // Causal requests still awaiting a response. A request in flight at run end
  // (a crash tears the page down before its response lands) must STILL become
  // a capsule exchange -- required:false, status:0, no response -- or the
  // hermetic replay re-fires a request the capsule has never heard of and
  // fail-closes with CAPSULE:MISS. Keyed by exchange id; entries are removed
  // when the response fact is appended, flushed as unresolved at teardown and
  // on requestfailed.
  const pendingCausal = new Map();
  const flushUnresolvedCausal = (only) => {
    for (const [id, stub] of [...pendingCausal]) {
      if (only && id !== only) continue;
      appendNetworkFact({
        version: 1,
        type: 'exchange',
        ...stub,
        status: 0,
        responseHeaders: {},
        required: false,
      });
      pendingCausal.delete(id);
    }
  };
  page.on('request', (req) => {
    if (NETWORK_FILE) {
      try {
        if (['xhr', 'fetch', 'eventsource'].includes(req.resourceType())) {
          const headers = req.headers();
          const ordinal = causalOrdinal++;
          const causal = {
            id: `${NETWORK_ACTOR}-${causalActionIndex}-${ordinal}`,
            actionIndex: causalActionIndex,
            ordinal,
            headers: redactNetworkHeaders(headers),
            body: parseNetworkBody(req.postData(), headers['content-type'] || ''),
          };
          causalRequests.set(req, causal);
          pendingCausal.set(causal.id, {
            id: causal.id,
            actor: NETWORK_ACTOR,
            actionIndex: Math.max(causal.actionIndex, 0),
            ordinal: causal.ordinal,
            protocol: new URL(req.url()).protocol.replace(':', ''),
            method: req.method(),
            url: req.url(),
            requestHeaders: causal.headers,
            requestBody: causal.body,
          });
        }
      } catch (_) {}
    }
    if (!dupReqLog) return;
    try {
      const method = req.method();
      if (method === 'GET') return;
      if (new URL(req.url()).origin !== APP_ORIGIN) return;
      dupReqLog.push(method + ' ' + req.url());
    } catch (e) {
      /* ignore */
    }
  });

  // Install the Long Tasks observer (jank/hang watchdog) BEFORE the first
  // navigation so it is live for every action. addInitScript re-runs it on every
  // document, so it survives in-app navigations and reloads.
  await installLongTaskObserver(page);
  // Install the cross-engine rAF frame-interval recorder too. On firefox/webkit
  // (no Long Tasks API) it is the ONLY jank/hang signal; on chromium it is unused
  // (the precise Long Tasks path is kept), but installing it everywhere keeps the
  // page setup uniform.
  await installFrameObserver(page);
  await page.addInitScript(installCriticalResourceObserver);
  // LISTENER-LEAK counter (opt-in): wrap add/removeEventListener as an INIT
  // script so it is installed before any page script on every document and its
  // tally survives client-side navigations (the leak surface). Must precede the
  // first goto below so the initial load is instrumented too.
  if (LISTENERLEAK) await page.addInitScript(installListenerLeakCounter);

  // Ready marker so the orchestrator starts its clock; matches the Dart
  // explorer's claim line.
  log('JOURNEY claimed role=a');
  // A `scan --record` clip pins the START url so it lands directly on the
  // finding's screen (a faithful, hand-followable "open this URL"), instead of
  // replaying drifty positional taps. Same-origin as APP_URL, so the off-origin
  // guards still hold. Absent for a normal run -> the app's start URL.
  const START_URL = loadFuzz().gotoUrl || APP_URL;
  const startResponse = await page
    .goto(START_URL, { waitUntil: 'networkidle', timeout: 8000 })
    .catch(() => null);
  await page.waitForTimeout(800);

  // BOT-WALL guard: if the landing page is a WAF challenge interstitial, reproit
  // never reached the app. Report the scan UNSCANNABLE with a clear remediation
  // and emit NO oracle findings (the completion markers still fire so the run
  // reads as a clean, complete pass with zero findings, not a cut-short crawl).
  const wall = await detectBotWall(page);
  if (wall) {
    const diag =
      `target is behind a ${wall.vendor} bot-challenge (${wall.marker}); ` +
      'reproit could not reach the app. ' +
      `Allowlist the reproit User-Agent ("${REPROIT_UA_TOKEN}") in your WAF, ` +
      'run reproit against your dev/staging build, ' +
      `or pass --header "Cookie: cf_clearance=..." to inject a clearance token.`;
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
      await browser.close();
    } catch (_) {}
    return;
  }

  // Layer-3 opt-in value-node selectors from reproit.yaml (empty if none).
  const valueNodeSelectors = loadValueNodes();
  if (valueNodeSelectors.length) log(`JOURNEY[a] step: value_nodes=${valueNodeSelectors.length}`);

  // Layer-1 hard cap (docs/signature.md "Value-state"): per structural node,
  // track the DISTINCT value-class combinations seen. Once a node exceeds
  // VALUE_CLASS_CAP, fall back to its structural-only signature for the rest of
  // the run so an adversarial value generator cannot explode the graph. The cap
  // is SESSION-wide (every seed): an adversarial value generator cannot evade it
  // by resetting between seeds, matching the other runners' contract.
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

  // If an action navigated the browser off the app-under-test's origin (a
  // footer "View on GitHub", a social/outbound link), that destination is NOT
  // a state of the app: recording it would make the whole map + every fuzz
  // finding about the foreign site. Recover by going back; if that fails to
  // return us on-origin, re-goto the app URL. Mirrors the back-path recovery.
  // Returns true if a recovery was performed (caller should not record state).
  async function recoverIfOffOrigin() {
    let url = '';
    try {
      url = page.url();
    } catch (e) {}
    let off = false;
    try {
      off = new URL(url).origin !== APP_ORIGIN;
    } catch (e) {
      off = true;
    }
    if (!off) return false;
    await page.goBack({ timeout: 3000 }).catch(() => {});
    await page.waitForTimeout(400);
    let back = '';
    try {
      back = page.url();
    } catch (e) {}
    let stillOff = true;
    try {
      stillOff = new URL(back).origin !== APP_ORIGIN;
    } catch (e) {
      stillOff = true;
    }
    if (stillOff) {
      await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
      await page.waitForTimeout(400);
    }
    return true;
  }

  // Re-pump a fresh starting screen between seeds. The Flutter explorer rebuilds
  // a clean widget tree per seed; the web analogue is to navigate back to the
  // app start URL so each seed begins from the same clean state. Session-wide
  // (browser/context/page + the value cap) survives; per-seed state does not.
  async function resetToRoot() {
    // Re-navigating alone does NOT reset a state-persisting app: a TodoMVC-style
    // list kept in localStorage (or sessionStorage / IndexedDB) survives the
    // reload, so a later seed inherits an earlier seed's state and a kept repro
    // diverges on its own re-check. Land on the app origin first, CLEAR the
    // client-side stores, then re-load so the app boots from a clean slate. An
    // app that exposes window.__reproitReset() (a server-backed / custom reset)
    // gets it called too, so that convention stays compatible.
    await page.goto(APP_URL, { waitUntil: 'domcontentloaded', timeout: 8000 }).catch(() => {});
    await clearClientStorage(page);
    await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
    await page.waitForTimeout(500);
  }

  // Explore/replay ONE seed, emitting the same EXPLORE:STATE / EXPLORE:EDGE /
  // FUZZ:ACT / FUZZ:MISS markers as a single-seed run. Seen states + tried edges
  // are LOCAL to the seed so per-seed coverage is independent, matching the other
  // runners' per-seed contract (runners/rn, runners/linux-atspi.py run_seed).
  async function runSeed(fuzz) {
    const seenStates = new Set();
    // ZOOM-REFLOW (WCAG 1.4.10): anchors (routes) already re-rendered at 200%
    // zoom, so each distinct route is checked once. See zoomReflowCheck below.
    const zoomChecked = new Set();
    // ROTATION / BACKGROUND-RESTORE (lifecycle-metamorphic): each distinct state
    // sig is transform-tested once. See rotationCheck / backgroundCheck below.
    const rotChecked = new Set();
    const bgChecked = new Set();
    // LISTENER-LEAK: anchors (routes) already revisit-probed, so each distinct
    // route is checked once. See listenerLeakCheck below (opt-in).
    const leakChecked = new Set();
    const triedEdges = new Set();
    // DUPLICATE-SUBMIT probe: (from sig, action) pairs already double-
    // dispatched this seed, so each submit-like control is probed (and
    // reported) at most once.
    const dupProbed = new Set();
    const actionsByState = new Map();
    const graph = new Map();
    let launchSig = null;
    // Same-origin link targets SEEN during the crawl (pathname -> source state
    // sig), HEAD-probed for dead links at the end. Coverage is bounded, so a dead
    // link the walk never tapped (a footer /download 404) was missed when
    // broken-route relied only on actual navigations.
    const seenLinks = new Map();
    const exercisedGroups = new Set(); // choice-groups already differential-tested this seed
    const pick = rng(fuzz.seed || 0);
    const replay = fuzz.replay || null;
    let inspectAutoContinue = false;
    // Finding-highlight hints for a recorded replay: the most recent action's
    // transition-level signals, so the end-of-replay box can point at what broke.
    const recording = !!(replay && VIDEO_DIR);
    // A human is stepping this replay action by action (`reproit inspect` /
    // `check --inspect`). The probes that physically DRIVE the page -- the
    // 60-press Tab traversal, the scroll round-trip, the wheel probe -- then
    // become a burst of scrolling between one action and the next, which reads
    // as the app misbehaving rather than as an audit. They are suppressed here
    // (see the call sites); every pure-DOM scan still runs, so the verdict
    // surface the inspection reuses is otherwise unchanged.
    const humanPaced = !!(replay && INSPECT);
    const crashAtStart = replayErrorCount;
    let lastTriggerLabel = null; // 'jank' / 'froze' from the latest action (crash overrides)
    let lastFlickerKeys = null; // churned persistent-chrome anchor keys, latest action
    // Property-matched fixture inputs for this seed (field -> concrete value).
    // Empty unless the config carries `inputs`; when present, a matching `type:`
    // action types the provided value instead of the adversarial-class token.
    const inputs = loadInputs(fuzz);
    if (fuzz.seed) log(`JOURNEY[a] step: fuzz seed=${fuzz.seed}`);

    // The state + action that triggered the CURRENT navigation, so a broken-route
    // landed on by tapping a link is attributed to the exact SOURCE page and link
    // (not reverse-matched by destination, which is arbitrary when several pages
    // link to the same dead route). Set right before each navigating tap; null for
    // the initial load (the start URL has no in-app source).
    let lastNav = null;

    async function observe() {
      const snap = await snapshot(page, valueNodeSelectors);
      snap.sig = effectiveSig(snap);
      // Temporal contracts need every observation, including revisits and text
      // changes that deliberately do not alter the structural map signature.
      log(
        'FUZZ:OBS ' +
          JSON.stringify({
            sig: snap.sig,
            ...(snap.anchor ? { route: snap.anchor } : {}),
            labels: snap.labels.slice(0, 24),
            elements: snap.tappables.slice(0, 24).map((e) => ({ role: e.role })),
          }),
      );
      if (replay) log('FUZZ:STATE ' + snap.sig);
      if (!seenStates.has(snap.sig)) {
        seenStates.add(snap.sig);
        // sig: STRUCTURAL (roles + tree shape + stable developer keys),
        //      locale-invariant.
        // labels: DISPLAY-ONLY visible text (map show), never in the sig.
        // elements: structural selectors for replay; `nokey` flags a tappable
        //           with no explicit author key (data-testid/name) so the map layer can
        //           warn the developer to add one.
        log(
          'EXPLORE:STATE ' +
            JSON.stringify({
              sig: snap.sig,
              // route: the URL path, so the candidate map can reconcile by route
              // (the reliable, framework-neutral join key) and not just by name.
              ...(snap.anchor ? { route: snap.anchor } : {}),
              labels: snap.labels.slice(0, 24),
              elements: snap.tappables.slice(0, 24).map((e) => {
                const o = { sel: e.sel, role: e.role, label: e.label };
                if (e.purpose) o.inputPurpose = e.purpose;
                if (e.bounds) o.bounds = e.bounds;
                if (!e.key) o.nokey = true;
                return o;
              }),
              texts: (snap.texts || []).slice(0, 48),
            }),
        );
        // Evidence recording is not another audit. The scan already found and
        // classified the bug; this run exists only to film that reproduction.
        // Skip state-audit probes here because some are intentionally invasive:
        // the 60-step Tab traversal walks focus through the whole document and
        // scrollRoundTripScan drives a scroller away and back. Filming those made
        // a choice-anomaly clip visit the footer before touching its picker.
        if (recording) {
          failureCountAtLastObservation = replayErrorCount;
          return snap;
        }
        // The structural oracle scans run on the SAME (un-mutated) DOM the
        // snapshot captured, and crucially BEFORE emitGroundtruth -- whose
        // keyboard-activation probe mutates the DOM and whose framebuffer probe
        // (REPROIT_PROBE=1) RELOADS the page to the start URL. Running them after
        // would scan the reloaded/mutated page yet attribute findings to THIS
        // sig, so a probe run mis-keyed every overflow/content-bug to the wrong
        // state. Order is therefore: scans first, ground-truth (mutating) last.
        //
        // CONTENT-BUG, keyed by the SAME sig. Pure DOM/label scan (no pixels, no
        // timing), so it reproduces on replay. Silent when nothing is broken.
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
        // ZERO-CONTRAST: visible text whose resolved foreground exactly equals
        // its composited backdrop is invisible where it must be read (the
        // supabase light-mode class). Exact equality only; a text node behind
        // an overlay or with a transparent color abstains. Same emission shape
        // as the TUI oracle; the Rust core evaluates it via EXPLORE:ZEROCONTRAST.
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
        // LAYOUT CONTAINMENT. Containers opt in with `data-reproit-contain`,
        // giving the detector authoritative intent instead of asking it to
        // guess whether clipping, scrolling, or decorative spill was wanted.
        // The Rust core evaluates these two-sample geometry facts.
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
        // OCCLUSION: an interactive element that is presented as usable (visible,
        // in the viewport, not aria-hidden/inert) but whose CENTER is covered by a
        // foreign element -- a click there hits the overlay, not the control. The
        // classic case is an invisible leftover backdrop or a z-index accident
        // blocking the UI. Pure hit-test (document.elementFromPoint), deterministic
        // given a fixed viewport, so it re-confirms on replay. FP guards: when a
        // modal is open the background is LEGITIMATELY covered, so we only check
        // elements inside the modal; and we skip hidden/zero-opacity/off-screen
        // controls (not presented as clickable). RE-CONFIRMED: a second scan a
        // beat later must agree (same target+cover), so a transient overlap from
        // an animating menu / mid-scroll dropdown drops out; only a stably buried
        // control survives.
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
        // EXPLICIT STRUCTURAL RELATIONSHIPS. This is deliberately not a visual
        // badge heuristic: the page must declare indicator, owner, and container
        // semantics. ABSTAIN relationships stay silent. A VIOLATION violation must
        // survive a second settled sample with the same structural identity and
        // violation before it enters the marker stream.
        const relation1 = await page.evaluate(indicatorRelationshipScan).catch(() => null);
        let relation2 = null;
        if (relation1?.outcome === 'VIOLATION') {
          await page.waitForTimeout(120);
          relation2 = await page.evaluate(indicatorRelationshipScan).catch(() => null);
          const relations = confirmRelationshipViolations(relation1, relation2);
          if (relations.length) {
            log(
              'EXPLORE:RELATION ' +
                JSON.stringify({
                  sig: snap.sig,
                  ...(snap.anchor ? { route: snap.anchor } : {}),
                  items: relations,
                }),
            );
          }
        }
        const relationStatus = relation2 || relation1;
        if (relationStatus) {
          log(
            'EXPLORE:RELATIONSTATUS ' +
              JSON.stringify({
                sig: snap.sig,
