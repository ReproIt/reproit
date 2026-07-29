async function snapshot(driver, valueNodeSelectors, insets) {
  const xml = await driver.getPageSource();
  const xmlRoot = parseXml(xml);
  let activity = null;
  try {
    if (typeof driver.getCurrentActivity === 'function')
      activity = await driver.getCurrentActivity();
  } catch {
    /* iOS / unsupported: anchor stays best-effort */
  }
  const out = {
    labels: [],
    elements: [],
    texts: [],
    seenLabel: new Set(),
    perRole: {},
    // roleSeen: document-order count of elements per canonical role, used to
    // resolve a Layer-3 role:<role>#<idx> value-node selector.
    roleSeen: {},
    // textNodes: (stable-key, raw text) pairs feeding the Layer-1 content
    // fingerprint. Carries localized text; NEVER folded into the canonical key.
    textNodes: [],
    valueNodeSelectors: valueNodeSelectors || [],
    // nativeCandidates: pointer-operable, id-bearing elements from the native
    // a11y tree, with whether each exposes a real AT role/name. Feeds the
    // native-fallback groundtruth when the JS fiber probe is unavailable.
    nativeCandidates: [],
    // contentBugs / brokenAssets / tapRects: oracle findings
    // accumulated during the tree walk (raw tuples; reduced + sorted below).
    // walkSeq numbers every element's DFS enter/exit so tapRects carry the
    // intervals the safe-area reducer's ancestor exclusion needs. screenRect: the
    // application/window frame used by blank-screen and safe-area checks.
    contentBugs: [],
    brokenAssets: [],
    tapRects: [],
    walkSeq: 0,
    screenRect: null,
  };
  // The top application/window element's frame is the blank-screen scan's
  // non-zero-window guard. Both drivers wrap the page
  // source in a geometry-less root (iOS `AppiumAUT`, Android `hierarchy`), so
  // walk down the first-child spine to the first element that exposes a frame
  // (the application/window element).
  out.screenRect = (() => {
    let el = xmlRoot.children[0];
    while (el) {
      const r = rectOfEl((n) => (el.attrs[n] != null ? el.attrs[n] : ''));
      if (r) return r;
      el = el.children[0];
    }
    return null;
  })();
  // The canonical root is a single `screen` node; the parsed app subtree hangs
  // under it (parallels the SDKs forcing the root role to "screen"). parentRect
  // starts null at the app root (the screen frame is the VIEWPORT reference, not
  // a SPILL container, so the topmost app element never self-spills).
  const screen = { role: 'screen', children: buildNodes(xmlRoot, out, null) };
  const reconciled = reconcileComposeControls(out.elements, out.nativeCandidates);
  out.elements = reconciled.elements;
  out.nativeCandidates = reconciled.nativeCandidates;
  const anchor = anchorFrom(xmlRoot, activity);
  const sig = signatureOf(anchor, screen);
  // Structural-only signature (no V: section): the per-node key the Layer-1 cap
  // tracks. Computed by hashing the descriptor with the value-class suffix
  // stripped, so it is the exact pre-value-state signature of this structure.
  const full = descriptorOf(anchor, screen);
  const vAt = full.indexOf('\nV:');
  const vsection = vAt >= 0 ? full.slice(vAt + 3) : '';
  const structuralSig = vAt >= 0 ? fnv1a(full.slice(0, vAt)) : sig;
  // Layer-1 content fingerprint (runner-local, ephemeral): structural sig plus
  // the sorted (stable-key, trimmed raw text) list. An action is EFFECTIVE iff
  // the structural sig OR this fingerprint changed (see observe/effect checks).
  out.textNodes.sort((a, b) =>
    a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0,
  );
  const content = sig + '|' + out.textNodes.map((p) => p[0] + '=' + p[1]).join(';');
  return {
    sig,
    structuralSig,
    vsection,
    content,
    anchor,
    labels: [...new Set(out.labels)],
    elements: out.elements,
    texts: out.texts.slice(0, 48),
    nativeCandidates: out.nativeCandidates,
    // Reduced + sorted oracle items ready for the corresponding markers.
    contentBugs: contentBugItems(out.contentBugs),
    brokenAssets: brokenAssetItems(out.brokenAssets),
    blank: blankScreenItems(out.labels, out.elements, out.roleSeen, out.screenRect),
    // SAFE-AREA: tappables whose frame intersects a device inset. `insets` is
    // resolved once per session (Android getSystemBars; iOS has no driver source,
    // so it is empty and this stays silent). Same {key,edge,by} shape the Flutter
    // explorer / the Rust parser expect.
    safeArea: safeAreaItems(out.tapRects, insets, out.screenRect),
  };
}

// Resolve the device safe-area insets once per session, in the SAME px space as
// the page-source frames. Android exposes the status/navigation bar geometry via
// getSystemBars(); the status bar is the top inset and the navigation bar the
// bottom inset (left/right stay 0: Appium exposes no landscape-cutout inset).
// iOS (XCUITest) exposes NO safe-area inset source, so this returns zeros and the
// safe-area scan stays silent on iOS-via-Appium -- the Flutter path is the iOS
// safe-area ground truth. Best-effort: any driver/parse failure yields zeros.
async function readSafeAreaInsets(driver) {
  const zero = { top: 0, bottom: 0, left: 0, right: 0 };
  try {
    if (isAndroid() && typeof driver.getSystemBars === 'function') {
      const bars = await driver.getSystemBars();
      const sb = bars && (bars.statusBar || bars.statusBars);
      const nb = bars && (bars.navigationBar || bars.navigationBars);
      return {
        top: sb && sb.visible !== false ? Number(sb.height || 0) : 0,
        bottom: nb && nb.visible !== false ? Number(nb.height || 0) : 0,
        left: 0,
        right: 0,
      };
    }
  } catch {
    /* unsupported / parse failure: no inset ground truth */
  }
  return zero;
}

// STRUCTURAL tap: resolve a canonical selector and click it. No visible text is
// used to locate the element.
//   key:<id>      -> resource-id / accessibility-id / testID / name
//   role:<role>#<idx> -> the idx-th tappable element of that role, document order
async function tap(driver, sel, snap) {
  if (sel.startsWith('key:')) {
    const id = sel.slice('key:'.length);
    const strategies = [
      `~${id}`,
      `//*[@resource-id="${id}"]`,
      `//*[contains(@resource-id,"/${id}")]`,
      `//*[@name="${id}"]`,
      `//*[@content-desc="${id}"]`,
    ];
    for (const s of strategies) {
      try {
        const el = await driver.$(s);
        if (await el.isExisting()) {
          await el.click();
          return true;
        }
      } catch {
        /* next */
      }
    }
    return false;
  }
  if (sel.startsWith('role:')) {
    // Resolve via the elements list captured in THIS snapshot (same structural
    // index basis as the signature), then click by its label/key if it has one.
    const el = (snap.elements || []).find((e) => e.sel === sel);
    if (!el) return false;
    const candidates = [];
    if (el.key)
      candidates.push(`~${el.key}`, `//*[@resource-id="${el.key}"]`, `//*[@name="${el.key}"]`);
    if (el.label)
      candidates.push(
        `~${el.label}`,
        `//*[@label="${el.label}"]`,
        `//*[@text="${el.label}"]`,
        `//*[@content-desc="${el.label}"]`,
      );
    for (const s of candidates) {
      try {
        const e = await driver.$(s);
        if (await e.isExisting()) {
          await e.click();
          return true;
        }
      } catch {
        /* next */
      }
    }
    return false;
  }
  return false;
}

// The target app's identifier, for the crash oracle.
function targetAppId() {
  return (
    CAPS['appium:appPackage'] || CAPS.appPackage || CAPS['appium:bundleId'] || CAPS.bundleId || ''
  );
}

// Emit the EXACT exception block the Rust oracle parses (drive.rs: a line
// containing "EXCEPTION CAUGHT BY" opens the block, a line of pure ═ closes it).
function emitCrash(action) {
  log('EXCEPTION CAUGHT BY RN RUNNER');
  log('The following error was thrown:');
  log('app crashed during ' + action + ' (foreground left ' + targetAppId() + ')');
  log('════════');
}

export async function confirmedAppExit(driver, target, settleMs = 250) {
  if (!target || typeof driver.queryAppState !== 'function') return false;
  try {
    const first = await driver.queryAppState(target);
    if (typeof first !== 'number' || first >= 4) return false;
    if (settleMs > 0) {
      await new Promise((resolve) => setTimeout(resolve, settleMs));
    }
    const second = await driver.queryAppState(target);
    return typeof second === 'number' && second < 4;
  } catch {
    return false;
  }
}

// Conservatively decide whether the target app has left the foreground.
async function appCrashed(driver) {
  const target = targetAppId();
  if (!target) return false;
  const wantPkg = CAPS['appium:appPackage'] || CAPS.appPackage || '';
  try {
    if (wantPkg && typeof driver.getCurrentPackage === 'function') {
      const pkg = await driver.getCurrentPackage();
      if (pkg && pkg !== wantPkg) return true;
    }
  } catch {
    /* probe unavailable; try queryAppState */
  }
  return confirmedAppExit(driver, target);
}

// Re-pump a fresh starting screen BETWEEN batch seeds so each replay begins from
// the same clean root (matching the web runner's resetToRoot contract). A prior
// seed may have navigated deep or CRASHED the app (left the foreground), so we
// terminate then relaunch the target: `noReset` keeps app data, so this is the
// cheap in-session equivalent of a cold start. Best-effort; a failure just leaves
// the next seed to start wherever the app is (still bracketed by SEED markers).
async function resetToRoot(driver) {
  const appId = isAndroid() ? androidPkg() : targetAppId();
  if (!appId) return;
  try {
    await driver.execute('mobile: terminateApp', isAndroid() ? { appId } : { bundleId: appId });
  } catch {
    try {
      if (typeof driver.terminateApp === 'function') await driver.terminateApp(appId);
    } catch {
      /* best-effort */
    }
  }
  try {
    await driver.execute('mobile: activateApp', isAndroid() ? { appId } : { bundleId: appId });
  } catch {
    try {
      if (typeof driver.activateApp === 'function') await driver.activateApp(appId);
    } catch {
      /* best-effort */
    }
  }
  await driver.pause(1200);
}

// ====================================================================
//  HANG / JANK / LEAK ORACLES (mirror the web runner's marker contract)
//
//  The Rust core parses (crates/reproit/src/model/map.rs, modes/soak.rs), shared
//  with the web/Flutter runners and NOT changed here:
//    EXPLORE:HANG  {"from","action","bucket"[,"count"]}  per-transition freeze
//    EXPLORE:JANK  {"from","action","bucket"[,"count"]}  per-transition stall
//    MEMORY:SAMPLE {"t_ms","heap_used"}                  heap-vs-time soak series
//  The marker carries the coarse BUCKET, not a raw ms / byte read, so the finding
//  id is deterministic for a fixed seed/replay even though the underlying timing
//  jitters. Floors are far from any real fixture so jitter can't flip a verdict.
//
//  PLATFORM COVERAGE:
//    HANG      both. Deterministic wall-clock watchdog around tap/back; Android
//              optionally confirms with the ANR trace ("ANR in <pkg>").
//    JANK      ANDROID ONLY, via `dumpsys gfxinfo <pkg>` framestats. iOS is a
//              DOCUMENTED GAP: no clean, non-flaky, sim-attributable per-frame
//              trace exists for an iOS-SIMULATOR app, which is the only iOS target
//              available here (real-device frame telemetry is out of scope). We do
//              NOT fake an iOS jank signal: drainGfxinfoJank returns null on iOS and
//              no iOS JANK marker is ever emitted. Unlike the iOS LEAK signal
//              (DONE(coarse): a sim app is a host process, so its RSS is a real,
//              deterministic, monotonic session-level number; see sampleIosHeap),
//              frame timing has NO equivalent host-readable source on the simulator.
//
//              FRAME-TIMING SOURCES TRIED ON THE BOOTED SIM, AND WHY EACH FAILS
//              (verified empirically against a booted iOS 26.2 sim, xctrace 26.0):
//                - `xcrun xctrace record --template 'Animation Hitches'` (the
//                  proper frame-pacing instrument): errors at record time with
//                  "Hitches is not supported on this platform." Hitches read the
//                  device render-server's hitch telemetry, which the simulator does
//                  not emit; it works only on a REAL device. -> no data at all.
//                - `xcrun xctrace record --template 'Metal System Trace' --device
//                  <simUDID> --all-processes`: records, but the export TOC shows it
//                  captured HOST macOS processes (the sim app's GPU work is fused
//                  into the host GPU via the SimMetalHost XPC service), NOT the sim
//                  app's per-frame display timing. There is no per-sim-app frame /
//                  display / vsync table to bucket, and the data is host-wide, so it
//                  is neither attributable to the app nor false-positive-free.
//                - `xctrace ... --attach <pid|name>` for the sim app: fails with
//                  "Cannot find process for provided pid" / "Cannot find process
//                  matching name": xctrace cannot target an in-simulator process,
//                  and the sim app's HOST pid (the one simctl launchctl list / the
//                  LEAK path resolves) is invisible to xctrace attach. So even the
//                  pid we CAN resolve for the leak signal does not open a frame
//                  trace.
//                - Appium `mobile: startPerfRecord` / `driver.getPerformanceData`:
//                  Android-only (no iOS frame-timing surface).
//              A session-level CA-commit / FPS capture would also be nondeterministic
//              to bucket over a host-shared sim GPU (no clean floor mapping to a
//              stable finding id), so even a coarse session-level iOS jank verdict
//              would risk false positives. We leave it silent rather than guess.
//    LEAK      BOTH, COARSELY, under --soak.
//              ANDROID: `dumpsys meminfo <pkg>` retained PSS (sampleAndroidHeap).
//              iOS: process RESIDENT SET SIZE (footprint) of the booted-sim app,
//              sampled per replay cycle (sampleIosHeap). The XCUITest session
//              exposes no heap/footprint readout (getPerformanceData is Android-
//              only; there is no `mobile: shell` on iOS), BUT a sim app is a HOST
//              macOS process, so the runner resolves its pid deterministically
//              from `simctl spawn booted launchctl list` (the single
//              `UIKitApplication:<bundleId>[...]` row) and reads RSS with host
//              `ps -o rss= -p <pid>`. This is a COARSE, SESSION-LEVEL signal
//              (whole-process RSS, not the JS heap, attributed to the soak run not
//              a transition), but it is REAL and DETERMINISTIC: a true leak grows
//              RSS monotonically with cycle count. It is gated HARD on a uniquely
//              resolved pid (exactly one matching app row + a single host pid);
//              when the bundleId is unset, the row is ambiguous, simctl/ps are
//              unavailable, or the pid does not resolve to one host process, it
//              stays SILENT (emits nothing) rather than risk a wrong-process read.
//              So iOS leak is DONE(coarse); iOS jank remains the documented gap.
//
//  The Android shell path (gfxinfo / meminfo / dumpsys / logcat) goes through the
//  Appium `mobile: shell` extension, which requires the server to run with
//  relaxed security (`appium:relaxedSecurity` / `--relaxed-security`). When that
//  channel is absent every shell read returns null and the oracle degrades to
//  silence (HANG via wall-clock still works), never a false positive.
// ====================================================================

// Whether the session targets Android (a `mobile: shell` / adb path exists for
// the gfxinfo jank + meminfo leak probes) vs iOS (no such path: documented gap).
function isAndroid() {
  const p = (
    CAPS['appium:platformName'] ||
    CAPS.platformName ||
    CAPS['appium:automationName'] ||
    CAPS.automationName ||
    ''
  ).toLowerCase();
  if (p.includes('android') || p.includes('uiautomator')) return true;
  if (p.includes('ios') || p.includes('xcuitest')) return false;
  // Fall back to the presence of an Android-only cap (appPackage/appActivity).
  return !!(
    CAPS['appium:appPackage'] ||
    CAPS.appPackage ||
    CAPS['appium:appActivity'] ||
    CAPS.appActivity
  );
}
function androidPkg() {
  return CAPS['appium:appPackage'] || CAPS.appPackage || targetAppId() || '';
}

// ====================================================================
//  --record CLIP CAPTURE (iOS simulator + Android emulator)
//
//  When REPROIT_VIDEO_DIR is set AND the fuzz config carries a clip plan
//  ({"replay":[...],"clip":{"sel","label","oracle"}}), film the DEVICE screen for
//  the whole replay and, once it settles, resolve the finding's element to a rect
//  + a time window, writing $REPROIT_VIDEO_DIR/box-spec.json next to clip.mov so
//  the host box-overlay step draws the finding box uniformly -- the same contract
//  as macOS-ax.swift (startClipCapture/stopClipCapture + finalize) and the web
//  runner's FINDING:BOXED handling. The element rect is read from the SAME snapshot
//  element list the replay tapped (bounds are page-source geometry: POINTS on iOS,
//  physical px on Android); videoW/videoH are the device's LOGICAL screen size in
//  the same units (driver.getWindowRect()), and box-overlay scales the recorded
//  pixel size against that automatically. Platform is detected from the Appium caps
//  (platformName) via isAndroid().
//
//  iOS films with `xcrun simctl io <udid> recordVideo` (a child process finalized
//  with SIGINT, exactly as Control-C would); Android films with Appium
//  start/stopRecordingScreen (base64 mp4 written to clip.mov). Both save to
//  $REPROIT_VIDEO_DIR/clip.mov.
// ====================================================================

// Arm a clip plan from the (single-seed) fuzz config: only in replay mode with a
// clip plan AND REPROIT_VIDEO_DIR set. Returns null (disarmed) otherwise.
function armClipCapture(fuzz) {
  const dir = process.env.REPROIT_VIDEO_DIR;
  const plan = fuzz && fuzz.clip;
  if (!dir || !plan || !plan.sel || !fuzz.replay) return null;
  return {
    dir,
    sel: plan.sel,
    label: plan.label || plan.oracle || 'finding',
    oracle: plan.oracle || '',
    mov: resolve(dir, 'clip.mov'),
    rect: null, // [x,y,w,h] captured at the triggering tap
    actionAt: 0, // seconds since capture start of the triggering tap
    startAt: 0,
    recording: null, // 'ios' | 'android' | null (start failed)
    proc: null, // the simctl recordVideo child (iOS)
  };
}

// The booted iOS simulator's udid: the caps' udid if pinned, else the first
// Booted device from `simctl list`, else the literal "booted" (simctl accepts it
// when exactly one sim is booted). Never throws.
function bootedUdid() {
  const capUdid = CAPS['appium:udid'] || CAPS.udid;
  if (capUdid) return capUdid;
  try {
    const out = execFileSync('xcrun', ['simctl', 'list', 'devices', 'booted', '-j'], {
      encoding: 'utf8',
    });
    const j = JSON.parse(out);
    for (const list of Object.values(j.devices || {})) {
      for (const d of list || []) {
        if (d && d.state === 'Booted' && d.udid) return d.udid;
      }
    }
  } catch {
    /* fall through to the literal */
  }
  return 'booted';
}

// Start filming. iOS: spawn `simctl io <udid> recordVideo` (finalized on SIGINT).
// Android: Appium startRecordingScreen (base64 mp4 drained at stop). Best-effort;
// a failure leaves clip.recording null so finalize still emits FINDING:BOXED.
async function startClipCapture(driver, clip) {
  try {
    mkdirSync(clip.dir, { recursive: true });
  } catch {
    /* ignore */
  }
  clip.startAt = Date.now();
  if (isAndroid()) {
    try {
      await driver.startRecordingScreen({ forceRestart: true });
      clip.recording = 'android';
    } catch {
      clip.recording = null;
    }
    return;
  }
  const udid = bootedUdid();
  try {
    rmSync(clip.mov, { force: true });
  } catch {
    /* ignore */
  }
  try {
    // --codec=h264 for broad ffmpeg/QuickTime compatibility; --force overwrites a
    // stale file. Records until it receives SIGINT (see stopClipCapture).
    clip.proc = spawn(
      'xcrun',
      ['simctl', 'io', udid, 'recordVideo', '--codec=h264', '--force', clip.mov],
      {
        stdio: 'ignore',
      },
    );
    clip.recording = 'ios';
  } catch {
    clip.recording = null;
  }
}

// Stop filming and finalize clip.mov. iOS: SIGINT the recordVideo child so it
// flushes+closes the .mov (bounded wait for exit). Android: stopRecordingScreen
// returns base64 mp4 which we write to clip.mov. Never throws.
async function stopClipCapture(driver, clip) {
  if (clip.recording === 'android') {
    try {
      const b64 = await driver.stopRecordingScreen();
      if (b64) writeFileSync(clip.mov, Buffer.from(b64, 'base64'));
    } catch {
      /* leave whatever exists */
    }
    return;
  }
  if (clip.recording === 'ios' && clip.proc) {
    try {
      clip.proc.kill('SIGINT');
    } catch {
      /* already gone */
    }
    await new Promise((res) => {
      let done = false;
      const finish = () => {
        if (!done) {
          done = true;
          res();
        }
      };
      clip.proc.on('exit', finish);
      clip.proc.on('error', finish);
      setTimeout(finish, 8000); // never hang the run on a stuck finalize
    });
  }
}

// Record the finding's element rect + tap timestamp when the replay taps the
// clip.sel control (mirrors the macOS runner grabbing clipEl at the triggering
// press). Called for every replayed tap; a no-op unless the sel matches.
function noteClipTap(clip, sel, snap) {
  if (!clip || clip.sel !== sel) return;
  const el = (snap.elements || []).find((e) => e.sel === sel);
  if (el && el.bounds) clip.rect = el.bounds; // freshest geometry at the tap
  clip.actionAt = (Date.now() - clip.startAt) / 1000;
}

// After the replay: resolve the element rect (fallback to the final snapshot),
// read the LOGICAL window size, stop+finalize the recording, write box-spec.json,
// and emit FINDING:BOXED. drew:false when the element never resolved to a rect.
async function finalizeClipCapture(driver, clip, snap) {
  if (!clip.rect) {
    const el = (snap.elements || []).find((e) => e.sel === clip.sel);
    if (el && el.bounds) clip.rect = el.bounds;
  }
  // The device's logical screen size, in the SAME units as the element rect
  // (points on iOS, physical px on Android) -- box-overlay scales the recorded
  // pixel size against this so the box lands regardless of Retina/sim scale.
  let win = null;
  try {
    win = await driver.getWindowRect();
  } catch {
    /* try size */
  }
  if (!win || !(win.width > 0)) {
    try {
      const s = await driver.getWindowSize();
      if (s) win = { width: s.width, height: s.height };
    } catch {
      /* none */
    }
  }
  await stopClipCapture(driver, clip);
  let drew = false;
  if (clip.rect && win && win.width > 0 && win.height > 0) {
    const [x, y, w, h] = clip.rect;
    const spec = {
      videoW: win.width,
      videoH: win.height,
      boxes: [
        {
          x,
          y,
          w,
          h,
          tStart: Math.max(0, (clip.actionAt || 0) - 0.3),
          tEnd: 1e9,
          label: clip.label,
          color: 'red',
        },
      ],
    };
    try {
      writeFileSync(resolve(clip.dir, 'box-spec.json'), JSON.stringify(spec));
      drew = true;
    } catch {
      drew = false;
    }
  }
  log('FINDING:BOXED ' + JSON.stringify({ oracle: clip.oracle, sel: clip.sel, drew }));
}

// HANG watchdog (deterministic, wall-clock). Wraps a tap+observe; we time the
// action with a monotonic clock and classify the BLOCKED wall time into the same
// coarse floors the web runner uses, so a slow handler that froze the UI is a
// HANG regardless of which platform it ran on. The floors are far apart so timing
// jitter never flips the verdict. We do NOT emit a JANK bucket from wall-clock
// (a sub-second stall is indistinguishable from normal Appium round-trip latency
// over a real device, so it would false-positive); wall-clock yields HANG only.
// Per-frame JANK on Android comes from gfxinfo instead (see jankFromGfxinfo).
const HANG_FLOOR_MS = 2000;
function hangBucket(ms) {
  return ms >= HANG_FLOOR_MS ? HANG_FLOOR_MS : null;
}

// Optionally CONFIRM an Android hang with the system ANR trace. `dumpsys activity
// processes` / logcat surface "ANR in <pkg>" when the watchdog killed the main
// thread; when present it upgrades a wall-clock hang from "slow" to a real freeze.
// Best-effort: a session without the shell path simply skips confirmation (the
// wall-clock floor still stands). Never throws.
async function androidAnrSeen(driver, pkg) {
  if (!isAndroid() || !pkg) return false;
  const out = await mobileShell(driver, 'dumpsys', ['activity', 'processes']);
  if (out && out.includes('ANR in ' + pkg)) return true;
  const log = await mobileShell(driver, 'logcat', ['-d', '-t', '200']);
  return !!(log && log.includes('ANR in ' + pkg));
}

// Run an adb shell command over the Appium `mobile: shell` extension (requires
// the server-side `--relaxed-security` / `appium:relaxedSecurity`). Returns the
// trimmed stdout, or null when the channel is unavailable / errored. Pure read;
// the gfxinfo/meminfo/dumpsys commands below never mutate the app. Never throws.
let mobileShellDenied = false;
async function mobileShell(driver, command, args) {
  if (mobileShellDenied) return null;
  try {
    if (typeof driver.execute !== 'function') return null;
    const r = await driver.execute('mobile: shell', { command, args: args || [] });
    if (r == null) return null;
    if (typeof r === 'string') return r;
    if (typeof r === 'object' && typeof r.stdout === 'string') return r.stdout;
    return String(r);
  } catch (error) {
    // Appium logs a rejected extension before this catch runs. Remember the
    // server policy denial so every optional oracle does not repeat the same
    // noisy, ~1.5s round-trip for the rest of the journey.
    const message = String(error && error.message ? error.message : error);
    if (message.includes('adb_shell') || message.includes('Potentially insecure feature')) {
      mobileShellDenied = true;
    }
    return null;
  }
}

// ====================================================================
//  APP-INVARIANT ORACLE (SDK-self-triggered)
//
//  The native fuzzer drives the app and cannot call the app's own predicates, so
//  the RN / iOS / Android SDKs evaluate their OWN registered invariants on each
//  settled state and, only when they detect the fuzzer (REPROIT_FUZZ env on iOS,
//  the debug.reproit.fuzz prop on Android, a stable global on RN), log a marker
//  on the platform diagnostic channel (console.log -> logcat/syslog, os_log /
//  NSLog, android.util.Log):
//      REPROIT_INVARIANT {"sig":"","items":[{"id":"...","message":"..."}]}
//  We scrape that channel every settle and map any NEW marker into the
//  EXPLORE:INVARIANT line the Rust core already parses (model/map.rs), with the
//  sig we are currently on substituted for the SDK's empty sig. This is the
//  runner half of the same contract the web runner emits directly via
//  page.evaluate; the Rust core is unchanged.
// ====================================================================

// De-dup key set (sig|id|message) so revisiting a state across settles does not
// re-emit the same violation. Module-scoped for the whole walk.
const invariantEmitted = new Set();
export const relationEmitted = new Set();
const deviceLogEmitted = new Set();

// Read new device-log lines since the last call. Prefers the Appium log API
// (getLogs('logcat'|'syslog'), which streams entries incrementally), falling back
// to an adb logcat dump on Android. Returns an array of message strings; never
// throws (a driver/platform without a readable log channel yields []).
