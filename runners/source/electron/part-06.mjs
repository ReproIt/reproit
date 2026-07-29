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
    if (replay && !inspectAutoContinue && process.env.REPROIT_INSPECT === '1') {
      const target = current.tappables.find((element) => `tap:${element.sel}` === act);
      const decision = await inspectPlatformStep({
        action: act,
        step: a + 1,
        total: replay.length,
        target: target?.label || target?.sel || null,
      });
      inspectAutoContinue = decision === 'continue';
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
