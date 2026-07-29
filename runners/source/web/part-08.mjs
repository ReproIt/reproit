                ...(snap.anchor ? { route: snap.anchor } : {}),
                outcome: relationStatus.outcome,
                checks: relationStatus.checks,
              }),
          );
        }
        // ACCESSIBILITY STATE PARITY. Native DOM properties are the application
        // authority; Chromium's computed accessibility tree is the semantic
        // authority. The scanner captures both channels twice and turns any
        // changing, missing, ambiguous, or unsupported evidence into ABSTAIN.
        // Custom ARIA widgets are intentionally excluded: comparing an ARIA
        // attribute with an AX value derived from that same attribute is not an
        // independent proof of application-state parity.
        const a11yState = await scanAccessibilityStateParity(page).catch(() => ({
          outcome: 'ABSTAIN',
          checks: [],
          items: [],
        }));
        log(
          'EXPLORE:A11YSTATESTATUS ' +
            JSON.stringify({
              sig: snap.sig,
              ...(snap.anchor ? { route: snap.anchor } : {}),
              outcome: a11yState.outcome,
              checks: a11yState.checks,
            }),
        );
        if (a11yState.items.length) {
          log(
            'EXPLORE:A11YSTATE ' +
              JSON.stringify({
                sig: snap.sig,
                ...(snap.anchor ? { route: snap.anchor } : {}),
                items: a11yState.items,
              }),
          );
        }
        // SECURITY hygiene: pure DOM/URL predicates, deterministic and FP-free.
        //   - tabnabbing: a cross-origin target=_blank link with no rel=noopener
        //     (the opened page can rewrite window.opener.location -- a phishing
        //     vector). Fires on any page.
        //   - insecure-form / mixed-content: an HTTPS document with an http: form
        //     action or http: subresource. Gated on https so an http dev page
        //     never false-positives.
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
        // BLANK-SCREEN candidate: structural emptiness alone is ambiguous and
        // is never a finding. It becomes reportable only when the independently
        // filtered first-party exception channel observed a failure on this
        // exact URL since the preceding state observation.
        let blank = await page.evaluate(blankScreenScan).catch(() => null);
        // A candidate-blank state may just be a MID-LOAD blank frame (the JS has
        // not populated the DOM yet), which is a transient loading state, NOT a
        // white-screen-of-death. Settle for content (network idle + DOM quiescence)
        // and re-check: only a state STILL blank AFTER settle fires. The settle is
        // paid ONLY on the rare candidate-blank state, so a normal state is unaffected.
        if (blank && blank.length) {
          await settleForSignature(page);
          blank = await page.evaluate(blankScreenScan).catch(() => null);
        }
        const currentUrl = page.url();
        const blankAuthority = blankScreenAuthority(
          lastAppFailure,
          failureCountAtLastObservation,
          currentUrl,
        );
        if (blank && blank.length && blankAuthority) {
          log(
            'EXPLORE:BLANKSCREEN ' +
              JSON.stringify({
                sig: snap.sig,
                ...(snap.anchor ? { route: snap.anchor } : {}),
                authority: blankAuthority,
                items: blank,
              }),
          );
        } else if (blank && blank.length) {
          log(
            'EXPLORE:BLANKSCREENCANDIDATE ' +
              JSON.stringify({
                sig: snap.sig,
                ...(snap.anchor ? { route: snap.anchor } : {}),
                reason: 'visual-emptiness-without-independent-authority',
                items: blank,
              }),
          );
        }
        // APP-INVARIANT: the app's OWN predicates, registered via the SDK
        // (ReproIt.invariant("id", fn), which pushes to the stable global
        // window.__reproit_invariants). Evaluate each on this settled state; a
        // predicate that returns falsy, throws, or an { ok:false, message }
        // object is a violation. The app owns this ground truth, so a reported
        // violation is real (FP-free). Silent when the app registered none or
        // all held. Each test is isolated so one throwing predicate cannot
        // suppress the others.
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
              if (!ok) out.push({ id: String(it.id), message: message });
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
        // BROKEN-ASSET: visible dead images/tofu plus same-origin critical CSS
        // and application scripts that failed or were browser-rejected. The
        // settled DOM is correlated with response/error facts, so optional or
        // unreferenced requests never become findings.
        const assets = await page.evaluate(brokenAssetScan, [...INJECTED_VALUES]).catch(() => null);
        const criticalAssets = await page
          .evaluate(criticalResourceScan, [...criticalResourceFacts.values()])
          .catch(() => null);
        const brokenAssets = [...(assets || []), ...(criticalAssets || [])].slice(0, 20);
        if (brokenAssets.length) {
          log(
            'EXPLORE:BROKENASSET ' +
              JSON.stringify({
                sig: snap.sig,
                ...(snap.anchor ? { route: snap.anchor } : {}),
                items: brokenAssets,
              }),
          );
        }
        // Both probes below drive the scroller, so they are skipped while a human
        // paces the replay (humanPaced): they are the visible up/down churn.
        if (!PROBE && !humanPaced) {
          // SCROLL ROUND-TRIP: scroll the primary list away and back and flag
          // content that differs at a pinned offset (a list-recycling /
          // virtualization bug rebinds a different row to the same position).
          // Self-restoring; value-state normalized out, so it reproduces on
          // replay. Silent when the list is stable or there is no scroller.
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
          // DEAD-INPUT: dispatch a trusted wheel (and a strict-safety-net
          // keystroke) and prove the input vanished: no event anywhere, no
          // delta, and no handler claimed it with preventDefault. Modal
          // interceptors and prevented inputs abstain; a confirmed finding
          // survived two probes. Self-restoring (offsets, probe char,
          // pointer). See dead-input-oracle.mjs for the zero-FP guards.
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
        // BROKEN-ROUTE: the document for this URL came back with a status that
        // means the resource is GENUINELY GONE -- 404 (not found) or 410 (gone).
        // ONLY those. Not 401/403 (intentional auth gates), 429 (rate limit),
        // 3xx (redirect), 405/501 (method semantics), or 5xx (a transient server
        // error is not a broken LINK) -- flagging any of those was a false
        // positive. Looked up by exact normalized path + query (snap.path), not
        // the signature anchor: fragments never reach the server, while queries
        // often select a distinct server route and must retain their response.
        const status = snap.path ? navStatus[snap.path] : undefined;
        if (typeof status === 'number' && isDeadRouteStatus(status)) {
          // SPA SOFT-404 guard: a static host (naiveui, GitHub Pages, Netlify) can
          // answer a deep path with HTTP 404 yet still serve index.html, and the
          // client router renders the CORRECT screen. The runner is standing ON that
          // rendered screen right now, so if it is a real app view (filled mount,
          // real interactive content, no not-found heading) the 404 status is not a
          // broken route. A genuine error page still fails the check and fires.
          const view = await page.evaluate(soft404View).catch(() => null);
          if (!isSoftHandled(view)) {
            log(
              'EXPLORE:BROKENROUTE ' +
                JSON.stringify({
                  sig: snap.sig,
                  ...(snap.anchor ? { route: snap.anchor } : {}),
                  status,
                  // Exact source attribution: the page + link that led here.
                  ...(lastNav ? { from: lastNav.from, action: lastNav.action } : {}),
                }),
            );
          }
        }
        // Operability/accessibility ground truth LAST: its keyboard-activation
        // probe mutates the DOM and its framebuffer probe reloads the page, so it
        // must run after the snapshot, the state record, AND the scans above. The
        // next action then drives the live (possibly mutated/reloaded) DOM.
        //
        // Skipped WHOLE while a human paces the replay. Its Tab traversal walks
        // focus through up to 60 elements, which scrolls the page end to end
        // between two inspected actions. Dropping only the traversal is not an
        // option: keyboard reachability would then read as empty and the record
        // would claim every control is unreachable, so no ground truth is
        // emitted for this state at all rather than false ground truth.
        if (!humanPaced) await emitGroundtruth(page, gtCdp, snap.sig);
      }
      // Record same-origin APP link targets on this page (dedup by pathname, first
      // source state wins) for the end-of-crawl broken-route link check. Exclude
      // non-app links the probe should never fetch: a `download` link (a file
      // download, not a navigable route) and an href whose path ends in a file /
      // asset extension (.zip/.pdf/.dmg/.exe/... plus static web assets). A 404 on
      // an asset is a broken-asset concern, not a broken-route, and many assets
      // legitimately answer non-200 to a bare fetch.
      try {
        const links = await page.evaluate(collectRouteLinks, ASSET_EXT_SOURCE);
        for (const p of links) if (!seenLinks.has(p)) seenLinks.set(p, snap.sig);
      } catch (_) {}
      // Advance the correlation floor on every observation, including revisits.
      // Otherwise an exception followed by a non-new visible state could be
      // reused incorrectly as authority for a later unrelated blank route.
      failureCountAtLastObservation = replayErrorCount;
      return snap;
    }

    // ZOOM-REFLOW (WCAG 1.4.10 Reflow, EAA-mandatory): re-render the CURRENT
    // route at 200% zoom by halving the viewport's CSS size (1280x720 -> the
    // reflow-equivalent 640x360), then flag content that breaks: the document
    // now requires TWO-DIMENSIONAL scrolling (fixed-width content grew a
    // horizontal scrollbar by >16px), or a previously visible tappable's hit
    // rect collapsed below 1px while still rendered (a responsively HIDDEN
    // control is intentional adaptation and never fires -- see
    // zoomReflowScan). Once per distinct route (the caller dedupes via
    // zoomChecked) and never in replay or probe mode (guarded at the call
    // sites). Self-restoring: the original viewport is always put back so the
    // walk continues undisturbed.
    async function zoomReflowCheck(sig, route) {
      try {
        const preKeys = await page.evaluate(zoomTappableKeys);
        await page.setViewportSize({ width: Math.round(VW / 2), height: Math.round(VH / 2) });
        await page.waitForTimeout(350);
        const items = await page.evaluate(zoomReflowScan, preKeys).catch(() => null);
        if (items && items.length) {
          log('EXPLORE:ZOOMREFLOW ' + JSON.stringify({ sig, ...(route ? { route } : {}), items }));
        }
      } catch (_) {
      } finally {
        // Restore the pinned viewport (layout-sensitive oracles depend on it).
        try {
          await page.setViewportSize({ width: VW, height: VH });
          await page.waitForTimeout(350);
        } catch (_) {}
      }
    }

    // ROTATION-stability (lifecycle-metamorphic): rotate the viewport by
    // swapping width/height (the orientation change a device rotation /
    // split-screen triggers), let it reflow, then rotate BACK to the original
    // orientation and re-observe. A correct screen reflows but rebuilds the SAME
    // structure once the original orientation is restored; an app that mishandles
    // the resize/orientationchange lifecycle -- dropping content or state that
    // never comes back -- regresses the STRUCTURAL signature (value-state
    // excluded, so a re-fetched timestamp never trips it). Round-trip identity
    // (same orientation in and out) makes it false-positive-free: a legit
    // responsive breakpoint swap is symmetric and restores, so it never fires;
    // only a permanent loss does. Guarded on the pre-transform state having
    // content, so an already-empty screen is not asserted about. Self-restoring
    // (viewport put back); never in replay/probe. Returns the re-observed state.
    async function rotationCheck(snap) {
      const expected = snap.structuralSig;
      // Do not attribute ordinary async settling to rotation. The source must
      // still be structurally identical after a quiet beat before we transform.
      await page.waitForTimeout(300);
      const pre = await snapshot(page, valueNodeSelectors).catch(() => null);
      if (!pre || pre.structuralSig !== expected) return pre || snap;
      try {
        await page.setViewportSize({ width: VH, height: VW });
        await page.waitForTimeout(350);
      } catch (_) {}
      try {
        await page.setViewportSize({ width: VW, height: VH });
        await page.waitForTimeout(350);
      } catch (_) {}
      const after = await observe();
      if (snap.tappables && snap.tappables.length > 0 && after.structuralSig !== expected) {
        // Reconfirm the destination after another quiet beat. A lazy/virtualized
        // view often mounts in phases after resize; only a stable permanent loss
        // is a lifecycle defect.
        await page.waitForTimeout(700);
        const confirmed = await snapshot(page, valueNodeSelectors).catch(() => null);
        if (confirmed && confirmed.structuralSig === after.structuralSig) {
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
      }
      return after;
    }

    // BACKGROUND-RESTORE-stability (lifecycle-metamorphic): send the page to the
    // background (visibilitychange -> hidden, pagehide, blur) then restore it
    // (visibilitychange -> visible, pageshow, focus) and re-observe. A correct
    // app returns to the SAME screen with its state intact; one that drops you on
    // a different screen or loses state across the lifecycle regresses the
    // STRUCTURAL signature. No size change, so it is a direct before/after
    // comparison (value-state excluded); guarded on the pre-transform state
    // having content. Self-restoring (the page ends visible); never in
    // replay/probe. Returns the re-observed state.
    async function backgroundCheck(snap) {
      const expected = snap.structuralSig;
      await page.waitForTimeout(300);
      const pre = await snapshot(page, valueNodeSelectors).catch(() => null);
      if (!pre || pre.structuralSig !== expected) return pre || snap;
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
        await page.waitForTimeout(700);
        const confirmed = await snapshot(page, valueNodeSelectors).catch(() => null);
        if (confirmed && confirmed.structuralSig === after.structuralSig) {
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
      }
      return after;
    }
    // LISTENER-LEAK (opt-in, REPROIT_LISTENERLEAK=1): drive N revisits of `route`
    // with history back/forward (client-side, NO reload -- the init-script
    // listener tally survives) and watch the live event-listener count (adds -
    // removes) and the attached DOM-node count. A route that mounts
    // listeners/nodes it never releases on unmount climbs MONOTONICALLY across
    // revisits; a stable route is flat after warmup. The first sample is taken
    // AFTER one warmup revisit so a route's one-time persistent listeners are not
    // mistaken for a leak. Fires only when a metric strictly increases on EVERY
    // revisit and rises past the floor. Once per route (the caller dedupes via
    // leakChecked), never in replay/probe mode. Self-restoring: back/forward net
    // to the entry we started on, so the walk continues undisturbed.
    async function listenerLeakCheck(route) {
      const CYCLES = 5; // revisit samples compared for a monotonic climb
      const MIN_RISE = 5; // net climb (last - first) a metric must show to count
      const samples = [];
      try {
        for (let i = 0; i < CYCLES; i++) {
          await page.goBack({ timeout: 3000 }).catch(() => {});
          await page.waitForTimeout(250);
          await page.goForward({ timeout: 3000 }).catch(() => {});
          await page.waitForTimeout(250);
          // Confirm the forward step landed back on the SAME route; if history
          // drifted (a redirect, an off-route back), abort so we never compare
          // samples from different screens.
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
        if (rise >= MIN_RISE)
          items.push({ kind, first: series[0], last: series[series.length - 1] });
      };
      // Drop the first sample as warmup (the route's initial persistent mount),
      // then require a strict monotonic climb across the remaining revisits.
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

    let current = await observe();
    launchSig = current.sig;
    // ZOOM-REFLOW for the start route: the walk's tap-edge check only covers
    // routes NAVIGATED to, so the launch screen gets its zoomed re-render here.
    if (!replay && !PROBE && current.anchor && !zoomChecked.has(current.anchor)) {
      zoomChecked.add(current.anchor);
      await zoomReflowCheck(current.sig, current.anchor);
    }
    let stuck = 0;
    const prefix = fuzz.prefix || null;
    const prefixLen = prefix ? prefix.length : 0;
    const mapMode = !replay && !prefix && !fuzz.seed;
    const coverageMode = isCoverageWalkConfig(fuzz);
    const budget = replay
      ? replay.length
      : (mapMode && !FUZZ_CONFIGURED ? MAP_ACTION_BUDGET : fuzz.budget || ACTION_BUDGET) +
        prefixLen;

    // LEAK sampler: in REPLAY mode (the `--soak` tier writes {"replay":[...]}),
    // sample the web heap once at the start and after every action, so the Rust
    // soak oracle gets a heap-vs-time series to read the slope from. Off outside
    // replay (a plain fuzz walk is not a soak). t0 anchors t_ms to walk start.
    const t0 = Date.now();
    if (replay) await sampleHeap(page, gtCdp, 0);

    let actions = 0;
    for (; actions < budget && stuck < 3; actions++) {
      // LEAK sampler: in replay mode, sample the heap once per action (this fires
      // BEFORE acting, so action k's sample reflects the heap after the previous
      // action settled; together with the start + final samples it forms the
      // monotonic series the soak slope is read from). No-op outside replay.
      if (replay && actions > 0) await sampleHeap(page, gtCdp, Date.now() - t0);
      // LIFECYCLE-metamorphic oracles (rotation, background-restore): once per
      // distinct state, apply a device-lifecycle transform and assert the
      // structural signature survives it. Self-restoring, so `current` is refreshed
      // to the (restored) reality afterwards; never in replay/probe (a recorded
      // clip must not jump viewport or fire lifecycle events). Runs before action
      // selection so the walk continues from the re-observed state.
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
      // COMPONENT-CHOICE differential (fuzz only, not replay): when the current
      // state exposes a multi-choice component not yet exercised this seed,
      // exhaustively select each choice and flag a global-layout outlier. Each
      // group is its own bounded sub-traversal, consuming one action slot.
      if (!replay) {
        // Record the ordinary frontier before the differential consumes this
        // slot. Otherwise a budget-one scan could finish without admitting that
        // the screen's taps and back edge remain unchecked.
        if (coverageMode) {
          rememberActions(actionsByState, current.sig, coverageActions(current));
        }
        let exercised = false;
        // ARIA / button-cluster groups (from the snapshot tappables) plus native
        // <select> components (FEATURE 1; queried live since the snapshot maps a
        // <select> to a text field and so never surfaces its options).
        const groups = detectChoiceGroups(current.tappables).concat(await detectSelectGroups(page));
        for (const group of groups) {
          const gkey =
            current.sig + '|' + group.role + '|' + group.opts.map((o) => o.sel).join(',');
          if (exercisedGroups.has(gkey)) continue;
          exercisedGroups.add(gkey);
          await exerciseChoiceGroup(page, group, current.sig);
          current = await observe();
          exercised = true;
          break;
        }
        if (exercised) continue;
      }
      let act;
      if (replay) act = replay[actions];
      else if (prefix && actions < prefixLen) act = prefix[actions];
      else if (fuzz.seed && !coverageMode) {
        // Inverse-visit-count weighted pick: weight each candidate edge by
        // 1/(1+globalVisits) from the edgeWeights snapshot, plus 'back'.
        // Seeded + deterministic, so replays reproduce exactly. Candidates are
        // addressed by STRUCTURAL selector (key, else role+index), never by
        // visible text, so the seeded pick and any replay are locale-invariant.
        // Candidate edges: tap every tappable; for text fields ALSO offer a type
        // edge whose adversarial value is chosen deterministically from the seed
        // (the option string carries the value id so a replay reconstructs it).
        // Exclude cross-origin links from the action set: tapping one leaves the
        // app (see isExternalLink). They stay in `tappables` so role:<role>#<idx>
        // indices are unchanged; they are just never chosen as an edge.
        const actable = current.tappables.filter((e) => !e.external);
        const taps = actable.map((e) => e.sel).sort();
        const textSels = actable
          .filter((e) => e.role === 'textfield')
          .map((e) => e.sel)
          .sort();
        const typeOpts = textSels.map((s) => {
          // Derive the adversarial id from seed + selector so the same field on
          // the same seed always types the same value (reproducible), but
          // different fields can get different values.
          const idx = pick(ADVERSARIAL.length === 0 ? 1 : ADVERSARIAL.length);
          return 'type:' + s + '=' + adversarialFor(idx).id;
        });
        const ew = (fuzz.edgeWeights && fuzz.edgeWeights[current.sig]) || {};
        const options = taps
          .map((s) => 'tap:' + s)
          .concat(typeOpts)
          .concat(['back']);
        if (coverageMode) rememberActions(actionsByState, current.sig, options);
        const contractActions = new Set(fuzz.contractActions || []);
        const weights = options.map((o) => (contractActions.has(o) ? 4 : 1) / (1 + (ew[o] || 0)));
        const total = weights.reduce((a, b) => a + b, 0);
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
        // Scan/map coverage walks use the explicit frontier even when they carry
        // a seed. A seed selects randomized fuzz walks; it must not make a
        // coverage walk repeat an already-tried edge while reachable work waits.
        const actions = coverageActions(current);
        rememberActions(actionsByState, current.sig, actions);
        act = firstUntriedAction(actionsByState, triedEdges, current.sig);
        if (!act) {
          const path = pathToFrontier(graph, actionsByState, triedEdges, current.sig);
          act = path && path.length ? path[0] : null;
        }
        if (!act && hasFrontier(actionsByState, triedEdges) && current.sig !== launchSig) break;
        if (!act) break;
      }

      if (replay && INSPECT && !inspectAutoContinue) {
        const model = inspectStepModel(act, actions + 1, replay.length, current);
        log(`INSPECT:PAUSE step=${model.stepIndex}/${model.totalSteps} action=${act}`);
        const decision = await inspectReplayStep(page, model, INSPECT_WAIT_MS);
        log(`INSPECT:DECISION step=${model.stepIndex} decision=${decision}`);
        inspectAutoContinue = decision === 'continue';
      }
      log('FUZZ:ACT ' + act);
      // Record/review HUD: when recording a REPLAY (`check --record`), draw a
      // paced on-screen caption of each action so a human can actually follow the
      // repro - the video analogue of the cloud "path to the bug". Only when
      // replaying AND recording, so a normal fuzz hunt is never slowed.
      if (replay && VIDEO_DIR && !act.startsWith('assert:') && !act.startsWith('shoot:')) {
        const isLast = actions >= replay.length - 1;
        const o = String(fuzz.highlight || '');
        // The final action of a sequence-bug clip is the one that breaks the app.
        const trigger = isLast && /hang|jank|exception|crash/.test(o);
        if (act.startsWith('tap:')) {
          // Highlight the element reproit is ABOUT to tap, with its human-readable
          // name (not `role:link#7`), drawn while the page is still live. For the
          // final trigger of a sequence-bug clip (hang/crash/jank) it is the bug
          // itself, so box it RED with the outcome; other taps are BLUE "here's what
          // I clicked". Drawing pre-tap (a PREVIEW box, no click) means a tap that
          // navigates/freezes still shows the right element (a frozen page can't be
          // annotated afterward), and lets the clip LINGER on the doomed control
          // before it is actually tapped.
          const sel = act.slice('tap:'.length);
          const target = current.tappables.find((e) => e.sel === sel);
          let name = (target && target.label && String(target.label).trim()) || sel;
          if (name.length > 36) name = name.slice(0, 35) + '…';
          const outcome = /hang/.test(o)
            ? '  → froze'
            : /jank/.test(o)
              ? '  → janked'
              : /exception|crash/.test(o)
                ? '  → crashed'
                : /dead/.test(o)
                  ? '  → no effect'
                  : '';
          // Highlight the element in RED before acting on it -- every clicked control
          // in a clip is boxed red the beat BEFORE the click, so the viewer always
          // sees what is about to be actuated (the trigger also carries its outcome).
          await tap(page, sel, {
            box: 'about to tap  ' + name + (trigger ? outcome : ''),
            boxColor: '#e21f1f',
          }).catch(() => {});
        } else {
          await showActionHud(page, act, actions, replay.length).catch(() => {});
        }
        // Hold before performing the action. Linger LONGEST on the control that is
        // about to break (the crash/jank/hang trigger) so the recorded clip clearly
        // shows the doomed element for a beat -- highlighted, pausable -- and THEN
        // breaks. Other final steps get a shorter beat; mid-sequence steps are quick.
        await page.waitForTimeout(trigger ? 2600 : isLast ? 1600 : 950);
      }
      if (act.startsWith('shoot:')) {
        // Screenshot point (e.g. a `do: shoot:<name>` journey/tour step): capture
        // the current screen to REPROIT_SHOTS_DIR and emit the SHOOT marker. Like
        // an assertion, it does not move the known state (no observe/stuck change).
        await shoot(page, act.slice('shoot:'.length));
        continue;
      }
      if (act.startsWith('auth:')) {
        // Session bypass: restore a pre-authenticated session for the account so a
        // journey can exercise a feature without re-driving the login UI each run.
        // The orchestrator injects REPROIT_SECRET_<ACCT>_STORAGE (a JSON map of
        // localStorage entries) from the vault; we seed it and reload so the app
        // boots authenticated. Absent/garbage => FUZZ:MISS (the journey is stale,
        // not a pass: it never reached the authenticated state it assumed).
        const acct = act.slice('auth:'.length);
        const envName =
          'REPROIT_SECRET_' + acct.replace(/[^A-Za-z0-9]/g, '_').toUpperCase() + '_STORAGE';
        const raw = process.env[envName];
        if (!raw) {
          log('FUZZ:MISS ' + act + ' (no ' + envName + ')');
          stuck++;
          continue;
        }
        let store;
        try {
          store = JSON.parse(raw);
        } catch {
          log('FUZZ:MISS ' + act + ' (bad JSON in ' + envName + ')');
          stuck++;
          continue;
        }
        await page.addInitScript((entries) => {
          try {
            for (const [k, v] of Object.entries(entries)) localStorage.setItem(k, v);
          } catch (_) {}
        }, store);
        await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
        await page.waitForTimeout(replay ? 700 : 400);
        current = await observe(); // observe() emits FUZZ:STATE in replay mode
        continue;
      }
      if (act.startsWith('visit:')) {
        const requested = act.slice('visit:'.length);
        if (!validRouteAccessPath(requested)) {
          log('FUZZ:MISS ' + act + ' (route must be a bounded same-origin path)');
          stuck++;
          continue;
        }
        const routeObservation = await visitRoute(page, requested, APP_ORIGIN);
        log('REPROIT:ROUTE_ACCESS ' + JSON.stringify(routeObservation));
        if (!routeObservation || !routeObservation.settled) {
          stuck++;
          continue;
        }
        current = await observe();
        continue;
      }
      if (act.startsWith('assert:')) {
        // Journey assertions: evaluated against the live screen at this point in
        // the replay. They never move state (no observe/stuck change); the verdict
        // is reported via FUZZ:ASSERT and the CLI maps a fail to a stale run.
        const body = act.slice('assert:'.length);
        if (body.startsWith('state=')) {
          const want = body.slice('state='.length);
          const got = current.sig; // current is the state after the previous action
          log(
            'FUZZ:ASSERT ' +
              (got === want ? 'pass' : 'fail') +
              ' state want=' +
              want +
              ' got=' +
              got,
          );
        } else if (body.startsWith('text=')) {
          const want = body.slice('text='.length);
          const ok = await page
            .evaluate((t) => !!(document.body && document.body.innerText.includes(t)), want)
            .catch(() => false);
          log('FUZZ:ASSERT ' + (ok ? 'pass' : 'fail') + ' text=' + JSON.stringify(want));
        } else if (body.startsWith('route=')) {
          const want = body.slice('route='.length);
          let got = '';
          try {
            const url = new URL(page.url());
            if (url.origin === APP_ORIGIN) got = publicRouteKey(url.pathname);
          } catch (_) {}
          log(
            'FUZZ:ASSERT ' +
              (got === want ? 'pass' : 'fail') +
              ' route want=' +
              want +
              ' got=' +
              got,
          );
        } else if (body.startsWith('count:')) {
          const rest = body.slice('count:'.length);
          const eq = rest.lastIndexOf('=');
          const finder = eq >= 0 ? rest.slice(0, eq) : rest;
          const want = eq >= 0 ? parseInt(rest.slice(eq + 1), 10) : 0;
          const got = await page.evaluate(countMatching, finder).catch(() => -1);
