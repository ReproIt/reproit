  if (!APP) {
    log('EXCEPTION CAUGHT BY REPROIT');
    log('REPROIT_APP (executable path) required');
    log('═'.repeat(8));
    process.exit(0);
  }
  const fuzz = loadFuzz();
  const { remote } = await import('webdriverio');
  const url = new URL(WD_URL);
  const browser = await remote({
    hostname: url.hostname,
    port: Number(url.port || 4444),
    path: url.pathname || '/',
    // No browserName: tauri-driver forwards it verbatim to the native driver
    // (WebKitWebDriver on Linux), which rejects unknown values like 'wry' with
    // "Failed to match capabilities". The official Tauri v2 WebDriver example
    // sends only tauri:options. tauri-driver reads tauri:options from
    // alwaysMatch (where wdio places a single plain capabilities object).
    capabilities: { 'tauri:options': { application: APP } },
  });

  // Multi-actor scenario: this process plays one actor, pulling from the
  // conductor; the fuzz walk and its oracles do not run.
  if (process.env.REPROIT_SCENARIO_BARRIER) {
    log('JOURNEY[a] step: scenario actor=' + (process.env.REPROIT_DEVICE || 'a'));
    await runScenarioActor(browser);
    await browser.deleteSession();
    return;
  }

  log('JOURNEY claimed role=a');
  await browser.pause(1500);
  // Raise the async-script timeout so the choice-anomaly pass (which waits for
  // layout to settle between each option of a multi-choice component) is not cut
  // off mid-exercise. A picker with many options at ~600ms each can run several
  // seconds; 30s leaves comfortable headroom without hanging the run if a webview
  // wedges (executeAsync still rejects on its own timeout). Best-effort.
  try {
    await browser.setTimeout({ script: 30000 });
  } catch (_) {}
  // Install the exception hooks before the first snapshot so even errors thrown
  // during initial render are captured.
  await installHooks(browser);
  // Install the Long Tasks observer (jank/hang watchdog) so it is live for every
  // action. Re-installed in observe() since a navigation replaces the window.
  await installLongTaskObserver(browser);
  // Install the cross-engine rAF frame observer too (the path that catches
  // jank/hang on Tauri's WebKit webview, where Long Tasks is unavailable).
  // Re-installed in observe() since a navigation replaces the window.
  await installFrameObserver(browser);
  // BOT-WALL guard (defensive): if the webview landed on a WAF challenge
  // interstitial instead of the app, report UNSCANNABLE with zero findings. The
  // completion markers still fire so the run reads as a clean, complete pass.
  const wall = await detectBotWall(browser);
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
    await browser.deleteSession();
    return;
  }
  const seen = new Set(),
    tried = new Set();
  const pick = rng(fuzz.seed || 0);

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
    // Re-install hooks first (a navigation since the last observe would have
    // replaced the window and dropped them); installHooks is idempotent.
    await installHooks(browser);
    // Re-install the Long Tasks observer too (a navigation drops it); idempotent.
    await installLongTaskObserver(browser);
    // Re-install the cross-engine rAF frame observer too (a navigation drops it).
    await installFrameObserver(browser);
    // Drain any errors that the just-completed action produced. observe() runs
    // after every action (tap and back), so this covers all action sites.
    await drainErrors(browser);
    const snap = await snapshot(browser, valueNodeSelectors);
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
      let overflow1 = null;
      let overflow2 = null;
      try {
        overflow1 = await browser.execute(layoutOverflowScan);
        await browser.pause(120);
        overflow2 = await browser.execute(layoutOverflowScan);
      } catch (_) {}
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
      // The synthetic keydown ground-truth probe can mutate the DOM, so it runs
      // after every state-present layout scan.
      await emitGroundtruth(browser, snap.sig);
      // CONTENT-BUG for this newly-seen state, keyed by the SAME sig. Pure
      // DOM/label scan (no pixels, no timing), so it reproduces on replay. Only
      // emitted when a broken-content artifact is actually rendered.
      let cbug = null;
      try {
        cbug = await browser.execute(DETECT_CONTENTBUG_JS, [...INJECTED_VALUES]);
      } catch (_) {}
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
      // OCCLUSION + SECURITY: same pure-DOM hygiene scans as the web runner,
      // shared from web/hygiene-oracles.mjs (webview DOM, identical API).
      let occ = null;
      try {
        const occ1 = await browser.execute(occlusionScan);
        occ = occ1;
        if (occ1 && occ1.length) {
          // RE-CONFIRM: a transient overlay (animating menu / mid-scroll list)
          // clears by the second frame; only a stably buried control survives.
          await browser.pause(300);
          const occ2 = await browser.execute(occlusionScan);
          occ = confirmOcclusions(occ1, occ2 || []);
        }
      } catch (_) {}
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
      const relation1 = await browser.execute(indicatorRelationshipScan).catch(() => null);
      let relation2 = null;
      if (relation1?.outcome === 'VIOLATION') {
        await browser.pause(120);
        relation2 = await browser.execute(indicatorRelationshipScan).catch(() => null);
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
              ...(snap.anchor ? { route: snap.anchor } : {}),
              outcome: relationStatus.outcome,
              checks: relationStatus.checks,
            }),
        );
      }
      // ZERO-CONTRAST: text whose resolved foreground exactly equals its
      // composited backdrop is invisible where it must be read. Pure in-webview
      // getComputedStyle scan (WebKitGTK/WebView2 both expose it), shared
      // verbatim from the web oracle, so it reproduces on replay.
      let zc = null;
      try {
        zc = await browser.execute(zeroContrastScan);
      } catch (_) {}
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
      let sec = null;
      try {
        sec = await browser.execute(securityScan);
      } catch (_) {}
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
      const deadInput = await tauriDeadInputProbe(browser).catch(() => []);
      if (deadInput.length) {
        log(
          'EXPLORE:DEADINPUT ' +
            JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              items: deadInput,
            }),
        );
      }
      // BLANK-SCREEN: the state rendered NOTHING -- zero visible text nodes,
      // zero tappable controls, zero visible media -- in a non-empty viewport
      // (the white-screen-of-death: a webview mount that threw before render).
      // observe() runs after the action's settle wait like every scan here,
      // and the scan itself requires a laid-out document.body, so a page still
      // loading never fires. Structural DOM emptiness, no pixels, so it
      // reproduces on replay. Silent when the state shows any content. Shared
      // from web/hygiene-oracles.mjs, injected the way every scan runs here
      // (browser.execute serializes the self-contained function).
      let blank = null;
      try {
        blank = await browser.execute(blankScreenScan);
      } catch (_) {}
      // Settle-then-recheck: a candidate-blank state may be a MID-LOAD blank frame,
      // not a WSOD. Only a state STILL blank AFTER settle fires (mirrors web runner).
      if (blank && blank.length) {
        await settleForSignature(browser);
        try {
          blank = await browser.execute(blankScreenScan);
        } catch (_) {}
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
      // (ReproIt.invariant, pushed to window.__reproit_invariants). Runner-
      // triggered like the web/electron runners; browser.execute serializes the
      // function into the Tauri webview main world, so it reads the page global
      // directly. Each test is isolated; falsy/throw/{ok:false} is a violation.
      // FP-free (the app owns the ground truth); silent when none registered or
      // all held. (Unlike duplicate-submit, this needs no driver request
      // capability Tauri lacks, so it ports cleanly.)
      let invViolations = null;
      try {
        invViolations = await browser.execute(() => {
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
        });
      } catch (_) {}
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
      // completed with no pixels, a FontFace whose load errored, rendered tofu
      // (a visible U+FFFD). Pure DOM/resource status facts; running after the
      // settle wait means loads have resolved, so a still-loading asset never
      // false-positives. Silent when every asset is healthy.
      let assets = null;
      try {
        assets = await browser.execute(brokenAssetScan, [...INJECTED_VALUES]);
      } catch (_) {}
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
      // control that is lost or shrinks below the min target size. Self-restoring
      // sync scan (browser.execute); skipped under the framebuffer probe. Silent
      // when the route scales cleanly. Same self-contained scan as the web tier
      // (the Tauri webview is Chromium/WebKit).
      if (!PROBE) {
        // SCROLL ROUND-TRIP: scroll the primary list away and back and flag
        // content that differs at a pinned offset (a list-recycling bug). Async
        // (it awaits frames), so it runs via executeAsync. Self-restoring; silent
        // when the list is stable or there is no scroller.
        let srt = [];
        try {
          srt = await browser.executeAsync(SCROLLROUNDTRIP_ASYNC_JS);
        } catch (_) {
          srt = [];
        }
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
      }
    }
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
  // ZOOM-REFLOW (WCAG 1.4.10 Reflow, EAA-mandatory), ported from the web
  // runner: re-render the CURRENT route at 200% zoom by halving the window's
  // size, then flag content that breaks (two-dimensional scrolling, a
  // pre-zoom-visible tappable collapsed below 1px -- see zoomReflowScan; a
  // responsively HIDDEN control is intentional adaptation and never fires).
  // WebDriver has no viewport emulation, so the resize surface is the W3C Set
  // Window Rect command (browser.setWindowSize); the webview tracks the
  // window. FP-safe by construction: the scans read LIVE innerWidth/
  // scrollWidth facts at whatever size actually resulted, so a resize the
  // window manager ignores or clamps only under-reports, never invents a
  // finding. Once per distinct route (zoomChecked), never in replay (a replay
  // must reproduce the recorded walk) or probe mode. Self-restoring: the
  // original window size is always put back.
  const zoomChecked = new Set();
  async function zoomReflowCheck(sig, route) {
    let orig = null;
    try {
      orig = await browser.getWindowSize();
      if (!orig || !(orig.width > 0 && orig.height > 0)) {
        orig = null;
        return;
      }
      const preKeys = await browser.execute(zoomTappableKeys);
      await browser.setWindowSize(Math.round(orig.width / 2), Math.round(orig.height / 2));
      await browser.pause(350);
      let items = null;
      try {
        items = await browser.execute(zoomReflowScan, preKeys);
      } catch (e) {
        items = null;
      }
      if (items && items.length) {
        log('EXPLORE:ZOOMREFLOW ' + JSON.stringify({ sig, ...(route ? { route } : {}), items }));
      }
    } catch (e) {
    } finally {
      // Restore the original window size (layout-sensitive oracles depend on it).
      if (orig) {
        try {
          await browser.setWindowSize(orig.width, orig.height);
          await browser.pause(350);
        } catch (e) {}
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
  // runner. The Tauri webview is Chromium-class, so a device rotation is emulated
  // by swapping the window width/height and a background/foreground by the
  // visibilitychange/pagehide-pageshow lifecycle events. Each distinct state sig
  // is transform-tested once. See rotationCheck / backgroundCheck below.
  const rotChecked = new Set();
  const bgChecked = new Set();
  // ROTATION-stability: swap the window size (portrait <-> landscape), reflow,
  // then rotate BACK to the original orientation and re-observe. A correct screen
  // rebuilds the SAME structure once restored; a permanent loss regresses the
  // STRUCTURAL signature (value-state excluded). Round-trip identity is
  // false-positive-free. Guarded on the pre-transform state having content;
  // self-restoring. Returns the re-observed state.
  async function rotationCheck(snap) {
    const expected = snap.structuralSig;
    let orig = null;
    try {
      orig = await browser.getWindowSize();
      if (!orig || !(orig.width > 0 && orig.height > 0)) {
        orig = null;
      } else {
        await browser.setWindowSize(orig.height, orig.width);
        await browser.pause(350);
      }
    } catch (e) {}
    if (orig) {
      try {
        await browser.setWindowSize(orig.width, orig.height);
        await browser.pause(350);
      } catch (e) {}
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
  // BACKGROUND-RESTORE-stability: background the webview (visibilitychange ->
  // hidden, pagehide, blur) then restore it (visible, pageshow, focus) and
  // re-observe. A correct app returns to the SAME screen with state intact; a
  // regression changes the STRUCTURAL signature. No size change; guarded on the
  // pre-transform state having content; self-restoring. Returns the re-observed
  // state.
  async function backgroundCheck(snap) {
    const expected = snap.structuralSig;
    try {
      await browser.execute(() => {
        try {
          Object.defineProperty(document, 'visibilityState', {
            configurable: true,
            get: () => 'hidden',
          });
        } catch (e) {}
        try {
          Object.defineProperty(document, 'hidden', { configurable: true, get: () => true });
        } catch (e) {}
        document.dispatchEvent(new Event('visibilitychange'));
        window.dispatchEvent(new Event('pagehide'));
        window.dispatchEvent(new Event('blur'));
      });
      await browser.pause(300);
      await browser.execute(() => {
        try {
          Object.defineProperty(document, 'visibilityState', {
            configurable: true,
            get: () => 'visible',
          });
        } catch (e) {}
        try {
          Object.defineProperty(document, 'hidden', { configurable: true, get: () => false });
        } catch (e) {}
        document.dispatchEvent(new Event('visibilitychange'));
        window.dispatchEvent(new Event('pageshow'));
        window.dispatchEvent(new Event('focus'));
      });
      await browser.pause(300);
    } catch (e) {}
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
  // LISTENER-LEAK (revisit probe): deliberately NOT ported to the Tauri tier.
  // The oracle needs its add/removeEventListener wrap installed as a page INIT
  // script (before the app's own scripts run), and the WebDriver bridge here has
  // no pre-load init-script injection into the Tauri webview -- so the listener
  // tally's ground truth is unavailable. It runs on the web + Electron (Playwright)
  // tiers, which do have addInitScript.
  // LEAK sampler: in REPLAY mode (the `--soak` tier writes {"replay":[...]}),
  // sample memory at the start and after every action so the Rust soak oracle gets
  // a heap-vs-time series. Off outside replay. t0 anchors t_ms. PRIMARY signal is
  // the webview process RSS (real, coarse); FALLBACK is performance.memory (no CDP
  // over WebDriver); see sampleHeap. tauriPid caches the resolved host pid.
  const t0 = Date.now();
  const tauriPid = { pid: null, tried: false };
  if (replay) await sampleHeap(browser, 0, tauriPid);

  // --record clip capture (route B): arm when this is a replay with a clip plan
  // {sel,label,oracle} + REPROIT_VIDEO_DIR. Start filming the app WINDOW now, so
  // the recording covers the whole replay up to the boxed finding state.
  const clipPlan = fuzz.clip && typeof fuzz.clip.sel === 'string' ? fuzz.clip : null;
  const clipArmed = !!(VIDEO_DIR && replay && clipPlan);
  const clipMov = clipArmed ? joinPath(VIDEO_DIR, 'clip.mov') : null;
  let clipProc = null;
  if (clipArmed) {
    const pid = resolveTauriPid(APP);
    if (pid) clipProc = startClipCapture(pid, clipMov);
    // Small lead-in so the first frames exist before the replay drives the app.
    if (clipProc) await browser.pause(400);
  }
  for (let a = 0; a < budget && stuck < 3; a++) {
    // LEAK sampler: in replay mode, sample once per action (fires BEFORE acting,
    // so action a's sample reflects the heap after the previous action settled).
    if (replay && a > 0) await sampleHeap(browser, Date.now() - t0, tauriPid);
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
    // runner. Tauri has no CDP, so the SAME self-contained in-page pass is injected
    // via executeAsync(): it finds the webview's choice components (native
    // <select>, ARIA tab/radio groups, button-cluster pickers), exercises each
    // option, measures the global-layout effect, and returns the outlier(s) using
    // the SHARED threshold rule -- entirely in-page, so it needs no presented-frame
    // or status stream the WebDriver surface lacks. Non-destructive (it restores
    // each component) and once per state per seed. Each finding -> EXPLORE:CHOICEBUG.
    if (!replay && !exercisedChoiceStates.has(current.sig)) {
      exercisedChoiceStates.add(current.sig);
      let findings = [];
      try {
        findings = await browser.executeAsync(CHOICE_ANOMALY_ASYNC_JS);
      } catch (_) {
        findings = [];
      }
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
      // the webview to REPROIT_SHOTS_DIR and emit the SHOOT marker. It does not
      // move the known state, so no observe/stuck change.
      await shoot(browser, act.slice('shoot:'.length));
      continue;
    }
    if (act === 'back') {
      const before = current.sig;
      const beforeContent = current.content;
      await browser.back().catch(() => {});
      await browser.pause(600);
      const next = await observe();
      if (next.sig !== before) {
        log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'back', to: next.sig }));
        stuck = 0;
      } else if (next.content !== beforeContent) {
        // Layer-1: the action changed on-screen content without moving the
        // structural sig (a value-state change on a capped node). EFFECTIVE, so
        // do not count it as stuck, but no graph edge is added.
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
    try {
      await browser.execute(RESET_LONGTASK_JS);
    } catch (e) {} // jank/hang: drop pre-action longtasks
    try {
      await browser.execute(RESET_FRAME_JS);
    } catch (e) {} // jank/hang: drop pre-action rAF intervals
    // FOCUS-LOSS: record the pre-tap activeElement + open dialog count and arm
    // the probe (TAP_JS's doClick then focuses the control before clicking, the
    // way a real user click does). Checked after the settle below.
    try {
      await browser.execute(focusLossArm);
    } catch (e) {}
    const flickerCapture = clipArmed ? null : startTransitionFlicker(resolveTauriPid(APP));
    if (flickerCapture) await browser.pause(500);
    if (!(await tap(browser, sel))) {
      await finishTransitionFlicker(flickerCapture);
      log('FUZZ:MISS ' + act);
      stuck++;
      continue;
    }
    await browser.pause(700);
    const flicker = await finishTransitionFlicker(flickerCapture);
    if (flicker) {
      log(
        'EXPLORE:FLICKER ' +
          JSON.stringify({
            from: before,
            action: 'tap:' + sel,
            peak: flicker.peak,
            frames: flicker.frames,
          }),
      );
    }
    // JANK/HANG watchdog: did this action block the main thread past the
    // jank/hang floor? Keyed by (from, action) like the flicker oracle, so the
    // Rust side attributes it to this transition and `check` re-confirms it.
    // drainJankForEngine uses the precise Long Tasks path on WebView2/Chromium
    // and the cross-engine rAF path on Tauri's WebKit webview, where Long Tasks
    // is unavailable, so the signal is no longer silent on mac/Linux.
    const tapJank = await drainJankForEngine(browser);
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
    // ground-truth probe runs there and later instrumentation must not corrupt
    // the reading. Whether the tap actually navigated is only known after
    // observe(), so the emit decision is just below.
    let focusLost = false;
    try {
      focusLost = await browser.execute(focusLossCheck);
    } catch (e) {}
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
      // restores the window size, so the reload still sees the original).
      // Never in replay (a recorded clip must not jump window sizes) or probe.
      if (!replay && !PROBE && next.anchor && !zoomChecked.has(next.anchor)) {
        zoomChecked.add(next.anchor);
        await zoomReflowCheck(next.sig, next.anchor);
      }
    } else if (next.content !== beforeContent) {
      // Layer-1 effect detection: the tap changed displayed content (a capped
      // value display) without a structural move. EFFECTIVE, so reset stuck and
      // keep driving; no self-edge is recorded.
      stuck = 0;
    }
    current = next;
  }
  // LEAK sampler: a final sample after the last action, so the series spans the
  // whole soak (start ... last action). No-op outside replay.
  if (replay) await sampleHeap(browser, Date.now() - t0, tauriPid);
  // Final drain: catch any error produced by the last action (or by async work
  // that settled after the last observe).
  await drainErrors(browser);
  // --record clip finalize: resolve the finding's element to a viewport-relative
  // rect (CSS px), write box-spec.json in the webview's logical space, HOLD the
  // boxed state on film, then stop the recorder so it flushes clip.mov. The host
  // runs box-overlay.mjs (clip.mov + box-spec -> boxed clip). Trust gate:
  // FINDING:BOXED drew tells the host whether the element resolved.
  if (clipArmed) {
    await browser.pause(300); // let the post-action state settle on screen
    const box = clipProc ? await resolveClipBox(browser, clipPlan.sel) : null;
    let drew = false;
    if (box) {
      const shownAt = Math.max(0, (Date.now() - t0) / 1000 - 0.2);
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
      await browser.pause(900); // hold the boxed state on camera
    }
    await stopClipCapture(clipProc); // flush clip.mov (no-op if capture never started)
    log(
      'FINDING:BOXED ' +
        JSON.stringify({ oracle: clipPlan.oracle || null, sel: clipPlan.sel, drew }),
    );
  }
  log(`JOURNEY[a] step: explored ${seen.size} states`);
  log('JOURNEY DONE');
  log('All tests passed');
  await browser.deleteSession();
}

// Only auto-run when invoked as the entry point. When imported (e.g. by the
// parity test) the canonical signature is exported without connecting WebDriver.
const INVOKED_DIRECTLY =
  process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href;
if (INVOKED_DIRECTLY) {
  main().catch((e) => {
    log('EXCEPTION CAUGHT BY TAURI RUNNER');
    log(String(e && e.stack ? e.stack : e));
    log('Some tests failed');
    process.exit(0);
  });
}
