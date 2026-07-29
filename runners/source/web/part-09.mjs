          log(
            'FUZZ:ASSERT ' +
              (got === want ? 'pass' : 'fail') +
              ' count ' +
              finder +
              ' want=' +
              want +
              ' got=' +
              got,
          );
        } else {
          log('FUZZ:ASSERT fail unknown-assertion ' + body);
        }
        continue;
      }
      if (act === 'back') {
        const before = current.sig;
        triedEdges.add(edgeKey(before, 'back'));
        const beforeContent = current.content;
        const origin = new URL(APP_URL).origin;
        await page.goBack({ timeout: 3000 }).catch(() => {});
        await page.waitForTimeout(600);
        // Stepping off the app (about:blank) is not a real state: go forward.
        if (!page.url().startsWith(origin)) {
          await page.goto(APP_URL, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
          await page.waitForTimeout(400);
          stuck++;
          current = await observe();
          continue;
        }
        const next = await observe();
        if (next.sig !== before) {
          log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'back', to: next.sig }));
          rememberEdge(graph, before, 'back', next.sig);
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
      if (act.startsWith('type:')) {
        // type:<sel>=<valueId> -> focus the field and type the value.
        const body = act.slice('type:'.length);
        const eq = body.lastIndexOf('=');
        const sel = eq >= 0 ? body.slice(0, eq) : body;
        const valId = eq >= 0 ? body.slice(eq + 1) : 'normal';
        // PRECEDENCE: an explicit property-matched fixture input for this field
        // wins over the adversarial-class token. The class token still picks the
        // value when no input matches (the existing path, unchanged). Both are
        // deterministic, so the replay reproduces the same text either way.
        const fixtureVal = inputValueFor(sel, inputs);
        const value =
          fixtureVal != null
            ? fixtureVal
            : ADVERSARIAL_BY_ID[valId] !== undefined
              ? ADVERSARIAL_BY_ID[valId]
              : expandEnv(valId);
        triedEdges.add(edgeKey(current.sig, act));
        const before = current.sig;
        const beforeContent = current.content;
        await page.evaluate(markAnchors, ANCHOR_SEL).catch(() => {});
        await page
          .evaluate(() => {
            window.__reproitLongTasks = [];
            window.__reproitFrameIntervals = [];
          })
          .catch(() => {}); // jank/hang: drop pre-action longtasks + frame intervals
        // Jank: machine-invariant forced-layout baseline.
        const perfBeforeType = await readLayoutCounters(gtCdp);
        // Tier 2 (gated): record presented frames.
        const typePix = await startScreencastCapture(gtCdp);
        const ok = await typeInto(page, sel, value, { mark: recording });
        if (!ok) {
          if (typePix) await typePix.stop();
          log('FUZZ:MISS ' + act);
          stuck++;
          continue;
        }
        // Read before settle so this captures synchronous reflow only.
        const perfAfterType = await readLayoutCounters(gtCdp);
        // Replays settle longer than the fuzz walk: under recording/CI load the
        // app's handler (and any uncaught throw it triggers) needs more wall-clock
        // to run and for `pageerror` to fire, so a deterministic crash isn't
        // missed. The fuzz walk stays fast.
        await page.waitForTimeout(replay ? 1100 : 700);
        const typeChurn = await page.evaluate(churnedAnchors, ANCHOR_SEL).catch(() => null);
        if (typeChurn && typeChurn.length) {
          log(
            'EXPLORE:RERENDER ' +
              JSON.stringify({
                from: before,
                action: 'type:' + sel + '=' + valId,
                churned: typeChurn,
              }),
          );
        }
        // Typing + Enter can navigate (e.g. a search form submitting to another
        // origin). Stay on the app-under-test: drop off-origin destinations.
        if (await recoverIfOffOrigin()) {
          if (typePix) await typePix.stop();
          stuck++;
          current = await observe();
          continue;
        }
        await finishScreencastCapture(typePix, before, 'type:' + sel + '=' + valId);
        const typeJank = await drainJankForEngine(page);
        const typeThrash = layoutThrash(perfBeforeType, perfAfterType);
        if (typeThrash && (!typeJank || typeJank.kind !== 'hang')) {
          log(
            'EXPLORE:JANK ' +
              JSON.stringify({
                from: before,
                action: 'type:' + sel + '=' + valId,
                bucket: typeThrash.count,
                unit: 'layouts',
                count: typeThrash.count,
              }),
          );
        } else if (typeJank) {
          log(
            'EXPLORE:' +
              (typeJank.kind === 'hang' ? 'HANG' : 'JANK') +
              ' ' +
              JSON.stringify({
                from: before,
                action: 'type:' + sel + '=' + valId,
                bucket: typeJank.bucket,
                count: typeJank.count,
              }),
          );
        }
        if (recording) {
          lastTriggerLabel =
            typeJank || typeThrash
              ? typeJank && typeJank.kind === 'hang'
                ? 'froze'
                : 'jank'
              : null;
          lastFlickerKeys = typeChurn && typeChurn.length ? typeChurn : null;
        }
        const next = await observe();
        if (next.sig !== before) {
          log(
            'EXPLORE:EDGE ' +
              JSON.stringify({ from: before, action: 'type:' + sel + '=' + valId, to: next.sig }),
          );
          rememberEdge(graph, before, 'type:' + sel + '=' + valId, next.sig);
          stuck = 0;
        } else if (next.content !== beforeContent) {
          stuck = 0; // Layer-1: content changed without a structural move; effective.
        } else stuck++;
        current = next;
        continue;
      }
      const sel = act.slice('tap:'.length);
      // Key MUST match the picker's edge form (`tap:<sel>`, line ~3337); recording
      // the bare `<sel>` left every tap looking perpetually untried, so the
      // deterministic walk kept re-tapping the first control and under-explored.
      triedEdges.add(edgeKey(current.sig, 'tap:' + sel));
      const before = current.sig;
      const beforeContent = current.content;
      const beforeAnchor = current.anchor;
      await page.evaluate(markAnchors, ANCHOR_SEL).catch(() => {});
      // Remember the source page + link before this (possibly navigating) tap, so a
      // broken-route landed on next is attributed to exactly here, not reverse-matched.
      lastNav = { from: before, action: 'tap:' + sel };
      await page
        .evaluate(() => {
          window.__reproitLongTasks = [];
          window.__reproitFrameIntervals = [];
        })
        .catch(() => {}); // jank/hang: drop pre-action longtasks + frame intervals
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
      // action sequence alone (the probe's own second dispatch was recorded as
      // a FUZZ:ACT when it fired), or minimization under REPROIT_DUPSUBMIT=1
      // shrinks the double tap to one and the saved repro silently depends on
      // the probe env at replay time.
      const dupTapTarget = DUPSUBMIT && !replay ? current.tappables.find((e) => e.sel === sel) : null;
      const dupProbe =
        DUPSUBMIT &&
        !replay &&
        !recording &&
        !!dupTapTarget &&
        dupTapTarget.role === 'button' &&
        !dupProbed.has(edgeKey(before, 'tap:' + sel));
      let dupUrlBefore = null;
      if (dupProbe) {
        dupProbed.add(edgeKey(before, 'tap:' + sel));
        dupUrlBefore = page.url();
        dupReqLog = [];
      }
      // Jank: machine-invariant forced-layout baseline.
      const perfBefore = await readLayoutCounters(gtCdp);
      const tapPix = await startScreencastCapture(gtCdp); // Tier-2 (gated): record presented frames
      const ok = await tap(page, sel, { mark: recording });
      if (!ok) {
        if (tapPix) await tapPix.stop();
        dupReqLog = null;
        log('FUZZ:MISS ' + act);
        stuck++;
        continue;
      }
      // JANK: read the forced-layout counter NOW, right after the synchronous
      // handler returned and BEFORE the settle wait, so the delta counts only the
      // handler's own reflows -- not animation frames over the settle (which would
      // be machine-dependent and reintroduce flake).
      const perfAfterTap = await readLayoutCounters(gtCdp);
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
          // RECORD the second dispatch into the action sequence (FUZZ:ACT) only when
          // it actually fired: the walk continues from the post-double-click state, so
          // a kept repro must replay both clicks or it diverges (the probe otherwise
          // mutated state invisibly).
          if (dupDispatched) log('FUZZ:ACT tap:' + sel);
        }
        if (!dupDispatched) dupReqLog = null;
      }
      // Replays settle longer than the fuzz walk (see the type branch): a
      // deterministic crash must have time to throw + flush `pageerror` under load.
      await page.waitForTimeout(replay ? 1100 : 700);
      const tapChurn = await page.evaluate(churnedAnchors, ANCHOR_SEL).catch(() => null);
      if (tapChurn && tapChurn.length) {
        log(
          'EXPLORE:RERENDER ' +
            JSON.stringify({
              from: before,
              action: 'tap:' + sel,
              churned: tapChurn,
            }),
        );
      }
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
      // SEQUENCE-BUG clip (hang/crash/jank), FINAL action: this tap IS the trigger
      // and the page may now be frozen/busy. The churn + observe below each do a
      // page.evaluate, which BLOCKS on a busy main thread for ~30s -- that is what
      // made a hang clip ~80s long. So for a clip we skip them and detect the bug by
      // RESPONSIVENESS (a hang's own definition: the page stops responding), which
      // is fast AND faithful (it really re-fired), not a timeout that gives up.
      if (
        recording &&
        replay &&
        actions >= replay.length - 1 &&
        /hang|jank|exception|crash/.test(String(fuzz.highlight || ''))
      ) {
        if (tapPix) await tapPix.stop();
        if (/hang|jank/.test(String(fuzz.highlight))) {
          const responsive = await Promise.race([
            page
              .evaluate(() => true)
              .then(
                () => true,
                () => true,
              ),
            new Promise((r) => setTimeout(() => r(false), 2500)),
          ]);
          if (!responsive) lastTriggerLabel = 'froze'; // unresponsive = the hang re-fired
        }
        // crash: the pageerror handler already bumped replayErrorCount.
        break; // end the replay; the end-of-replay block emits FINDING:BOXED + holds
      }
      // ORIGIN GUARD: a tap on an outbound link (footer "View on GitHub", a
      // social link) navigates off the app-under-test's origin. That page is NOT
      // a state of the app; recording it would make the whole map about the
      // foreign site. Recover (go back / re-goto) and do NOT record the state.
      if (await recoverIfOffOrigin()) {
        if (tapPix) await tapPix.stop();
        stuck++;
        current = await observe();
        continue;
      }
      await finishScreencastCapture(tapPix, before, 'tap:' + sel);
      // JANK/HANG watchdog: did this action block the main thread past the
      // jank/hang floor? Keyed by (from, action) like the flicker oracle, so the
      // Rust side attributes it to this transition and `check` re-confirms it.
      const tapJank = await drainJankForEngine(page);
      // Deterministic layout-thrash jank (machine-invariant forced-layout count).
      // Preferred over the wall-clock jank bucket when it fires: the count
      // reproduces on any runner, so `check` re-confirms it without depending on
      // machine speed. A HANG (freeze) still reports from the timing watchdog (a 2s
      // freeze is robust and may be pure-compute with no layouts).
      const tapThrash = layoutThrash(perfBefore, perfAfterTap);
      if (tapThrash && (!tapJank || tapJank.kind !== 'hang')) {
        log(
          'EXPLORE:JANK ' +
            JSON.stringify({
              from: before,
              action: 'tap:' + sel,
              bucket: tapThrash.count,
              unit: 'layouts',
              count: tapThrash.count,
            }),
        );
      } else if (tapJank) {
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
      if (recording) {
        lastTriggerLabel =
          tapJank || tapThrash ? (tapJank && tapJank.kind === 'hang' ? 'froze' : 'jank') : null;
        lastFlickerKeys = tapChurn && tapChurn.length ? tapChurn : null;
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
        rememberEdge(graph, before, 'tap:' + sel, next.sig);
        stuck = 0;
        // ZOOM-REFLOW: this tap navigated to a route not yet zoom-tested; run the
        // 200% zoom re-render BEFORE the metamorphic reload below (the check
        // restores the viewport, so the reload still sees the pinned size). Never
        // in replay (a recorded clip must not jump viewports) or probe mode.
        if (!replay && !PROBE && next.anchor && !zoomChecked.has(next.anchor)) {
          zoomChecked.add(next.anchor);
          await zoomReflowCheck(next.sig, next.anchor);
        }
        // LISTENER-LEAK (opt-in): this tap navigated to a new route with a real
        // history entry (the anchor CHANGED). Probe it for a revisit leak via the
        // back/forward loop. Once per route; guarded off in replay/probe mode like
        // the other route checks.
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
        // Layer-1 effect detection: the tap changed displayed content (a calculator
        // keypress on a capped display) without a structural move. EFFECTIVE, so
        // reset stuck and keep driving; no self-edge is recorded.
        stuck = 0;
      }
      current = next;
    }

    if (coverageMode && actions >= budget && hasFrontier(actionsByState, triedEdges)) {
      log(
        'EXPLORE:TRUNCATED ' +
          JSON.stringify({
            reason: 'action-budget',
            budget,
            states: actionsByState.size,
          }),
      );
    }

    // LEAK sampler: a final heap sample after the last action, so the series
    // spans the whole soak (start ... last action). No-op outside replay.
    if (replay) await sampleHeap(page, gtCdp, Date.now() - t0);
    // FINDING HIGHLIGHT: on a recorded replay, draw a red box around what broke
    // on this final state and hold it so the clip ends on the bug itself - the
    // visual companion to the action HUD. State oracles (overflow/content) are
    // re-detected inside; crash/jank/hang/flicker come from the latest action's
    // captured signals (crash overrides a jank label on the same action). Replay+
    // record only, so a normal fuzz hunt is untouched.
    if (recording) {
      if (fuzz.highlight && fuzz.highlight.includes('choice')) {
        // CHOICE-ANOMALY clip: a CALM, minimal reproduction. The scan already named
        // the outlier (fuzz.choiceOutlier) and confirmed the anomaly, so the clip
        // does NOT re-run the differential (clicking every option + an A/B re-toggle
        // on camera made an unwatchable, jumpy clip). It just: find the outlier
        // option, bring it into view ONLY if it is off-screen (a slow scroll to the
        // one control the action touches -- never a full-page scroll-through), select
        // it once so the page visibly shifts, and box it. If the host did not pass an
        // outlier (older map), fall back to one in-page detection pass to name it.
        let drew = false;
        try {
          let label = fuzz.choiceOutlier || null;
          let mag = Number(fuzz.choiceMag) || 0;
          if (!label) {
            const found = await page
              .evaluate(choiceAnomalyInPage, {
                settleMs: 600,
                ratio: CHOICE_OUTLIER_RATIO,
                minMag: CHOICE_MIN_MAGNITUDE,
                choiceRoles: CHOICE_ROLE_LIST,
              })
              .catch(() => []);
            const top = (found || []).sort((a, b) => (b.magnitude || 0) - (a.magnitude || 0))[0];
            if (top && top.outlier) {
              label = top.outlier;
              mag = top.magnitude || 0;
            }
          }
          if (label) {
            const replayed = await page
              .evaluate(replayChoiceComponentInPage, {
                label,
                settleMs: 450,
              })
              .catch(() => ({ ok: false, choices: [] }));
            if (replayed && replayed.ok) {
              await page.waitForTimeout(800); // let the page settle into the shifted layout
              await drawFindingBoxes(page, {
                triggerLabel: mag ? 'layout shift +' + Math.round(mag) + 'px' : 'layout shift',
                oracle: 'no-choice-anomaly',
              }).catch(() => {});
              await page.waitForTimeout(2000); // hold the boxed shift for a beat
              drew = true;
            }
          }
        } catch (_) {
          /* ignore */
        }
        log('FINDING:BOXED ' + JSON.stringify({ oracle: fuzz.highlight, drew }));
      } else if (/hang|jank|exception|crash/.test(String(fuzz.highlight || ''))) {
        // SEQUENCE-BUG clip (hang/crash/jank): the trigger was already boxed RED
        // PRE-tap (while the page was live), so we do NOT draw on the now-frozen/
        // broken page -- that is what made the clip wait ~80s for the freeze to
        // release. The trust gate is whether the bug ACTUALLY RE-FIRED on replay
        // (a real re-hang / re-crash), not whether a box drew. Faithful or dropped.
        const fired =
          lastTriggerLabel === 'froze' ||
          lastTriggerLabel === 'jank' ||
          replayErrorCount > crashAtStart;
        log('FINDING:BOXED ' + JSON.stringify({ oracle: fuzz.highlight, drew: !!fired }));
      } else if (fuzz.brokenRouteStatus) {
        // A broken document reached during the original walk has no source
        // anchor to box. Revalidate the actual navigation response and apply
        // the same SPA soft-404 guard used during discovery. This makes the
        // trust marker deterministic without substituting an unrelated link.
        const expected = Number(fuzz.brokenRouteStatus);
        const actual = startResponse ? startResponse.status() : 0;
        const view = await page
          .evaluate(soft404View)
          .catch(() => ({ controls: 0, mountFilled: false, notFound: false }));
        const fired = actual === expected && isDeadRouteStatus(actual) && !isSoftHandled(view);
        log('FINDING:BOXED ' + JSON.stringify({ oracle: fuzz.highlight, drew: fired }));
      } else {
        // STATE-PRESENT (overflow/content) + broken-route: re-detect on the live
        // page and box it (the page is not frozen here).
        await drawFindingBoxes(page, {
          triggerLabel: lastTriggerLabel,
          flickerKeys: lastFlickerKeys,
          oracle: fuzz.highlight || null,
          linkHref: fuzz.linkHref || null,
        }).catch(() => {});
      }
      await page.waitForTimeout(2200);
    }
    // BROKEN-ROUTE link check: catch a dead link the bounded action walk never
    // tapped. An 8-way GET pass also extracts links from a bounded set of
    // successful HTML pages, then probes those children without recursing. A
    // candidate is reported only after a real document navigation confirms the
    // GET 404/410 and the rendered view fails the SPA soft-404 guard.
    if (!replay) {
      const routeInspection = await inspectLinkedRoutes(page, {
        origin: APP_ORIGIN,
        seenLinks,
        navStatus,
        observe,
        log,
      });
      if (routeInspection.unverified) {
        log(
          `JOURNEY[a] step: broken-route: ${routeInspection.unverified} ` +
            'candidate link(s) not verified (capped)',
        );
      }
      const routeGaps = routeInspection.coverageGaps + routeInspection.unverified;
      if (coverageMode && routeGaps > 0) {
        log(
          'EXPLORE:TRUNCATED ' +
            JSON.stringify({
              reason: 'linked-page-cap',
              skipped: routeGaps,
              fetched: routeInspection.fetched,
            }),
        );
      }
    }
    log(`JOURNEY[a] step: explored ${seenStates.size} states`);
  }

  // Run every seed in this session in sequence. For a multi-seed batch
  // ({"batch":[...]}) wrap EACH seed's walk in SEED:BEGIN <seed> ... SEED:END
  // <seed> so the Rust side (fuzz.rs split_seed_segments) attributes coverage,
  // trace, and findings to the right seed; between seeds re-pump a fresh start
  // screen so each seed begins clean. A single-seed {"seed":..} run emits NO
  // SEED markers.
  const { seeds, isBatch } = loadBatch();
  for (let i = 0; i < seeds.length; i++) {
    const fuzz = seeds[i];
    if (isBatch) {
      if (i > 0) await resetToRoot();
      // Batch runs share this process, but capsule exchange matching and the
      // causal ids are per-run action clocks: flush any in-flight requests
      // from the previous run and restart the clock, or hermetic replay can
      // only ever match exchanges in the first run of a batch.
      flushUnresolvedCausal();
      causalActionIndex = 0;
      causalOrdinal = 0;
      if (capsuleReplayReset) capsuleReplayReset();
      log(`SEED:BEGIN ${Number(fuzz.seed || 0)}`);
    }
    await runSeed(fuzz);
    if (isBatch) log(`SEED:END ${Number(fuzz.seed || 0)}`);
  }
  if (INSPECT) {
    log('INSPECT:FINISHED');
    await inspectReplayFinished(page, INSPECT_WAIT_MS);
  }

  // Flush: a `pageerror` from the final action is delivered asynchronously, so
  // give it a beat to reach `emitError` (and the EXCEPTION block) before we tear
  // the page down. Without this, a crash on the very last replay step can race
  // the close and be lost under load.
  await page.waitForTimeout(500);
  // Requests still in flight (a crash ended the run before their responses
  // landed) become unresolved capsule exchanges so hermetic replay can match
  // and abort them instead of fail-closing on an unknown request.
  flushUnresolvedCausal();
  log('JOURNEY DONE');
  log('All tests passed');
  await context.close();
  await browser.close();
}

// Standard ESM main guard: drive the browser only when executed directly
// (`node runner.mjs`, how the orchestrator launches it), NOT when this module is
// imported. Keeps snapshot()/signatureOf importable from tests without launching
// a browser on import.
if (import.meta.url === pathToFileURL(process.argv[1] || '').href) {
  main().catch((e) => {
    log('EXCEPTION CAUGHT BY WEB RUNNER');
    log(String(e && e.stack ? e.stack : e));
    log('Some tests failed');
    process.exit(0); // evidence already emitted; orchestrator judges by markers
  });
}
