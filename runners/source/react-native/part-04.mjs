async function readDeviceLog(driver) {
  const out = [];
  // Android's Appium log API can return the pre-session ring buffer on its first
  // call. Read logcat for only the current app PID so a prior process can never
  // inject stale CAPSULE/EXCHANGE markers into this run.
  if (isAndroid()) {
    try {
      const pkg =
        typeof driver.getCurrentPackage === 'function' ? await driver.getCurrentPackage() : null;
      const pidRaw = pkg ? await mobileShell(driver, 'pidof', [pkg]) : null;
      const pid = String(pidRaw || '')
        .trim()
        .split(/\s+/)[0];
      if (/^\d+$/.test(pid)) {
        const dump = await mobileShell(driver, 'logcat', ['-d', `--pid=${pid}`, '-t', '400']);
        if (dump)
          for (const line of dump.split('\n')) {
            if (line && !deviceLogEmitted.has(line)) {
              deviceLogEmitted.add(line);
              out.push(line);
            }
          }
        return out;
      }
    } catch {
      /* fall back to Appium's log stream */
    }
  }
  const type = isAndroid() ? 'logcat' : 'syslog';
  try {
    if (typeof driver.getLogs === 'function') {
      const entries = await driver.getLogs(type);
      if (Array.isArray(entries)) {
        for (const e of entries) {
          const m = e && e.message != null ? e.message : e;
          // Appium's log API is a draining stream. Do not globally suppress a
          // repeated marker here: the same invariant in a different state is a
          // distinct finding. The per-(sig,id,message) set below suppresses
          // duplicate settles of one state. Only the non-draining adb dump
          // fallback needs raw-line de-duplication.
          if (m != null) out.push(String(m));
        }
      }
    }
  } catch {
    /* fall through to the adb path on Android */
  }
  if (!out.length && isAndroid()) {
    const dump = await mobileShell(driver, 'logcat', ['-d', '-t', '400']);
    if (dump)
      for (const line of dump.split('\n'))
        if (line && !deviceLogEmitted.has(line)) {
          deviceLogEmitted.add(line);
          out.push(line);
        }
  }
  return out;
}

// Extract the marker JSON object from one log line, tolerant of a log-framing
// prefix (timestamp/tag) before the token and trailing content after the object.
// Returns the parsed object or null.
function parseInvariantMarker(line) {
  const at = line.indexOf('REPROIT_INVARIANT ');
  if (at < 0) return null;
  const braceStart = line.indexOf('{', at);
  if (braceStart < 0) return null;
  const jsonStr = line.slice(braceStart);
  try {
    return JSON.parse(jsonStr);
  } catch {
    const end = jsonStr.lastIndexOf('}');
    if (end < 0) return null;
    try {
      return JSON.parse(jsonStr.slice(0, end + 1));
    } catch {
      return null;
    }
  }
}

// SDK-side mobile relation probes measure framework-native layout objects. The
// marker is accepted only after the SDK observed two identical, settled samples.
// This makes animation, unresolved transforms, and one-frame layout intermediate
// states abstain instead of becoming findings.
export function parseRelationMarker(line) {
  const at = line.indexOf('REPROIT_RELATION ');
  if (at < 0) return null;
  const braceStart = line.indexOf('{', at);
  if (braceStart < 0) return null;
  const jsonStr = line.slice(braceStart);
  try {
    return JSON.parse(jsonStr);
  } catch {
    const end = jsonStr.lastIndexOf('}');
    if (end < 0) return null;
    try {
      return JSON.parse(jsonStr.slice(0, end + 1));
    } catch {
      return null;
    }
  }
}

function validRelationCheck(it) {
  if (!it || it.kind !== 'indicator-anchor') return null;
  const outcome = String(it.outcome || '');
  if (!['VIOLATION', 'SATISFIED', 'ABSTAIN'].includes(outcome)) return null;
  const dependentKey = String(it.dependentKey || '');
  const ownerKey = String(it.ownerKey || '');
  const containerKey = String(it.containerKey || '');
  if (!dependentKey || !ownerKey || !containerKey) return null;
  const violation = it.violation == null ? undefined : String(it.violation);
  if (
    outcome === 'VIOLATION' &&
    !['detached', 'escaped-container'].includes(violation)
  ) return null;
  return {
    kind: 'indicator-anchor',
    dependentKey,
    ownerKey,
    containerKey,
    outcome,
    ...(violation ? { violation } : {}),
  };
}

export async function scrapeRelations(driver, sig, anchor, suppliedLines = null) {
  let lines = suppliedLines;
  if (!lines) {
    try {
      lines = await readDeviceLog(driver);
    } catch {
      return;
    }
  }
  for (const line of lines) {
    const obj = parseRelationMarker(line);
    // The proof contract is fail closed: a single sample is never evidence.
    if (!obj || Number(obj.stableSamples) < 2 || !Array.isArray(obj.checks)) continue;
    const checks = obj.checks.map(validRelationCheck).filter(Boolean);
    if (!checks.length) continue;
    const outcome = checks.some((x) => x.outcome === 'VIOLATION')
      ? 'VIOLATION'
      : checks.every((x) => x.outcome === 'SATISFIED')
        ? 'SATISFIED'
        : 'ABSTAIN';
    const key = sig + '|' + JSON.stringify(checks);
    if (relationEmitted.has(key)) continue;
    relationEmitted.add(key);
    const payload = { sig, ...(anchor ? { route: anchor } : {}), outcome, checks };
    log('EXPLORE:RELATIONSTATUS ' + JSON.stringify(payload));
    if (outcome === 'VIOLATION') {
      log(
        'EXPLORE:RELATION ' +
          JSON.stringify({
            sig,
            ...(anchor ? { route: anchor } : {}),
            items: checks.filter((x) => x.outcome === 'VIOLATION'),
          }),
      );
    }
  }
}

// Scrape the device log for REPROIT_INVARIANT markers and emit an
// EXPLORE:INVARIANT line (carrying THIS state's sig) for any NEW violations.
// De-duped per sig|id|message so the same violation is not re-emitted across
// settles of the same state. Best-effort; never throws.
async function scrapeInvariants(driver, sig, anchor) {
  let lines;
  try {
    lines = await readDeviceLog(driver);
  } catch {
    return;
  }
  const fresh = [];
  await scrapeRelations(driver, sig, anchor, lines);
  for (const line of lines) {
    for (const marker of [
      'REPROIT:EXCHANGE ',
      'REPROIT:CAPABILITIES ',
      'CAPSULE:HIT ',
      'CAPSULE:MISS ',
    ]) {
      const at = line.indexOf(marker);
      if (at >= 0) log(line.slice(at));
    }
    const obj = parseInvariantMarker(line);
    const items = obj && Array.isArray(obj.items) ? obj.items : null;
    if (!items) continue;
    for (const it of items) {
      if (!it || it.id == null) continue;
      const id = String(it.id);
      const message = it.message != null ? String(it.message) : '';
      const key = sig + '|' + id + '|' + message;
      if (invariantEmitted.has(key)) continue;
      invariantEmitted.add(key);
      fresh.push({ id, message });
    }
  }
  if (fresh.length) {
    log(
      'EXPLORE:INVARIANT ' +
        JSON.stringify({ sig, ...(anchor ? { route: anchor } : {}), items: fresh }),
    );
  }
}

// JANK from Android gfxinfo framestats (deterministic, bucketed). `dumpsys
// gfxinfo <pkg>` reports a "Janky frames:" summary line: "<n> (<pct>%)". We key
// the verdict off the PERCENTAGE of janky frames crossing a coarse floor, not a
// raw frame-time, so the same render workload yields the same bucket on replay.
// A clean render stays well under the floor (0-a few %); a dropped-frame storm is
// tens of percent. Returns { bucket, count } or null. The bucket is the floor
// (the deterministic detail the marker carries), count is the janky-frame count.
const JANK_PCT_FLOOR = 30; // >= 30% janky frames over the window -> jank
const JANK_BUCKET = JANK_PCT_FLOOR; // coarse, well-separated detail for the marker
// BASELINE-RELATIVE jank (FP guard for the software compositor). The absolute
// 30% floor is right for real hardware, where a settled/trivial transition drops
// ~0 frames. But under an emulator's SOFTWARE GPU (swiftshader/angle) even a
// zero-work Activity transition drops tens of percent of frames purely from the
// software compositor, so the absolute floor false-positives on trivial screens.
// We therefore raise the effective floor by the DEVICE BASELINE (frame jank of a
// representative cheap render, measured once at launch) plus a margin. When
// a software renderer is detected, clamp it up to a near-total-drop floor so
// only a genuine main-thread stall (which drops nearly EVERY frame) still fires.
// On real hardware the baseline is ~0 and no software floor applies, so the
// behavior is unchanged (>= 30% still fires); a planted long-task jank storm sits
// near 100% and clears every floor.
const JANK_BASELINE_MARGIN = 25; // a transition must beat the device baseline by this much
const JANK_SOFTWARE_FLOOR = 80; // under a software GPU, only a near-total frame-drop storm counts
// Parse the raw "Janky frames: <n> (<pct>%)" summary from `dumpsys gfxinfo`.
// Returns { pct, count } or null. Shared by the calibration read and the
// per-transition verdict so both key off the SAME number.
function jankyPctFromGfxinfo(text) {
  if (!text) return null;
  // "Janky frames: 42 (35.00%)". Read the count and the percentage.
  const m = text.match(/Janky frames:\s*(\d+)\s*\(([\d.]+)%\)/);
  if (!m) return null;
  const count = parseInt(m[1], 10);
  const pct = parseFloat(m[2]);
  if (!Number.isFinite(pct)) return null;
  return { pct, count: Number.isFinite(count) ? count : 0 };
}
// Classify Android render jank against an effective floor. `floorPct` defaults to
// the absolute floor (real-hardware behavior); callers raise it by the device
// baseline / software-renderer clamp to stay honest under a software compositor.
// The marker still carries the fixed JANK_BUCKET so the finding id is
// deterministic across replays.
function jankFromGfxinfo(text, floorPct = JANK_PCT_FLOOR) {
  const r = jankyPctFromGfxinfo(text);
  if (!r || r.pct < floorPct) return null;
  return { bucket: JANK_BUCKET, count: r.count };
}
// The per-transition jank floor for THIS device: the absolute floor, raised over
// the measured baseline (+margin) and clamped to the software-GPU floor when a
// software renderer is present. Pure, so it is unit-tested.
function jankFloorFor(baselinePct, softwareRenderer) {
  let floor = JANK_PCT_FLOOR;
  if (Number.isFinite(baselinePct)) floor = Math.max(floor, baselinePct + JANK_BASELINE_MARGIN);
  if (softwareRenderer) floor = Math.max(floor, JANK_SOFTWARE_FLOOR);
  return floor;
}

// BACK-TRAP decision (pure, unit-tested). The NARROW, FP-safe slice of the removed
// general dead-end/sink oracle: an Android screen that SWALLOWS the system back.
// The engine-wide dead-end oracle was pulled as crawler-budget FP-prone (a budget
// -limited crawl mistook an unexhausted screen for a sink), so this deliberately
// does NOT resurrect it -- it fires only on the environment-anchored ground truth
// that the runner ITSELF performed `back` and the screen did not move.
//
// Inputs are snapshots {sig, content, anchor}: `before` = the state the back was
// pressed on, `first` = the observation right after the first press, `retry` = the
// observation after ONE retry press, `launch` = {sig, anchor} of the root/home
// screen. Returns true only when ALL hold:
//   1. NON-ROOT: `before` is neither the launch signature nor the launch activity.
//      On the home/root activity `back` is EXPECTED to be a no-op or to exit the
//      app, so a self-loop there is normal, never a trap.
//   2. FIRST press was a PURE self-loop: BOTH the structural signature AND the
//      content fingerprint are unchanged. A back that dismissed a dialog/sheet
//      moves the signature (or at least the content), so requiring both unchanged
//      excludes the legitimate "back closed an overlay" case.
//   3. RETRY press ALSO self-looped identically: a back pressed mid-transition /
//      mid-animation can read as a momentary self-loop on the first observe, so we
//      give it one more frame; only a screen still pinned after the retry is a trap.
function isBackTrap(before, first, retry, launch) {
  const nonRoot = before.sig !== launch.sig && !!before.anchor && before.anchor !== launch.anchor;
  const swallowed = (o) => o.sig === before.sig && o.content === before.content;
  return nonRoot && swallowed(first) && swallowed(retry);
}
// The software-rasterizer renderer names: a SwiftShader / Mesa-pipe pipeline
// really does drop frames on trivial transitions, so we raise the jank floor when
// one is in use. Shared by the primary (GL renderer string) and fallback (render
// property) probes below.
const SOFTWARE_RENDERER_RE = new RegExp(
  'swiftshader|llvmpipe|softpipe|softwarepipe|software rasteriz|mesa ' + 'offscreen',
  '',
);
// Whether this device renders on a SOFTWARE GPU (e.g. the emulator's SwiftShader
// pipe). Under a software compositor trivial transitions drop frames, so we raise
// the jank floor there. Best-effort: an unavailable shell channel reports hardware
// (no FP suppression, no missed real finding on a real device).
//
// The DISCRIMINATOR is the actual GL renderer NAME, not a render-driver property:
// on the Android emulator `ro.hardware.egl` is "emulation" for EVERY gpu mode,
// INCLUDING `-gpu host` (which translates GLES to the host GPU / Metal on Apple
// Silicon and is genuinely hardware-accelerated). Keying on that property misread
// a hardware host-GPU emulator as software and wrongly raised the floor to 80. The
// renderer string tells them apart: a software pipe names SwiftShader / llvmpipe,
// while the host path names a real GPU ("Apple M1 ... Metal", "Adreno", "Mali").
async function detectSoftwareRenderer(driver) {
  if (!isAndroid()) return false;
  // PRIMARY: SurfaceFlinger's "GLES:" line carries GL_RENDERER (the resolved
  // renderer name). Present on emulators and real devices alike.
  const sf = (await mobileShell(driver, 'dumpsys', ['SurfaceFlinger'])) || '';
  const gles = (sf.split('\n').find((l) => /GLES:/i.test(l)) || '').toLowerCase();
  if (SOFTWARE_RENDERER_RE.test(gles)) return true;
  if (gles) return false; // a named hardware renderer (host GPU) -> not software.
  // FALLBACK (no SurfaceFlinger GLES line): the render-driver properties, matched
  // ONLY against unambiguous software-rasterizer names. The generic "emulation" /
  // "goldfish" / "angle" tokens are deliberately NOT here: they are present under
  // `-gpu host` too and would misclassify a hardware-accelerated emulator.
  for (const prop of ['ro.hardware.egl', 'ro.hardware.gpu', 'debug.hwui.renderer']) {
    const v = ((await mobileShell(driver, 'getprop', [prop])) || '').trim().toLowerCase();
    if (SOFTWARE_RENDERER_RE.test(v)) return true;
  }
  return false;
}
// Measure the device's baseline frame jank: read the gfxinfo window accumulated
// over a representative cheap render (the launch + first settle), before the walk
// resets it per action. Returns the janky-frame percentage of that window, or
// null when unavailable. This is the "first settled idle period" calibration: it
// captures the software compositor's inherent per-frame cost on a NON-pathological
// render, which the per-transition floor is then measured relative to.
async function calibrateJankBaseline(driver, pkg) {
  if (!isAndroid() || !pkg) return null;
  const text = await mobileShell(driver, 'dumpsys', ['gfxinfo', pkg]);
  const r = jankyPctFromGfxinfo(text);
  return r ? r.pct : null;
}
// Reset the gfxinfo framestats window so the NEXT read reflects only the frames
// rendered by the action under test (otherwise jank accumulates across the run
// and every later action inherits it -> not per-transition). Best-effort.
async function resetGfxinfo(driver, pkg) {
  if (!isAndroid() || !pkg) return;
  await mobileShell(driver, 'dumpsys', ['gfxinfo', pkg, 'reset']);
}
// Read + classify the Android render jank for the action that just ran, against
// the device's effective floor (baseline + software-renderer aware). Null on
// iOS / no shell channel / clean render.
async function drainGfxinfoJank(driver, pkg, floorPct = JANK_PCT_FLOOR) {
  if (!isAndroid() || !pkg) return null;
  const text = await mobileShell(driver, 'dumpsys', ['gfxinfo', pkg]);
  return jankFromGfxinfo(text, floorPct);
}

// LEAK sample from Android meminfo (deterministic, retained PSS). `dumpsys
// meminfo <pkg>` reports a "TOTAL" / "TOTAL PSS:" line in KB; PSS is the app's
// proportional set size (retained memory), the Android equivalent of the web
// runner's post-GC v8 used-heap read. We emit it as the SAME MEMORY:SAMPLE
// marker the soak oracle reads (heap_used in BYTES, so KB*1024). A true leak
// grows monotonically with the soak cycle count; a resource-neutral cycle stays
// flat. Returns the bytes, or null when meminfo is unavailable.
function pssFromMeminfo(text) {
  if (!text) return null;
  // Newer: "TOTAL PSS:   123456 ..."; older: a "TOTAL" row whose first number is
  // the total PSS in KB. Prefer the explicit label, fall back to the TOTAL row.
  let m = text.match(/TOTAL PSS:\s*(\d+)/);
  if (!m) m = text.match(/\n\s*TOTAL\s+(\d+)/);
  if (!m) return null;
  const kb = parseInt(m[1], 10);
  if (!Number.isFinite(kb)) return null;
  return kb * 1024;
}
async function sampleAndroidHeap(driver, pkg, tMs) {
  if (!isAndroid() || !pkg) return;
  const text = await mobileShell(driver, 'dumpsys', ['meminfo', pkg]);
  const used = pssFromMeminfo(text);
  if (used == null) return;
  log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: used }));
}

// ====================================================================
//  WAKELOCK LEAK (ANDROID ONLY): a wakelock / window FLAG_KEEP_SCREEN_ON held on
//  a screen must be released when the user leaves that screen. Ground truth is
//  `dumpsys power` (the app-owned held wake locks) plus the focused window's
//  keep-screen-on flag, read LIVE while the app displays each screen. A lock
//  ACQUIRED on screen X that is STILL held after navigating to a structurally
//  different screen Y is a battery-drain leak (the CPU/screen stays awake off the
//  video/map/call screen that needed it). Sequence-dependent (a before/after
//  comparison across a navigation), so it belongs to the fuzz/soak walk, not the
//  single-screen scan crawl.
//
//  DETERMINISTIC + FP-SAFE:
//    - baseline: locks held at the LAUNCH screen are app-global; never flagged.
//    - only locks OWNED BY THE TARGET PACKAGE count (the held line names the pkg
//      in its tag or WorkSource); system/framework locks (PowerManagerService.*,
//      *:launch, *alarm*, *job*, GnssLocationProvider, ...) are ignored.
//    - each leak is attributed to the ORIGIN screen (where the lock was first
//      seen held) and reported ONCE, so a lock that legitimately persists is not
//      re-flagged on every later screen; a released lock is forgotten so a fresh
//      re-acquire is judged anew.
//    - short-lived locks released before the next screen never appear in the
//      after-sample, so they never fire.
//
//  iOS is EXCLUDED (ground-truth impossible, not effort): iOS exposes NO public
//  API to enumerate another process's held wakelocks or its
//  UIApplication.isIdleTimerDisabled state; there is no `dumpsys power`
//  equivalent and no host-readable source on the simulator. Web / desktop / TUI
//  have no wakelock concept at all. So this oracle is Android/Appium only, the
//  same shell path as the gfxinfo JANK / meminfo LEAK probes; when the
//  `mobile: shell` channel is absent every read returns an empty set and the
//  oracle degrades to silence, never a false positive.
// ====================================================================

// Wake-lock TYPES that hold the device/CPU awake (the leak-relevant ones); a
// PROXIMITY_SCREEN_OFF / DRAW lock is not a battery-drain-by-staying-awake lock,
// so it is not matched.
const WAKELOCK_TYPE_RE = new RegExp(
  '(PARTIAL_WAKE_LOCK|FULL_WAKE_LOCK|SCREEN_BRIGHT_WAKE_LOCK|SCREEN_DIM_W' + 'AKE_LOCK)',
  '',
);

// Parse the app-owned held wakelock tags from `dumpsys power`. The output has a
// "Wake Locks: size=N" block whose held entries look like
//   PARTIAL_WAKE_LOCK 'com.app:Video' ON_AFTER_RELEASE ACQ=-2s
//   (uid=10234 pid=.. ws=WorkSource{10234 com.app})
// We keep only lines that (a) name an awake-holding TYPE, (b) carry a quoted tag,
// and (c) reference the target package (in the tag or the WorkSource), so a
// system lock of the same type is excluded. Returns a Set of tag strings.
// Version-tolerant (no reliance on the block header); never throws.
export function wakelocksFromDumpsysPower(text, pkg) {
  const held = new Set();
  if (!text || !pkg) return held;
  for (const raw of String(text).split('\n')) {
    const line = raw.replace(/\r$/, '');
    if (!WAKELOCK_TYPE_RE.test(line)) continue;
    if (!line.includes(pkg)) continue; // only locks owned by the target package
    const m = line.match(/'([^']+)'/);
    if (!m) continue;
    held.add(m[1]);
  }
  return held;
}

// Parse the focused target-package window's FLAG_KEEP_SCREEN_ON from
// `dumpsys window windows`. Windows are listed as blocks headed by a
// `Window{<hash> u0 <pkg>/<activity>}` line; a video/map screen that keeps the
// display on carries KEEP_SCREEN_ON inside its block. Returns true when the
// target package's window keeps the screen on. Version-tolerant; false when
// absent/unknown. Represented downstream as a synthetic KEEP_SCREEN_ON lock so
// the leak reducer treats a stuck screen-on flag exactly like a stuck wakelock.
export function keepScreenOnFromDumpsys(text, pkg) {
  if (!text || !pkg) return false;
  let inPkgWindow = false;
  for (const raw of String(text).split('\n')) {
    const line = raw;
    if (/Window\{/.test(line)) inPkgWindow = line.includes(pkg); // entered/left a window block
    if (inPkgWindow && /KEEP_SCREEN_ON/.test(line)) return true;
  }
  return false;
}

// The reported kind for a held id: the synthetic screen-on flag vs a real lock.
export function wakelockKind(id) {
  return id === 'KEEP_SCREEN_ON' ? 'keep-screen-on' : 'wakelock';
}
// EXPLORE:WAKELOCK `items` entry for a leaked id (tag + kind), sorted upstream.
export function wakelockItem(id) {
  return { tag: id, kind: wakelockKind(id) };
}

// PURE reducer (no device): advance the wakelock-leak state across one
// transition. `state` is { origin: Map<id,sig>, reported: Set<id> }; `baseline`
// is the app-global held set (locks held at launch, never flagged); `heldBefore`
// / `heldAfter` are the held id sets sampled on X (before the action) and on Y
// (after the transition); `fromSig`/`toSig` are the transition endpoints.
// Returns { leaks: string[] (ids acquired on X still held on a DIFFERENT Y,
// sorted), origin, reported } for the next step. See the doc block above for the
// determinism + FP-safety rules this encodes.
export function wakelockLeakStep(state, baseline, heldBefore, heldAfter, fromSig, toSig) {
  const origin = new Map(state && state.origin ? state.origin : []);
  const reported = new Set(state && state.reported ? state.reported : []);
  // Record the acquisition screen for non-baseline locks currently held on X
  // (captures locks acquired mid-dwell on X, e.g. tapping play).
  for (const id of heldBefore) {
    if (baseline.has(id) || reported.has(id)) continue;
    if (!origin.has(id)) origin.set(id, fromSig);
  }
  const leaks = [];
  if (toSig !== fromSig) {
    // A released lock (gone from the after-sample) is healthy: forget it so a
    // later re-acquire is attributed + judged afresh, and it never fires.
    for (const id of [...origin.keys()]) if (!heldAfter.has(id)) origin.delete(id);
    for (const id of [...reported]) if (!heldAfter.has(id)) reported.delete(id);
    for (const id of heldAfter) {
      if (baseline.has(id) || reported.has(id)) continue;
      if (origin.get(id) === fromSig) {
        // Acquired on X, still held after leaving X -> a leak. Report once.
        leaks.push(id);
        reported.add(id);
        origin.delete(id);
      } else if (!origin.has(id)) {
        origin.set(id, toSig); // first seen on arrival at Y
      }
    }
  }
  leaks.sort();
  return { leaks, origin, reported };
}

// Sample the app's live held wakelock id set (Android only). Unions the
// app-owned `dumpsys power` wake locks with a synthetic KEEP_SCREEN_ON id when
// the focused package window holds FLAG_KEEP_SCREEN_ON. Empty set on iOS / no
// shell channel, so the leak reducer stays silent there (documented exclusion).
async function sampleWakelocks(driver, pkg) {
  if (!isAndroid() || !pkg) return new Set();
  const power = await mobileShell(driver, 'dumpsys', ['power']);
  const held = wakelocksFromDumpsysPower(power, pkg);
  const win = await mobileShell(driver, 'dumpsys', ['window', 'windows']);
  if (keepScreenOnFromDumpsys(win, pkg)) held.add('KEEP_SCREEN_ON');
  return held;
}

// ---- iOS LEAK: session-level process RSS of the booted-sim app (COARSE) -------
// The XCUITest session exposes no heap/footprint readout, but an iOS-simulator app
// is a HOST macOS process. We resolve its pid deterministically from the simulator
// and read its resident set size (footprint) with host `ps`, giving a real, coarse
// MEMORY:SAMPLE series the soak oracle reads. A true leak grows RSS monotonically
// with cycle count; the floor in soak.rs (262KB/cycle) is far above GC noise, so a
// resource-neutral cycle is not a false leak. Gated HARD: any ambiguity -> silent.

// The target app's iOS bundle identifier (the join key for the launchctl row).
function iosBundleId() {
  return CAPS['appium:bundleId'] || CAPS.bundleId || '';
}

// Run a host command (simctl / ps) and return trimmed stdout, or null. Pure read;
// never mutates the device or the app. Never throws (a missing binary, a non-zero
// exit, or any spawn error yields null, so the sampler degrades to silence).
function hostExec(cmd, args) {
  try {
    const out = execFileSync(cmd, args, {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      timeout: 5000,
    });
    return out == null ? null : String(out);
  } catch {
    return null;
  }
}

// Resolve the booted-sim app's HOST pid from `simctl spawn booted launchctl list`.
// Each running app is one row "pid status UIKitApplication:<bundleId>[token]...".
// We require EXACTLY ONE row whose UIKitApplication bundleId equals the target and
// a single numeric pid; zero or multiple matches -> null (we never guess). The pid
// is a real host pid (sim apps are host processes), readable with `ps`. iOS only.
function resolveIosAppPid() {
  if (isAndroid()) return null;
  const bundle = iosBundleId();
  if (!bundle) return null;
  const out = hostExec('xcrun', ['simctl', 'spawn', 'booted', 'launchctl', 'list']);
  if (out == null) return null;
  const pids = [];
  for (const line of out.split('\n')) {
    // Match "<pid>\t<status>\tUIKitApplication:<bundleId>[..." anchoring the
    // bundleId to a '[' so a prefix bundle (com.x vs com.x.y) never cross-matches.
    const m = line.match(/^(\d+)\s+\S+\s+UIKitApplication:([^\[]+)\[/);
    if (!m) continue;
    if (m[2] !== bundle) continue;
    pids.push(parseInt(m[1], 10));
  }
  if (pids.length !== 1 || !Number.isFinite(pids[0]) || pids[0] <= 0) return null;
  return pids[0];
}

// Read a host pid's resident set size (KB, from `ps -o rss=`) as BYTES, or null.
function hostRssBytes(pid) {
  if (!(pid > 0)) return null;
  const out = hostExec('ps', ['-o', 'rss=', '-p', String(pid)]);
  if (out == null) return null;
  const kb = parseInt(out.trim(), 10);
  if (!Number.isFinite(kb) || kb <= 0) return null;
  return kb * 1024;
}

// Sample the iOS-sim app's process RSS and emit the SAME MEMORY:SAMPLE marker the
// soak oracle reads (heap_used in BYTES). No-op on Android / when the pid cannot be
// uniquely resolved / when ps is unavailable -> stays silent, never false-positive.
// `pidRef` is a one-shot cache ({ pid }) so the pid is resolved once per soak.
function sampleIosHeap(pidRef, tMs) {
  if (isAndroid()) return;
  if (pidRef.pid == null) pidRef.pid = resolveIosAppPid();
  if (!(pidRef.pid > 0)) return;
  const used = hostRssBytes(pidRef.pid);
  if (used == null) return;
  log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: used }));
}

export {
  jankFromGfxinfo,
  jankyPctFromGfxinfo,
  jankFloorFor,
  isBackTrap,
  pssFromMeminfo,
  contentBugItems,
  contentBugReason,
  rectOfEl,
  hangBucket,
  tofuReason,
  brokenAssetItems,
  blankScreenItems,
  safeAreaItems,
  snapshot,
  loadBatch,
};
export { parseInvariantMarker, scrapeInvariants, invariantEmitted };

// ====================================================================
//  OPERABILITY / ACCESSIBILITY GROUND TRUTH (the EXPLORE:GROUNDTRUTH marker)
//
//  Appium's page source (above) is GRAPH 2: the accessibility tree, the subset
//  of the UI a screen-reader / keyboard user reaches. It is structurally blind
//  to a control that has an onPress but exposes NO a11y role/label, which is
//  exactly the WCAG operability gap reproit hunts.
//
//  GRAPH 1 (ground truth) for React Native comes from the JS side: React's
//  FIBER TREE knows every node that has a press/gesture handler
//  (onPress/onPressIn/onLongPress, Pressable, Touchable*, PanResponder,
//  Gesture.Tap) AND the a11y props the developer exported (accessible,
//  accessibilityRole, accessibilityLabel, nativeID/testID). A
//  `<TouchableOpacity onPress>` with accessible={false} / no role is operable by
//  finger but invisible to AT: a gap.
//
//  The engine rule (crates/reproit/src/model/map.rs gaps_from_groundtruth):
//    operable && (rolePresent==false) -> no_role (WCAG 4.1.2)
//  We set operable = has-press-handler, rolePresent = accessibilityRole present,
//  namePresent = accessibilityLabel present. We do NOT assert keyboardActivatable
//  / inTabOrder on RN (no hardware-keyboard tab model on a touch surface), so
//  those default true in the engine and never spuriously flag.
//
//  HOW WE READ THE FIBER (and its constraints):
//    - Needs a DEV / Hermes build that exposes the React DevTools global hook
//      `__REACT_DEVTOOLS_GLOBAL_HOOK__` (present in dev; stripped in release),
//      or an app that registered `global.__REPROIT_FIBER__`. A release build has
//      neither, so the probe is a NO-OP there (a11y-only mapping, unchanged).
//    - The JS bridge runs IN the RN JS runtime. Appium can reach it on Hermes
//      via the `mobile: executeScript` / inspector channel; the exact transport
//      is environment-specific, so emitGroundtruth tries the documented hooks
//      and degrades gracefully (logs why it could not run, never throws).
//    - The JOIN to graph 2 is by nativeID / testID: the fiber record carries the
//      node's nativeID/testID and that is the same stable id idOfEl() pulls from
//      the page source, so the runtime `key:<id>` selector lines up.
// ====================================================================

// The bridge SOURCE that runs inside the RN JS runtime. It is a self-contained
// IIFE-returning function body (no closure over runner state) so it can be
// stringified and injected over whatever JS channel the build exposes. It walks
// every mounted fiber root and returns a flat array of records:
//   { id, hasPress, role, label, accessible }
// id = nativeID || testID (the join key; null if neither). hasPress = any press/
// gesture handler prop present. role/label = accessibilityRole/accessibilityLabel
// (null when absent). Pure read; it mutates nothing in the app.
const FIBER_PROBE_SRC = `(function reproitFiberProbe() {
  var records = [];
  var PRESS_PROPS = ['onPress','onPressIn','onPressOut','onLongPress','onClick'];
  function hasPressProp(props) {
    if (!props) return false;
    for (var i = 0; i < PRESS_PROPS.length; i++) {
      if (typeof props[PRESS_PROPS[i]] === 'function') return true;
    }
    // PanResponder spreads its handlers onto props (onStartShouldSetResponder /
    // onResponderRelease); a Gesture.Tap detector exposes an onGestureEvent.
    if (typeof props.onResponderRelease === 'function') return true;
    if (typeof props.onStartShouldSetResponder === 'function') return true;
    if (typeof props.onStartShouldSetResponderCapture === 'function') return true;
    if (typeof props.onGestureEvent === 'function') return true;
    return false;
  }
  // A composite type whose NAME implies a press affordance (Pressable,
  // TouchableOpacity, TouchableHighlight, TouchableWithoutFeedback, Button).
  function pressByType(type) {
    if (!type) return false;
    var name = typeof type === 'string' ? type
      : (type.displayName || type.name || '');
    return /Pressable|Touchable|^Button$|^TouchableOpacity$/.test(name);
  }
  function recordFiber(fiber) {
    if (!fiber) return;
    var props = fiber.memoizedProps || (fiber.pendingProps) || null;
    if (props) {
      var id = props.nativeID != null ? props.nativeID
        : (props.testID != null ? props.testID : null);
      var hasPress = hasPressProp(props) || pressByType(fiber.type);
      // Only emit a record for a node that is either operable OR carries a
      // join id, so the host side has something to reason about. A bare layout
      // View with neither is noise.
      if (hasPress || id != null) {
        records.push({
          id: id != null ? String(id) : null,
          hasPress: !!hasPress,
          role: props.accessibilityRole != null ? String(props.accessibilityRole) : null,
          label: props.accessibilityLabel != null ? String(props.accessibilityLabel) : null,
          accessible: props.accessible === undefined ? null : !!props.accessible,
        });
      }
    }
    // Depth-first over the fiber child/sibling links.
    var child = fiber.child;
    while (child) { recordFiber(child); child = child.sibling; }
  }
  try {
    var hook = (typeof global !== 'undefined' && global.__REACT_DEVTOOLS_GLOBAL_HOOK__) ||
      (typeof window !== 'undefined' && window.__REACT_DEVTOOLS_GLOBAL_HOOK__) || null;
    // App-registered explicit hook wins (a build can export its fiber roots).
    var explicit = (typeof global !== 'undefined' && global.__REPROIT_FIBER__) || null;
    if (explicit && typeof explicit.getRoots === 'function') {
      var roots = explicit.getRoots() || [];
      for (var r = 0; r < roots.length; r++) recordFiber(roots[r]);
    } else if (hook && hook.renderers) {
      // DevTools hook: getFiberRoots(rendererId) -> Set of FiberRoot, each with
      // a .current pointer to the root fiber.
      var ids = [];
      try { hook.renderers.forEach(function (_v, k) { ids.push(k); }); } catch (e) {}
      for (var j = 0; j < ids.length; j++) {
        var set = hook.getFiberRoots ? hook.getFiberRoots(ids[j]) : null;
        if (!set) continue;
        set.forEach(function (root) { if (root && root.current) recordFiber(root.current); });
      }
    } else {
      return { ok: false, reason: 'no-fiber-hook', records: [] };
    }
  } catch (e) {
    return { ok: false, reason: String(e && e.message ? e.message : e), records: [] };
  }
  return { ok: true, records: records };
})()`;

// HOST-SIDE pure reducer: turn the raw fiber records the bridge returned into
// the EXPLORE:GROUNDTRUTH `elements` list. Pure + deterministic (sorted by id),
// so it is unit-testable in Node WITHOUT a device. The engine rule only consults
// `operable` + `a11y.{rolePresent,namePresent,...}`:
//   operable      = the fiber node has a press/gesture handler.
//   rolePresent   = accessibilityRole was set (else AT sees a generic node).
//   namePresent   = accessibilityLabel was set.
// We DON'T claim keyboardActivatable / inTabOrder (no keyboard tab model on a
// touch surface), so the engine defaults them true and never false-flags those.
// `nativeIds` is the set of stable ids present in the native page source; when an
// operable fiber node's id is NOT among them, the native a11y tree never exposed
// it at all -> rolePresent=false (the strongest no-role signal: invisible to AT).
function groundtruthFromFiber(records, nativeIds) {
