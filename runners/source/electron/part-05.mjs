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
  if (process.env.REPROIT_ELECTRON_DISABLE_SANDBOX === '1') {
    launchOpts.args = [
      ...(launchOpts.args || []),
      '--no-sandbox',
      '--disable-setuid-sandbox',
    ];
  }
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
  let inspectAutoContinue = false;
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
