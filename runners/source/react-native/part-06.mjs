  const url = new URL(APPIUM);
  // Session creation can legitimately take minutes on a cold host: the first
  // XCUITest session builds WebDriverAgent from source (several minutes on a
  // stock CI runner). REPROIT_APPIUM_CONNECT_TIMEOUT_MS raises the webdriverio
  // client's cap without changing the local default.
  const connectMs = Number(process.env.REPROIT_APPIUM_CONNECT_TIMEOUT_MS) || 120000;
  // Arm the SDK-self-triggered app-invariant oracle. The RN/iOS/Android SDKs
  // evaluate the app's OWN registered invariants only when they detect the
  // fuzzer, then log a REPROIT_INVARIANT marker we scrape (see scrapeInvariants).
  // iOS reads REPROIT_FUZZ from the app process env, which XCUITest sets via
  // `processArguments.env`; Android has no app-env channel under UiAutomator2, so
  // we set the unprivileged `debug.reproit.fuzz` system property once the session
  // exists (below). RN (JS) reads a stable global its reproit E2E build sets,
  // since Appium cannot inject a JS global into the RN VM (documented in the RN
  // SDK README).
  // PERMISSION-WALK sweep: when REPROIT_DENY_PERMISSION names a permission, DON'T
  // auto-grant (Appium would otherwise pre-approve everything), so the app takes
  // its denied branch; the explicit denial below forces the "not allowed" state.
  const denyPermission = process.env.REPROIT_DENY_PERMISSION || '';
  const caps = { ...CAPS };
  // autoGrantPermissions belongs to UiAutomator2. Sending it to XCUITest is not
  // harmless: recent drivers warn that the capability is unrecognized while
  // negotiating an already expensive WDA session. Keep platform capabilities
  // structurally separated and let iOS receive only XCUITest options.
  if (isAndroid()) caps['appium:autoGrantPermissions'] = !denyPermission;
  const androidCausalStaged = stageAndroidCausalBeforeLaunch(caps);
  const androidPackage = caps['appium:appPackage'] || caps.appPackage;
  // A remote farm normally does not expose adb to the runner. In replay mode,
  // keep the app stopped until pushFile/setprop have installed the capsule.
  // Without an appPackage Appium cannot deterministically activate it, so fail
  // before claiming hermetic replay instead of accepting a bootstrap race.
  const delayedAndroidLaunch = isAndroid() && !!process.env.REPROIT_CAPSULE && !androidCausalStaged;
  if (delayedAndroidLaunch && !androidPackage) {
    throw new Error(
      'Hermetic Android replay on a remote device requires appium:appPackage ' +
        '(or pre-launch adb access)',
    );
  }
  if (delayedAndroidLaunch) caps['appium:autoLaunch'] = false;
  if (!isAndroid()) {
    const pa =
      caps['appium:processArguments'] && typeof caps['appium:processArguments'] === 'object'
        ? { ...caps['appium:processArguments'] }
        : {};
    let capsuleJson;
    if (process.env.REPROIT_CAPSULE) {
      try {
        capsuleJson = readFileSync(process.env.REPROIT_CAPSULE, 'utf8');
      } catch {
        /* capability gate will explain */
      }
    }
    pa.env = {
      REPROIT_FUZZ: '1',
      REPROIT_CAUSAL: '1',
      REPROIT_DEVICE: process.env.REPROIT_DEVICE || 'a',
      ...(capsuleJson ? { REPROIT_CAPSULE_JSON: capsuleJson } : {}),
      ...(pa.env || {}),
    };
    caps['appium:processArguments'] = pa;
  }
  const { remote } = await import('webdriverio');
  const driver = await remote({
    hostname: url.hostname,
    port: Number(url.port) || 4723,
    path: url.pathname && url.pathname !== '/' ? url.pathname : '/',
    capabilities: caps,
    logLevel: 'error',
    connectionRetryTimeout: connectMs,
  });
  // Android fuzz signal (see caps note): set the unprivileged debug.* prop the
  // SDK reads. Best-effort over the relaxed-security shell; a session without
  // that channel simply leaves the app-invariant oracle inert (never a false
  // positive). Set early so the SDK sees it on subsequent state settles.
  if (isAndroid()) {
    await mobileShell(driver, 'setprop', ['debug.reproit.fuzz', '1']);
    await mobileShell(driver, 'setprop', ['debug.reproit.action', '0']);
    if (process.env.REPROIT_CAPSULE) {
      try {
        const destination = '/data/local/tmp/reproit-capsule.json';
        const encoded = Buffer.from(readFileSync(process.env.REPROIT_CAPSULE)).toString('base64');
        await driver.pushFile(destination, encoded);
        await mobileShell(driver, 'chmod', ['0644', destination]);
        await mobileShell(driver, 'setprop', ['debug.reproit.capsule', destination]);
      } catch (error) {
        log(
          'REPROIT:CAPABILITIES {"http_replay":{"status":"unsupported","detail":' +
            '"could not inject Android capsule"}}',
        );
        if (delayedAndroidLaunch)
          throw new Error(`Could not inject Android replay capsule before launch: ${error}`);
      }
    } else {
      await mobileShell(driver, 'setprop', ['debug.reproit.capsule', '__reproit_none__']);
    }
    if (delayedAndroidLaunch) await driver.activateApp(String(androidPackage));
  }

  // Multi-actor scenario: this process plays one actor, pulling from the
  // conductor; the fuzz walk and its oracles do not run. The value-node
  // selectors still apply so scenario snapshots sign identically to fuzz.
  if (process.env.REPROIT_SCENARIO_BARRIER) {
    log('JOURNEY[a] step: scenario actor=' + (process.env.REPROIT_DEVICE || 'a'));
    await runScenarioActor(driver, loadValueNodes());
    await driver.deleteSession();
    return;
  }

  log('JOURNEY claimed role=a');
  await driver.pause(1500);

  // SAFE-AREA: resolve the device insets once (Android getSystemBars; iOS has no
  // driver source, so this is zeros and the safe-area scan stays silent on iOS).
  const safeAreaInsets = await readSafeAreaInsets(driver);

  // JANK CALIBRATION (device-level, once per session): the render pipeline's
  // frame-jank baseline on a representative CHEAP render (the launch + first
  // settle), plus whether this device uses a SOFTWARE GPU. The per-transition
  // jank floor is measured relative to these so a software compositor's inherent
  // frame drops on trivial transitions don't false-positive, while a real
  // main-thread stall still clears the floor. On real hardware the baseline is
  // ~0 and no software floor applies, so behavior is unchanged. Read BEFORE the
  // walk resets the gfxinfo window per action.
  const jankPkgId = androidPkg();
  const softwareRenderer = await detectSoftwareRenderer(driver);
  const jankBaselinePct = await calibrateJankBaseline(driver, jankPkgId);
  const jankFloor = jankFloorFor(jankBaselinePct, softwareRenderer);
  if (isAndroid()) {
    log(
      `JOURNEY[a] step: jank-floor=${jankFloor}` +
        (softwareRenderer ? ' (software GPU)' : '') +
        (Number.isFinite(jankBaselinePct) ? ` baseline=${jankBaselinePct}%` : ''),
    );
  }

  // PERMISSION-WALK: explicitly DENY the named permission so every screen the app
  // reaches next is on the denied branch. Android: `mobile: changePermissions`
  // (or resetPermission) sets it to denied. iOS has no reliable Appium primitive
  // to deny a specific permission post-launch, so the sweep is Android-first here
  // (Flutter's mocked platform channel covers both); a failure leaves the flag
  // off so no PERMISSIONWALK marker is ever emitted for an ungated run.
  let permissionDenied = false;
  if (denyPermission) {
    try {
      if (isAndroid()) {
        const pkg = targetAppId();
        await driver.execute('mobile: changePermissions', {
          permissions: 'all',
          appPackage: pkg,
          action: 'revoke',
        });
        permissionDenied = true;
        log(`JOURNEY[a] step: denied permission=${denyPermission}`);
      } else {
        log(
          'JOURNEY[a] step: permission-walk unsupported on iOS-via-Appium ' +
            `(use Flutter); permission=${denyPermission}`,
        );
      }
    } catch (e) {
      log(`JOURNEY[a] step: permission denial failed (${e && e.message ? e.message : e})`);
    }
  }

  // Drive every seed in this session. A multi-seed BATCH ({"batch":[...]}, written
  // by `reproit check` when gate.runs > 1 and by multi-seed fuzz) wraps each seed's
  // walk in SEED:BEGIN <seed> ... SEED:END <seed> so the Rust core (fuzz.rs
  // split_log_segments) splits the one drive log into one segment per replay;
  // between seeds re-pump a clean root so each begins identically. A single
  // {"seed":..}/{"replay":..} run emits NO SEED markers and runs exactly as before.
  const { seeds, isBatch } = loadBatch();
  let anyCrashed = false;
  for (let seedIdx = 0; seedIdx < seeds.length; seedIdx++) {
    const fuzz = seeds[seedIdx];
    if (isBatch) {
      if (seedIdx > 0) await resetToRoot(driver);
      log(`SEED:BEGIN ${Number(fuzz.seed || 0)}`);
    }

    const seenStates = new Set();
    const triedEdges = new Set();
    const actionsByState = new Map();
    const graph = new Map();
    let launchSig = null;
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

    async function observe() {
      const snap = await snapshot(driver, valueNodeSelectors, safeAreaInsets);
      snap.sig = effectiveSig(snap);
      log(
        'FUZZ:OBS ' +
          JSON.stringify({
            sig: snap.sig,
            ...(snap.anchor ? { route: snap.anchor } : {}),
            labels: snap.labels.slice(0, 24),
            elements: snap.elements.slice(0, 24).map((e) => ({ role: e.role })),
          }),
      );
      if (!seenStates.has(snap.sig)) {
        seenStates.add(snap.sig);
        // sig: CANONICAL STRUCTURAL signature (anchor + normalized Node tree),
        //      locale-invariant.
        // labels: DISPLAY-ONLY visible text (map show), never in the sig.
        // elements: structural selectors for replay; `nokey` flags a tappable with
        //           no stable id so the map layer can warn the developer.
        log(
          'EXPLORE:STATE ' +
            JSON.stringify({
              sig: snap.sig,
              // route: the foreground activity / screen anchor, so the candidate map
              // reconciles by route (the reliable join key), consistent with the web
              // and Flutter runners.
              ...(snap.anchor ? { route: snap.anchor } : {}),
              labels: snap.labels.slice(0, 24),
              elements: snap.elements.slice(0, 24).map((e) => {
                const o = { sel: e.sel, role: e.role, label: e.label };
                if (e.purpose) o.inputPurpose = e.purpose;
                if (e.bounds) o.bounds = e.bounds;
                if (e.nokey) o.nokey = true;
                return o;
              }),
              texts: (snap.texts || []).slice(0, 48),
            }),
        );
        // GRAPH 1 vs GRAPH 2: once per newly-seen state, probe the React fiber
        // tree for press handlers + exported a11y props and emit EXPLORE:GROUNDTRUTH
        // so the engine can diff the operable set against the a11y tree. Joined to
        // the native page source by the stable ids it just saw. Best-effort.
        const nativeIds = new Set(snap.elements.map((e) => e.key).filter((k) => k != null));
        await emitGroundtruth(driver, snap.sig, nativeIds, snap.nativeCandidates);
        // CONTENT-BUG for this newly-seen state, keyed by the SAME sig. Pure label
        // scan (no pixels, no timing), so it reproduces on replay; only emitted
        // when a broken-content artifact is actually rendered (clean app stays
        // silent).
        if (snap.contentBugs.length) {
          log(
            'EXPLORE:CONTENTBUG ' +
              JSON.stringify({
                sig: snap.sig,
                ...(snap.anchor ? { route: snap.anchor } : {}),
                items: snap.contentBugs,
              }),
          );
        }
        // STUCK-KEYBOARD for this newly-seen state, keyed by the SAME sig.
        // Ground truth from the driver: the IME is visible (isKeyboardShown)
        // while the active element is not an editable. iOS tags are
        // XCUIElementType roles, Android tags are widget classes; TextView only
        // counts as editable on iOS but matching it on Android merely suppresses
        // a finding (safe direction). Only emitted on a violation; any driver
        // hiccup stays silent so a flaky bridge can never mint a false positive.
        try {
          if (await driver.isKeyboardShown()) {
            let editableFocused = false;
            try {
              const active = await driver.getActiveElement();
              const elId =
                active && (active['element-6066-11e4-a52e-4f735466cecf'] || active.ELEMENT);
              if (elId) {
                const tag = String((await driver.getElementTagName(elId)) || '');
                editableFocused = new RegExp(
                  'TextField|SecureTextField|SearchField|TextView|EditText|AutoComplete|I' + 'nput',
                  'i',
                ).test(tag);
              }
            } catch (_) {
              /* no active element => nothing focused */
            }
            if (!editableFocused) {
              log(
                'EXPLORE:STUCKKEYBOARD ' +
                  JSON.stringify({ sig: snap.sig, ...(snap.anchor ? { route: snap.anchor } : {}) }),
              );
            }
          }
        } catch (_) {
          /* driver without IME introspection stays silent */
        }
        // SAFE-AREA for this newly-seen state, keyed by the SAME sig. Pure
        // inset-vs-frame geometry (Android insets from getSystemBars; iOS is
        // silent for lack of a driver source), no pixels, so it reproduces on
        // replay; only emitted when a tappable actually sits in an inset.
        if (snap.safeArea.length) {
          log(
            'EXPLORE:SAFEAREA ' +
              JSON.stringify({
                sig: snap.sig,
                ...(snap.anchor ? { route: snap.anchor } : {}),
                items: snap.safeArea,
              }),
          );
        }
        // PERMISSION-WALK: under a denial sweep, mark each newly-seen screen as
        // reached AFTER the denial. The Rust invariant fires only for a marked
        // screen that is ALSO a graph dead end. Silent outside a denial sweep.
        if (permissionDenied) {
          log(
            'EXPLORE:PERMISSIONWALK ' +
              JSON.stringify({
                sig: snap.sig,
                permission: denyPermission,
                ...(snap.anchor ? { route: snap.anchor } : {}),
              }),
          );
        }
        // BROKEN-ASSET (tofu only on native; img/font reasons stay web-only) for
        // this newly-seen state, keyed by the SAME sig. Pure label scan, so it
        // reproduces on replay; silent when every label decodes cleanly.
        if (snap.brokenAssets.length) {
          log(
            'EXPLORE:BROKENASSET ' +
              JSON.stringify({
                sig: snap.sig,
                ...(snap.anchor ? { route: snap.anchor } : {}),
                items: snap.brokenAssets,
              }),
          );
        }
        // BLANK-SCREEN (white-screen-of-death) for this newly-seen state, keyed
        // by the SAME sig. A just-launched app can expose a transiently empty
        // a11y tree (boot timing), so a blank verdict is CONFIRMED against a
        // second snapshot after a short settle: only a still-blank tree emits.
        // Any driver hiccup stays silent so a flaky bridge can never mint a
        // false positive.
        if (snap.blank.length) {
          try {
            await driver.pause(1500);
            const again = await snapshot(driver, valueNodeSelectors, safeAreaInsets);
            if (again.blank.length) {
              log(
                'EXPLORE:BLANKSCREEN ' +
                  JSON.stringify({
                    sig: snap.sig,
                    ...(snap.anchor ? { route: snap.anchor } : {}),
                    items: snap.blank,
                  }),
              );
            }
          } catch (_) {
            /* cannot confirm => never guess-and-flag */
          }
        }
      }
      // APP-INVARIANT: scrape the SDK's self-emitted markers for this state. Runs
      // every settle (not only newly seen states) so a marker logged after the
      // first observation is still caught.
      // Markers are de-duplicated per state, id, and message.
      await scrapeInvariants(driver, snap.sig, snap.anchor);
      return snap;
    }

    let current = await observe();
    // A just-launched app can expose a not-yet-populated a11y tree on the very
    // first snapshot (boot-timing dependent; observed with Settings on CI iOS
    // simulators: valid signature, zero elements). One short settle + re-observe
    // so the walk starts from the real launch state instead of an empty one.
    // Cross-platform and cheap; a same-sig retry is a no-op in observe().
    if (current.elements.length === 0) {
      await driver.pause(2000);
      current = await observe();
    }
    launchSig = current.sig;
    // BACK-TRAP: the root/home activity anchor, so a back self-loop THERE (expected:
    // back exits or no-ops on the launch screen) is never mistaken for a trap.
    const launchAnchor = current.anchor;
    let stuck = 0;
    let crashed = false;
    const prefix = fuzz.prefix || null;
    const replay = fuzz.replay || null;
    let inspectAutoContinue = false;
    const prefixLen = prefix ? prefix.length : 0;
    const mapMode = !replay && !prefix && !fuzz.seed;
    const budget = replay
      ? replay.length
      : (mapMode && !FUZZ_CONFIGURED ? Number.MAX_SAFE_INTEGER : fuzz.budget || ACTION_BUDGET) +
        prefixLen;

    // LEAK sampler: in REPLAY mode (the `--soak` tier writes {"replay":[...]}),
    // sample memory once at the start and after every action so the Rust soak oracle
    // gets a heap-vs-time series to read the slope from. Off outside replay (a plain
    // fuzz walk is not a soak). ANDROID samples retained PSS (dumpsys meminfo); iOS
    // samples the sim app's process RSS (a coarse, session-level signal resolved over
    // simctl+ps, gated hard on a unique pid). t0 anchors t_ms to walk start; iosPid
    // is the one-shot pid cache the iOS sampler resolves lazily on first use.
    const pkg = androidPkg();
    const iosPid = { pid: null };
    const sampleHeap = async (tMs) => {
      await sampleAndroidHeap(driver, pkg, tMs);
      sampleIosHeap(iosPid, tMs);
    };
    const t0 = Date.now();
    if (replay) await sampleHeap(0);

    // ROTATION / BACKGROUND-RESTORE (lifecycle-metamorphic): each distinct state
    // sig is transform-tested once. Native device lifecycle via the Appium driver.
    const rotChecked = new Set();
    const bgChecked = new Set();
    // ROTATION-stability: rotate the device to the opposite orientation, settle,
    // then rotate BACK to the original orientation and re-observe. A correct screen
    // reflows but rebuilds the SAME structure once the original orientation is
    // restored; an app that mishandles the configuration change (Android activity
    // recreation, iOS trait-collection change) and loses content/state that never
    // comes back regresses the STRUCTURAL signature (value-state excluded).
    // Round-trip identity is false-positive-free; an app that LOCKS orientation
    // makes setOrientation a no-op, so the check silently reports nothing. Guarded
    // on the pre-transform state having content; self-restoring. Returns the
    // re-observed state.
    async function rotationCheck(snap) {
      const expected = snap.structuralSig;
      let orig = null;
      try {
        orig = await driver.getOrientation();
        const other = orig === 'LANDSCAPE' ? 'PORTRAIT' : 'LANDSCAPE';
        await driver.setOrientation(other);
        await driver.pause(700);
      } catch (_) {
        orig = null;
      }
      if (orig) {
        try {
          await driver.setOrientation(orig);
          await driver.pause(700);
        } catch (_) {}
      }
      const after = await observe();
      if (snap.elements && snap.elements.length > 0 && after.structuralSig !== expected) {
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
    // BACKGROUND-RESTORE-stability: send the app to the background then bring it
    // back to the foreground (driver.background(seconds) backgrounds for N seconds
    // then auto-restores), and re-observe. A correct app returns to the SAME
    // screen with state intact; one that drops you elsewhere or loses state across
    // the lifecycle regresses the STRUCTURAL signature. No size change; guarded on
    // the pre-transform state having content. Any driver hiccup stays silent so a
    // flaky bridge can never mint a false positive. Returns the re-observed state.
    async function backgroundCheck(snap) {
      const expected = snap.structuralSig;
      let ok = false;
      try {
        await driver.background(2);
        ok = true;
      } catch (_) {
        ok = false;
      }
      if (!ok) return snap;
      await driver.pause(700);
      const after = await observe();
      if (snap.elements && snap.elements.length > 0 && after.structuralSig !== expected) {
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
    // WAKELOCK-LEAK state (Android only; see the doc block by
    // wakelocksFromDumpsysPower). wlBaseline = the app-global locks held at the
    // launch screen (never flagged); wlState threads the per-lock origin screen +
    // already-reported set through the walk so each leak fires once, attributed to
    // the screen that acquired it. Empty/no-op on iOS (documented exclusion).
    const wlBaseline = await sampleWakelocks(driver, pkg);
    let wlState = { origin: new Map(), reported: new Set() };
    // Emit an EXPLORE:WAKELOCK finding for any lock acquired on `fromSig` that is
    // still held after landing on `toSig` (a real navigation away). No-op when the
    // sets are empty (iOS / clean release), so nothing is faked off-Android.
    const checkWakelocks = async (fromSig, toSig, heldBefore) => {
      if (fromSig === toSig) return;
      const heldAfter = await sampleWakelocks(driver, pkg);
      const step = wakelockLeakStep(wlState, wlBaseline, heldBefore, heldAfter, fromSig, toSig);
      wlState = { origin: step.origin, reported: step.reported };
      if (step.leaks.length) {
        log(
          'EXPLORE:WAKELOCK ' +
            JSON.stringify({ sig: fromSig, items: step.leaks.map(wakelockItem) }),
        );
      }
    };

    // --record clip capture: film the device for the whole replay, then box the
    // finding's element once it settles (iOS simctl recordVideo / Android Appium
    // screen recording). Armed only in replay mode with a clip plan + video dir.
    const clip = replay ? armClipCapture(fuzz) : null;
    if (clip) {
      await startClipCapture(driver, clip);
      await driver.pause(400); // lead-in so the first frames exist before the tap
    }

    for (let actions = 0; actions < budget && stuck < 3; actions++) {
      // LEAK sampler: in replay mode, sample memory once per action (BEFORE acting,
      // so action k's sample reflects the heap after the previous action settled);
      // together with the start + final samples it forms the monotonic series the
      // soak slope is read from. No-op outside replay; per-platform inside sampleHeap.
      if (replay && actions > 0) await sampleHeap(Date.now() - t0);
      // LIFECYCLE-metamorphic oracles (rotation, background-restore): once per
      // distinct state, apply a native device-lifecycle transform and assert the
      // structural signature survives it. Self-restoring, so `current` is refreshed
      // to the (restored) reality; never in replay (a recorded clip must reproduce
      // the walk without extra lifecycle events).
      if (!replay) {
        if (!rotChecked.has(current.sig)) {
          rotChecked.add(current.sig);
          current = await rotationCheck(current);
        }
        if (!bgChecked.has(current.sig)) {
          bgChecked.add(current.sig);
          current = await backgroundCheck(current);
        }
      }
      let act;
      if (replay) act = replay[actions];
      else if (prefix && actions < prefixLen) act = prefix[actions];
      else if (fuzz.seed) {
        // Inverse-visit-count weighted pick over STRUCTURAL selectors, plus 'back'.
        // Seeded + deterministic, so replays reproduce exactly. Candidates are
        // addressed by selector (key, else role+index), never by visible text.
        const sels = current.elements.map((e) => e.sel).sort();
        const ew = (fuzz.edgeWeights && fuzz.edgeWeights[current.sig]) || {};
        const options = sels.map((s) => 'tap:' + s).concat(['back']);
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
        const actions = current.elements
          .map((el) => 'tap:' + el.sel)
          .sort()
          .concat(['back']);
        rememberActions(actionsByState, current.sig, actions);
        act = firstUntriedAction(actionsByState, triedEdges, current.sig);
        if (!act) {
          const path = pathToFrontier(graph, actionsByState, triedEdges, current.sig);
          act = path && path.length ? path[0] : null;
        }
        if (!act && hasFrontier(actionsByState, triedEdges) && current.sig !== launchSig) break;
        if (!act) break;
      }

      if (replay && !inspectAutoContinue && process.env.REPROIT_INSPECT === '1') {
        const target = current.elements.find((element) => `tap:${element.sel}` === act);
        const decision = await inspectPlatformStep({
          action: act,
          step: actions + 1,
          total: replay.length,
          target: target?.label || target?.sel || null,
        });
        inspectAutoContinue = decision === 'continue';
      }
      log('FUZZ:ACT ' + act);
      await advanceCausalAction(driver);
      if (act === 'back') {
        const before = current.sig;
        const beforeAnchor = current.anchor;
        triedEdges.add(edgeKey(before, 'back'));
        const beforeContent = current.content;
        // WAKELOCK: the locks held ON this screen, sampled just before leaving it.
        const wlBefore = await sampleWakelocks(driver, pkg);
        const tHang0 = Date.now();
        try {
          await driver.back();
        } catch {
          /* ignore */
        }
        await driver.pause(700);
        // HANG watchdog on the back transition (same floor + keying as the tap path).
        const hb = hangBucket(Date.now() - tHang0 - 700);
        if (hb != null) {
          const confirmed = await androidAnrSeen(driver, pkg);
          log(
            'EXPLORE:HANG ' +
              JSON.stringify({
                from: before,
                action: 'back',
                bucket: hb,
                ...(confirmed ? { anr: true } : {}),
              }),
          );
        }
        let next = await observe();
        // BACK-TRAP (Android, narrow): the back press left the structural signature
        // AND the content fingerprint unchanged -- a pure self-loop, i.e. back was
        // SWALLOWED (a dialog/sheet dismissal would move one of them). On a NON-root
        // activity that is a trapped screen. This is the FP-safe, runner-observed
        // slice of the removed general dead-end oracle; it never fires on the
        // launch/home activity (back is expected to exit there) and requires the SAME
        // self-loop to survive ONE retry (an in-flight animation gets another frame).
        const beforeSnap = { sig: before, content: beforeContent, anchor: beforeAnchor };
        const launchSnap = { sig: launchSig, anchor: launchAnchor };
        const firstSwallowed = next.sig === before && next.content === beforeContent;
        const nonRoot = before !== launchSig && !!beforeAnchor && beforeAnchor !== launchAnchor;
        if (isAndroid() && firstSwallowed && nonRoot) {
          // Retry once for animation/transition settle, then let the pure decision
          // (isBackTrap) make the final call over before/first/retry/launch.
          try {
            await driver.back();
          } catch {
            /* ignore */
          }
          await driver.pause(700);
          const retry = await observe();
          if (isBackTrap(beforeSnap, next, retry, launchSnap)) {
            // ESCAPE: relaunch the target (terminate + activate) so the walk continues
            // from a clean root instead of ramming the trap until the stuck-counter
            // kills the walk (the audit's starvation). Reset stuck: escaping is progress.
            await resetToRoot(driver);
            current = await observe();
            stuck = 0;
            continue;
          }
          // The retry moved: it was a slow transition, not a trap. Continue with the
          // post-retry snapshot as the observed result.
          next = retry;
        }
        if (next.sig !== before) {
          log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'back', to: next.sig }));
          rememberEdge(graph, before, 'back', next.sig);
          // WAKELOCK: leaving `before` for a different screen; flag locks acquired
          // on `before` that are still held now (Android only, no-op otherwise).
          await checkWakelocks(before, next.sig, wlBefore);
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
      // --record: the tap on the finding's element is the moment to box. Grab its
      // rect + the capture-relative timestamp from THIS snapshot, before the press
      // may mutate the tree (finalize falls back to the final snapshot).
      if (clip) noteClipTap(clip, sel, current);
      triedEdges.add(edgeKey(current.sig, 'tap:' + sel));
      const before = current.sig;
      const beforeContent = current.content;
      // JANK: reset the gfxinfo framestats window so the read after this tap counts
      // only the frames this action rendered (per-transition, not run-cumulative).
      await resetGfxinfo(driver, pkg);
      // WAKELOCK: the locks held ON this screen, sampled before the tap (outside the
      // HANG timing window below so it doesn't inflate the blocked-time measure).
      const wlBefore = await sampleWakelocks(driver, pkg);
      // HANG: time the action's blocking wall-clock. We measure tap + settle only
      // (NOT the subsequent observe, which is a page-source round-trip whose latency
      // is unrelated to the app's responsiveness), so the floor reflects the app
      // freezing, not Appium overhead.
      const tHang0 = Date.now();
      const ok = await tap(driver, sel, current);
      if (!ok) {
        log('FUZZ:MISS ' + act);
        stuck++;
        continue;
      }
      await driver.pause(800);
      const blockedMs = Date.now() - tHang0 - 800; // subtract the fixed settle pause
      // Crash oracle: if the target app left the foreground after this tap, the app
      // crashed (uncaught exception -> process died -> launcher).
      if (await appCrashed(driver)) {
        emitCrash(act);
        crashed = true;
        break;
      }
      // HANG watchdog: did the action block past the freeze floor? Keyed by (from,
      // action) so the Rust side attributes it to this transition and `check`
      // re-confirms it. On Android, optionally upgrade-confirm with the ANR trace.
      const hb = hangBucket(blockedMs);
      if (hb != null) {
        const confirmed = await androidAnrSeen(driver, pkg);
        log(
          'EXPLORE:HANG ' +
            JSON.stringify({
              from: before,
              action: 'tap:' + sel,
              bucket: hb,
              ...(confirmed ? { anr: true } : {}),
            }),
        );
      }
      // JANK watchdog (Android only): did this transition render a dropped-frame
      // storm? Read the gfxinfo framestats window we reset above. Keyed by (from,
      // action) like HANG. iOS has no per-frame trace over XCUITest (documented gap).
      const jk = await drainGfxinfoJank(driver, pkg, jankFloor);
      if (jk) {
        log(
          'EXPLORE:JANK ' +
            JSON.stringify({
              from: before,
              action: 'tap:' + sel,
              bucket: jk.bucket,
              count: jk.count,
            }),
        );
      }
      const next = await observe();
      if (next.sig !== before) {
        log('EXPLORE:EDGE ' + JSON.stringify({ from: before, action: 'tap:' + sel, to: next.sig }));
        rememberEdge(graph, before, 'tap:' + sel, next.sig);
        // WAKELOCK: this tap navigated away from `before`; flag locks acquired on
        // `before` that are still held on `next` (Android only, no-op otherwise).
        await checkWakelocks(before, next.sig, wlBefore);
        stuck = 0;
      } else if (next.content !== beforeContent) {
        // Layer-1 effect detection: the tap changed displayed content (a calculator
        // keypress / counter on a capped display) without a structural move.
        // EFFECTIVE, so reset stuck and keep driving; no self-edge is recorded.
        stuck = 0;
      } else stuck++;
      current = next;
    }

    // LEAK sampler: a final sample after the last action, so the series spans the
    // whole soak (start ... last action). No-op outside replay; per-platform inside.
    if (replay) await sampleHeap(Date.now() - t0);
    // --record clip finalize: resolve the finding's element rect, write box-spec.json
    // next to clip.mov, finalize the recording, and emit FINDING:BOXED. The host
    // gates on drew + runs box-overlay.mjs to draw the box (the uniform post-capture
    // path for every backend that cannot inject a live overlay).
    if (clip) await finalizeClipCapture(driver, clip, current);
    log(`JOURNEY[a] step: explored ${seenStates.size} states`);
    if (crashed) anyCrashed = true;
    if (isBatch) log(`SEED:END ${Number(fuzz.seed || 0)}`);
  }

  log('JOURNEY DONE');
  log(anyCrashed ? 'Some tests failed' : 'All tests passed');
  await driver.deleteSession();
}

// Only auto-run when invoked directly (not when imported by the parity test).
const invokedDirectly = process.argv[1] && import.meta.url === `file://${process.argv[1]}`;
if (invokedDirectly) {
  main().catch((e) => {
    log('EXCEPTION CAUGHT BY RN RUNNER');
    log('The following error was thrown:');
    log(String(e && e.message ? e.message : e));
    log('════════');
    log('Some tests failed');
    process.exit(0);
  });
}
