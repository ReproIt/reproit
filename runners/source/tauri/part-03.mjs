  try {
    await browser.execute(INSTALL_LONGTASK_JS);
  } catch {
    /* webview not ready */
  }
}
async function drainJank(browser) {
  let tasks = [];
  try {
    tasks = await browser.execute(DRAIN_LONGTASK_JS);
  } catch {
    return null;
  }
  if (!tasks || !tasks.length) return null;
  const max = Math.max(...tasks);
  if (max >= HANG_FLOOR_MS) return { kind: 'hang', bucket: HANG_FLOOR_MS, count: tasks.length };
  if (max >= JANK_FLOOR_MS) return { kind: 'jank', bucket: JANK_FLOOR_MS, count: tasks.length };
  return null;
}

// CROSS-ENGINE jank/hang path (requestAnimationFrame frame-drop detector). COPIED
// VERBATIM from runners/web/runner.mjs (installFrameObserver / drainFrameJank /
// classifyFrameIntervals + the RAF_* constants). The Long Tasks path above is
// CHROMIUM/WebView2-ONLY; on Tauri's WebKit webview it records nothing. rAF works
// in every engine: the browser fires the callback once per would-be paint, so the
// interval between two callbacks is how long the main thread blocked between two
// frames. The classifier is deliberately conservative to stay FALSE-POSITIVE-FREE:
//   - HANG: a single interval >= HANG_FLOOR_MS (2000ms). Nothing benign blocks
//     paint for two whole seconds.
//   - JANK: EITHER a LONE long frame >= RAF_JANK_LONE_MS (a stall far above any
//     GC/scheduling blip), OR a SUSTAINED RUN of >= RAF_JANK_RUN_MIN consecutive
//     long (>= RAF_FRAME_MS) frames whose summed blocked time reaches
//     JANK_FLOOR_MS. A single GC pause is one sub-lone-floor frame -> dropped.
// The EMITTED bucket is the SAME reused JANK_FLOOR_MS / HANG_FLOOR_MS constant the
// Long Tasks path uses, so the marker is byte-identical across paths and to the
// web runner. `count` is the number of distinct stall EVENTS (runs), not raw
// frames. The floors are FP-validated on real firefox/webkit; do not retune them.
const RAF_FRAME_MS = 100; // an inter-frame interval this long is a "long frame"
const RAF_JANK_RUN_MIN = 2; // a sustained jank run needs >= this many long frames
// One frame this long is jank on its own (> GC noise, < the 600ms fixture).
const RAF_JANK_LONE_MS = 350;

// Pure classifier over a list of inter-frame intervals (ms). Deterministic: the
// SAME interval list always yields the same verdict. Byte-identical to the web
// runner's classifyFrameIntervals. Returns { kind, bucket, count } or null.
function classifyFrameIntervals(intervals) {
  if (!intervals || !intervals.length) return null;
  // A HANG is any single frame that blocked paint past the hang floor. Counted as
  // distinct events so the detail is stable.
  let hangRuns = 0;
  for (const iv of intervals) if (iv >= HANG_FLOOR_MS) hangRuns++;
  if (hangRuns > 0) return { kind: 'hang', bucket: HANG_FLOOR_MS, count: hangRuns };
  // Group consecutive long frames into runs; a run is jank if it is a LONE frame
  // past the lone floor, or a sustained run (>= RAF_JANK_RUN_MIN frames) whose
  // total blocked time reaches the jank floor. A single sub-lone-floor frame
  // (a GC blip) forms a length-1 run that meets neither test -> not jank.
  let jankRuns = 0;
  let i = 0;
  const n = intervals.length;
  while (i < n) {
    if (intervals[i] < RAF_FRAME_MS) {
      i++;
      continue;
    }
    let j = i;
    let total = 0;
    let peak = 0;
    while (j < n && intervals[j] >= RAF_FRAME_MS) {
      total += intervals[j];
      if (intervals[j] > peak) peak = intervals[j];
      j++;
    }
    const runLen = j - i;
    const lone = peak >= RAF_JANK_LONE_MS;
    const sustained = runLen >= RAF_JANK_RUN_MIN && total >= JANK_FLOOR_MS;
    if (lone || sustained) jankRuns++;
    i = j;
  }
  if (jankRuns > 0) return { kind: 'jank', bucket: JANK_FLOOR_MS, count: jankRuns };
  return null;
}

// Install the rAF frame-interval recorder inside the webview, alongside the
// longtask observer. A self-perpetuating requestAnimationFrame loop appends each
// inter-frame interval to a window-global the per-action probe drains. Idempotent
// (a navigation drops it; observe() re-installs). Cross-engine (rAF is universal),
// cheap (one timestamp per frame), side-effect-free. The buffer is capped so a
// long idle stretch cannot grow it unbounded.
const INSTALL_FRAME_JS = `
  try {
    if (!window.__reproitFrameHooked) {
      window.__reproitFrameHooked = true;
      window.__reproitFrameIntervals = [];
      let last = -1;
      const tick = (now) => {
        if (last >= 0) {
          const d = now - last;
          const buf = window.__reproitFrameIntervals;
          if (buf.length < 4096) buf.push(Math.round(d));
        }
        last = now;
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    }
  } catch (_) { /* no rAF: cross-engine jank/hang silent (never a false positive) */ }
  return true;
`;
const RESET_FRAME_JS = `try { window.__reproitFrameIntervals = []; } catch (_) {} return true;`;
const DRAIN_FRAME_JS = `
  const t = window.__reproitFrameIntervals || [];
  window.__reproitFrameIntervals = [];
  return t;
`;
async function installFrameObserver(browser) {
  try {
    await browser.execute(INSTALL_FRAME_JS);
  } catch {
    /* webview not ready */
  }
}
// Drain the rAF interval buffer and classify it. Returns the SAME shape as
// drainJank ({ kind, bucket, count }) or null. The cross-engine path.
async function drainFrameJank(browser) {
  let intervals = [];
  try {
    intervals = await browser.execute(DRAIN_FRAME_JS);
  } catch {
    return null;
  }
  return classifyFrameIntervals(intervals);
}
// Per-action jank/hang verdict, engine-agnostic. Tauri cannot tell us which
// engine backs the webview from JS, so we run the PRECISE Long Tasks path first;
// when it produced a verdict (Chromium/WebView2), we keep it unchanged. When it
// is silent (no Long Tasks API, i.e. WebKit, OR a genuinely clean Chromium
// action), we fall back to the rAF path, which is the cross-engine signal that
// closes the WebKit silence. A clean action returns null on both -> no marker.
async function drainJankForEngine(browser) {
  const lt = await drainJank(browser);
  if (lt) return lt;
  return drainFrameJank(browser);
}

export { classifyFrameIntervals };

// LEAK sampler (deterministic). `--soak` replays a reversible cycle N times and
// reads the heap slope. The web/Electron runners read the PRECISE, unrounded v8
// used-heap via CDP `Runtime.getHeapUsage` + a forced GC. Tauri is driven over
// WebDriver, which has NO CDP, so that precise path is unreachable here.
//
// PRIMARY (real, coarse, session-level): the Tauri webview is a HOST PROCESS, so we
// sample its resident set size (RSS) with a host process tool. The app's main
// process is the one whose executable IS the built binary ($REPROIT_APP); helper
// processes (WebKitWebProcess / msedgewebview2 / *Helper) have a different argv[0]
// and never match, so the read is the MAIN process's footprint, not a helper's.
// RSS is whole-process memory (native + webview heaps), so it is COARSER than the
// JS heap and attributed to the SOAK RUN, not a transition; but it is REAL and
// DETERMINISTIC: a true leak grows RSS monotonically with cycle count, and the soak
// floor (262KB/cycle) is far above sampling noise. Gated HARD: we use it only when
// the app path resolves to EXACTLY ONE host pid; any ambiguity (zero or several
// matches) -> we do NOT guess and fall through to the JS fallback below.
//
// FALLBACK (when the pid can't be cleanly resolved): `performance.memory.
// usedJSHeapSize`, the same fallback the web runner uses on firefox/webkit. That
// value is QUANTIZED by Chromium (WebView2) to a coarse bucket and ABSENT entirely
// in WebKit (WKWebView / WebKitGTK), so the slope may be too coarse to see a small
// leak, or no sample is emitted at all on WebKit ('~'). We emit MEMORY:SAMPLE when
// a number is available and stay silent otherwise; soak reads whatever it gets.
const PERF_MEMORY_JS = `
  try {
    if (performance.memory && typeof performance.memory.usedJSHeapSize === 'number') {
      return performance.memory.usedJSHeapSize;
    }
  } catch (_) {}
  return null;
`;

// Run a host process tool and return trimmed stdout, or null. Pure read; never
// throws (a missing binary / non-zero exit / spawn error yields null, so the
// sampler degrades to the JS fallback).
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

// Resolve the Tauri app's MAIN host pid from its binary path ($REPROIT_APP), or
// null. Cross-platform: macOS/Linux read `ps -axww -o pid=,comm=` and keep rows
// whose command IS the app path; Windows queries `tasklist` by image name. We
// require EXACTLY ONE matching pid (the main process); zero or several -> null, so
// a helper-process race or a second instance never yields a wrong-process read.
function resolveTauriPid(appPath) {
  if (!appPath) return null;
  const isWin = osPlatform() === 'win32';
  if (isWin) {
    // tasklist filters by image name; argv[0] path isn't exposed, so match the
    // executable's base name and require a single PID row.
    const base = appPath.split(/[\\/]/).pop() || appPath;
    const out = hostExec('tasklist', ['/FI', 'IMAGENAME eq ' + base, '/FO', 'CSV', '/NH']);
    if (out == null) return null;
    const pids = [];
    for (const line of out.split(/\r?\n/)) {
      // CSV: "name","pid","session","sess#","mem". Take the 2nd quoted field.
      const m = line.match(/^"[^"]*","(\d+)"/);
      if (m) pids.push(parseInt(m[1], 10));
    }
    if (pids.length !== 1 || !Number.isFinite(pids[0]) || pids[0] <= 0) return null;
    return pids[0];
  }
  const out = hostExec('ps', ['-axww', '-o', 'pid=,args=']);
  if (out == null) return null;
  const pids = [];
  for (const line of out.split('\n')) {
    const m = line.match(/^\s*(\d+)\s+(.*)$/);
    if (!m) continue;
    const command = m[2].trim();
    if (command === appPath || command.startsWith(appPath + ' ')) {
      pids.push(parseInt(m[1], 10));
    }
  }
  if (pids.length !== 1 || !Number.isFinite(pids[0]) || pids[0] <= 0) return null;
  return pids[0];
}

// Read a host pid's RSS as BYTES, or null. macOS/Linux: `ps -o rss=` (KB).
// Windows: `tasklist` reports "N,NNN K" memory; parse the digits as KB.
function hostRssBytes(pid) {
  if (!(pid > 0)) return null;
  if (osPlatform() === 'win32') {
    const out = hostExec('tasklist', ['/FI', 'PID eq ' + pid, '/FO', 'CSV', '/NH']);
    if (out == null) return null;
    const m = out.match(/"([\d.,]+)\s*K"/);
    if (!m) return null;
    const kb = parseInt(m[1].replace(/[.,]/g, ''), 10);
    if (!Number.isFinite(kb) || kb <= 0) return null;
    return kb * 1024;
  }
  const out = hostExec('ps', ['-o', 'rss=', '-p', String(pid)]);
  if (out == null) return null;
  const kb = parseInt(out.trim(), 10);
  if (!Number.isFinite(kb) || kb <= 0) return null;
  return kb * 1024;
}

// Sample the leak signal and emit MEMORY:SAMPLE (heap_used in BYTES). PRIMARY:
// the main webview process RSS (real, coarse, session-level), used when the pid
// resolves uniquely. FALLBACK: performance.memory.usedJSHeapSize over WebDriver.
// `pidRef` is a one-shot cache ({ pid, tried }) so the host pid is resolved once.
async function sampleHeap(browser, tMs, pidRef) {
  // PRIMARY: process RSS, gated on a uniquely resolved main-process pid.
  if (pidRef) {
    if (!pidRef.tried) {
      pidRef.tried = true;
      pidRef.pid = resolveTauriPid(APP);
    }
    if (pidRef.pid > 0) {
      const rss = hostRssBytes(pidRef.pid);
      if (rss != null) {
        log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: rss }));
        return;
      }
    }
  }
  // FALLBACK: quantized JS heap (Chromium/WebView2) or silence (WebKit '~').
  let used = null;
  try {
    used = await browser.execute(PERF_MEMORY_JS);
  } catch (_) {
    used = null;
  }
  if (used == null) return;
  log('MEMORY:SAMPLE ' + JSON.stringify({ t_ms: tMs, heap_used: used }));
}

// Exception oracle for the webview. tap() clicks an element via execute(); a
// throw inside that element's event LISTENER does not propagate back through
// click()'s return value, it surfaces as an uncaught error on the webview
// window. el.click() returning true therefore says nothing about whether the
// listener threw. So we install window-level error hooks (matching the
// Playwright web runner's page.on('pageerror')) that buffer every uncaught
// error and unhandled rejection, then drain that buffer after each action.
//
// Hooks must be re-installed after navigations (each document gets a fresh
// window), so installHooks() is idempotent and called on every observe().
const INSTALL_HOOKS_JS = `
  if (!window.__reproit_hooked) {
    window.__reproit_hooked = true;
    window.__reproit_errors = [];
    window.addEventListener('error', (ev) => {
      try {
        const e = ev.error;
        window.__reproit_errors.push({
          message: (e && e.message) || ev.message || String(e || ev),
          source: ev.filename || '',
          line: ev.lineno || 0,
          stack: (e && e.stack) ? String(e.stack) : '',
        });
      } catch (_) { /* never let the hook itself throw */ }
    });
    window.addEventListener('unhandledrejection', (ev) => {
      try {
        const r = ev.reason;
        window.__reproit_errors.push({
          message: (r && r.message) ? r.message : ('Unhandled rejection: ' + String(r)),
          source: '',
          line: 0,
          stack: (r && r.stack) ? String(r.stack) : '',
        });
      } catch (_) { /* never let the hook itself throw */ }
    });
    // We intentionally do NOT also set window.onerror: in WebKitGTK both the
    // 'error' event listener above and window.onerror fire for the same
    // uncaught error, which would emit the block twice. The 'error' event is
    // the reliable single source (same as the web runner's page.on('pageerror')).
  }
  return true;
`;

async function installHooks(browser) {
  try {
    await browser.execute(INSTALL_HOOKS_JS);
  } catch {
    /* webview not ready yet */
  }
}

// Emit the SAME exception block the web/Electron runners emit and the Rust
// oracle parses (drive.rs / fuzz.rs look for "EXCEPTION CAUGHT BY", read until
// a line of pure ═, and pull the message from after "The following ...").
function emitError(err) {
  log('EXCEPTION CAUGHT BY TAURI WEBVIEW');
  log('The following error was thrown:');
  log(String(err && err.message ? err.message : err));
  const stack = err && err.stack ? String(err.stack) : '';
  for (const line of stack.split('\n').slice(0, 8)) {
    if (line) log(line);
  }
  log('════════');
}

// Pull every buffered error out of the webview and emit one block each.
async function drainErrors(browser) {
  let errs = [];
  try {
    errs = await browser.execute(() => {
      const e = window.__reproit_errors || [];
      window.__reproit_errors = [];
      return e;
    });
  } catch {
    return;
  }
  if (Array.isArray(errs)) {
    for (const e of errs) emitError(e);
  }
}

// STRUCTURAL tap: resolve a locale-invariant selector and click it inside the
// webview. Returns true on success. Mirrors runners/web/runner.mjs's tap(). No
// visible text is ever used to locate the element.
//   key:testid:<v> -> [data-testid="v"] (or data-test-id)
//   key:id:<v>     -> #<v>
//   key:name:<v>   -> [name="v"]
//   role:<role>#<idx> -> the idx-th visible tappable of that role, document order
//
// IDENTITY is the shared resolver, byte-identical to the web and Electron
// runners. ACTIVATION is still el.click() here, and that is a STATED GAP, not a
// parity claim: el.click() dispatches an untrusted event straight at the node,
// so it cannot be intercepted by an overlay the way the web runner's real
// pointer click can. WebDriver does expose a real pointer (the W3C Actions
// API), so this is unimplemented rather than unavailable; it is left alone here
// because changing it cannot be proven without tauri-driver and the platform
// webdriver, which this change had no way to drive.
const TAP_JS = `
  const resolveStructuralTarget = ${RESOLVE_STRUCTURAL_TARGET_SRC};
  const el = resolveStructuralTarget(arguments[0]);
  if (!el) return false;
  // Stash the clicked element for the post-tap oracle probes (the focus-loss
  // guards read it in-page). A window ref only, never a DOM mutation, so the
  // signature/content/mutation oracles are untouched.
  try {
    window.__reproitLastTap = el;
    // UNCHANGED, deliberately. The web and Electron runners now OBSERVE focus on
    // the real click instead of calling el.focus() first (the synthetic focus is
    // ignored by focusLossCheck, so it bought nothing, while it parked focus on
    // the control and manufactured the \`pre === tapped\` precondition for the
    // NEXT action). The same correction belongs here, but it changes what this
    // runner reports and can only be proven against tauri-driver plus the
    // platform webdriver, which this change had no way to drive. Left alone and
    // stated rather than changed on reasoning alone.
    if (window.__reproitFocusProbe) {
      try { el.focus({ preventScroll: true }); } catch (_) {}
      window.__reproitTapFocused = document.activeElement === el;
    }
  } catch (_) {}
  el.click();
  return true;
`;

async function tap(browser, sel) {
  try {
    const ok = await browser.execute(TAP_JS, sel);
    return !!ok;
  } catch {
    return false;
  }
}

// --record clip capture (route B: host film plus box-spec)
// Tauri renders in the system webview driven over WebDriver -- there is NO CDP
// and no Playwright recordVideo sink, so (unlike Electron) we cannot let the
// driver film. We film the app WINDOW with a host screen recorder (window-only,
// never the desktop -- the hard privacy rule; same shape as the macOS AX runner
// filming its target window), resolve the finding's element to a viewport-
// relative rect in CSS-px logical space via browser.execute, and write
// box-spec.json. The host then draws the red box + caption with box-overlay.mjs
// (clip.mov + box-spec.json), the uniform post-capture path for every backend
// that cannot inject a live DOM overlay.
//
// NOTE: this path is implemented symmetrically with the Electron/macOS runners
// but is exercised only where the Tauri toolchain runs (tauri-driver + the
// platform webdriver, i.e. Linux/Windows). The one coordinate assumption is that
// the captured window frame IS the webview content area (Tauri app windows are
// typically borderless/undecorated, especially under the Xvfb Linux CI path);
// videoW/videoH are the webview's own logical size so box-overlay scales the
// rect by recordedPixels/logical (DPR-safe).

// Resolve the finding's element (SAME key:/role: grammar as TAP_JS) to a
// viewport-relative box in CSS px, scrolling it into view and letting the scroll
// settle first (so the rect matches the frames filmed after this resolves).
// Runs via executeAsync (it awaits the scroll settle). Returns
// { x, y, w, h, videoW, videoH } or null.
const RESOLVE_CLIP_BOX_JS = `
  const done = arguments[arguments.length - 1];
  const resolveStructuralTarget = ${RESOLVE_STRUCTURAL_TARGET_SRC};
  const el = resolveStructuralTarget(arguments[0]);
  if (!el) { done(null); return; }
  // Scroll INSTANTLY (not smooth): a smooth animation is still moving when we
  // measure, so the rect would diverge from the settled frame the video holds.
  try { el.scrollIntoView({ behavior: 'instant', block: 'center', inline: 'center' }); }
  catch (_) { try { el.scrollIntoView({ block: 'center', inline: 'center' }); } catch (__) {} }
  let lastY = -1, stable = 0, i = 0;
  const tick = () => {
    const y = window.scrollY;
    if (y === lastY) { stable++; } else { stable = 0; lastY = y; }
    i++;
    if (stable >= 2 || i >= 20) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) { done(null); return; }
      const vw = window.innerWidth || document.documentElement.clientWidth || 1;
      const vh = window.innerHeight || document.documentElement.clientHeight || 1;
      const ins = 4;
      const left = Math.min(Math.max(r.left - 2, ins), Math.max(ins, vw - ins - 8));
      const top = Math.min(Math.max(r.top - 2, ins), Math.max(ins, vh - ins - 8));
      const w = Math.max(8, Math.min(r.width + 4, vw - left - ins));
      const h = Math.max(8, Math.min(r.height + 4, vh - top - ins));
      done({ x: left, y: top, w, h, videoW: vw, videoH: vh });
      return;
    }
    setTimeout(tick, 50);
  };
  setTimeout(tick, 50);
`;

async function resolveClipBox(browser, sel) {
  try {
    return await browser.executeAsync(RESOLVE_CLIP_BOX_JS, sel);
  } catch {
    return null;
  }
}

// Film ONLY the app window (never the desktop, a hard privacy rule) with a host
// screen recorder, cropped to the window's geometry. Returns the running child
// process (SIGINT-flushable) or null when the window/geometry can't be resolved
// (an honest gap: we never fall back to full-screen capture). Platform-specific:
//   Linux  -> ffmpeg x11grab cropped to the window rect (xdotool/xwininfo).
//   Windows-> ffmpeg gdigrab of the single titled window.
//   macOS  -> screencapture -v -l <windowid> (window-scoped), if resolvable.
function startClipCapture(pid, outMov) {
  try {
    mkdirSync(joinPath(outMov, '..'), { recursive: true });
  } catch (_) {}
  const plat = osPlatform();
  try {
    if (plat === 'linux') {
      const disp = process.env.DISPLAY || ':0';
      // Resolve the window id + geometry from the pid (best-effort; needs
      // xdotool). Geometry lets us crop x11grab to the WINDOW, never the display.
      let wid = (hostExec('xdotool', ['search', '--pid', String(pid), '--onlyvisible']) || '')
        .trim()
        .split(/\s+/)
        .filter(Boolean)
        .pop();
      if (!wid) return null;
      const geo = hostExec('xdotool', ['getwindowgeometry', '--shell', wid]) || '';
      const g = {};
      for (const line of geo.split('\n')) {
        const m = line.match(/^(\w+)=(-?\d+)/);
        if (m) g[m[1]] = parseInt(m[2], 10);
      }
      if (!(g.WIDTH > 0 && g.HEIGHT > 0)) return null;
      // Even dimensions for yuv420p.
      const w = g.WIDTH - (g.WIDTH % 2),
        h = g.HEIGHT - (g.HEIGHT % 2);
      const proc = spawn(
        'ffmpeg',
        [
          '-hide_banner',
          '-loglevel',
          'error',
          '-y',
          '-f',
          'x11grab',
          '-framerate',
          '15',
          '-video_size',
          `${w}x${h}`,
          '-i',
          `${disp}+${g.X || 0},${g.Y || 0}`,
          '-c:v',
          'libx264',
          '-pix_fmt',
          'yuv420p',
          outMov,
        ],
        { stdio: ['pipe', 'ignore', 'ignore'] },
      );
      return proc;
    }
    if (plat === 'win32') {
      // gdigrab films exactly one titled window (never the desktop). The window
      // title is the app's, matched loosely; ffmpeg errors out cleanly if absent.
      const title = hostExec('tasklist', ['/FI', 'PID eq ' + pid, '/FO', 'CSV', '/NH', '/V']) || '';
      const m = title.match(/^"[^"]*","\d+","[^"]*","[^"]*","[^"]*","([^"]*)"/);
      const win = m && m[1] && m[1] !== 'N/A' ? m[1] : null;
      if (!win) return null;
      const proc = spawn(
        'ffmpeg',
        [
          '-hide_banner',
          '-loglevel',
          'error',
          '-y',
          '-f',
          'gdigrab',
          '-framerate',
          '15',
          '-i',
          'title=' + win,
          '-c:v',
          'libx264',
          '-pix_fmt',
          'yuv420p',
          outMov,
        ],
        { stdio: ['pipe', 'ignore', 'ignore'] },
      );
      return proc;
    }
    if (plat === 'darwin') {
      // screencapture -l needs a CGWindowID; there is no reliable pure-node way to
      // map pid->CGWindowID here, so we do NOT guess (full-screen capture is
      // forbidden). Tauri's macOS webdriver path is not a supported target anyway.
      return null;
    }
  } catch (_) {}
  return null;
}

// Stop the recorder so it flushes/closes the .mov (SIGINT == a clean Control-C
// for ffmpeg/screencapture). Waits briefly for the file to finalize.
async function stopClipCapture(proc) {
  if (!proc || proc.exitCode !== null) return;
  await new Promise((resolve) => {
    let done = false;
    const finish = () => {
      if (!done) {
        done = true;
        resolve();
      }
    };
    proc.once('exit', finish);
    // ffmpeg reads 'q' on stdin for a graceful stop; SIGINT is the fallback.
    try {
      proc.stdin && proc.stdin.writable && proc.stdin.write('q');
    } catch (_) {}
    try {
      proc.kill('SIGINT');
    } catch (_) {}
    setTimeout(finish, 4000);
  });
}

const FLICKER_PIXELS = process.env.REPROIT_FLICKER_PIXELS === '1';
const FLICKER_DIAGNOSTICS = process.env.REPROIT_FLICKER_DIAGNOSTICS === '1';

// Tauri has no compositor-frame protocol. Under the explicit pixel gate, film
// only the target app window for one action using the same privacy-preserving
// recorder as evidence clips. Unsupported hosts abstain instead of falling back
// to a desktop recording.
function startTransitionFlicker(pid) {
  if (!FLICKER_PIXELS || !pid) return null;
  let dir;
  try {
    dir = mkdtempSync(joinPath(tmpdir(), 'reproit-tauri-flicker-'));
    const mov = joinPath(dir, 'transition.mov');
    const proc = startClipCapture(pid, mov);
    if (!proc) {
      if (FLICKER_DIAGNOSTICS) log('REPROIT:FLICKER_CAPTURE unavailable');
      rmSync(dir, { recursive: true, force: true });
      return null;
    }
    if (FLICKER_DIAGNOSTICS) log('REPROIT:FLICKER_CAPTURE started');
    return { dir, mov, proc };
  } catch (_) {
    if (dir) rmSync(dir, { recursive: true, force: true });
    return null;
  }
}

async function finishTransitionFlicker(capture) {
  if (!capture) return null;
  try {
    await stopClipCapture(capture.proc);
    const result = classifyVideoFile(capture.mov);
    if (FLICKER_DIAGNOSTICS) {
      const bytes = existsSync(capture.mov) ? readFileSync(capture.mov).length : 0;
      log(`REPROIT:FLICKER_CAPTURE frames=${result?.frames || 0} bytes=${bytes}`);
    }
    return result;
  } catch (_) {
    return null;
  } finally {
    try {
      rmSync(capture.dir, { recursive: true, force: true });
    } catch (_) {}
  }
}

// Multi-actor scenario client (the conductor protocol)
// Same wire protocol as the web/electron runners, the flutter explorer and the
// tui backend: the host conductor owns identity (`GET /claim`) and ordering
// (`GET /next` + `POST /done`); this process plays ONE actor.
