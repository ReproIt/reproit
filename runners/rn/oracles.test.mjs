// Validates the RN runner's pure oracle reducers (no device / Appium / emulator
// needed): the content-bug classifier, the geometry parser, the
// blank-screen / tofu scans, the gfxinfo jank parser, and the meminfo PSS
// parser. These mirror the web runner's oracle rules and feed the SAME
// EXPLORE:CONTENTBUG / EXPLORE:BLANKSCREEN /
// EXPLORE:BROKENASSET / EXPLORE:JANK / MEMORY:SAMPLE markers the Rust core
// already parses. Run: `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import {
  contentBugReason, contentBugItems, rectOfEl,
  jankFromGfxinfo, jankyPctFromGfxinfo, jankFloorFor, isBackTrap, pssFromMeminfo, hangBucket,
  tofuReason, brokenAssetItems, blankScreenItems, safeAreaItems,
  snapshot, loadBatch,
  parseInvariantMarker, scrapeInvariants, invariantEmitted,
  wakelocksFromDumpsysPower, keepScreenOnFromDumpsys, wakelockLeakStep, wakelockItem,
  confirmedAppExit,
} from './runner.mjs';
import { writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

test('crash oracle requires a sustained foreground exit', async () => {
  const driver = (states) => ({
    queryAppState: async () => states.shift(),
  });
  assert.strictEqual(await confirmedAppExit(driver([3, 4]), 'app', 0), false);
  assert.strictEqual(await confirmedAppExit(driver([3, 3]), 'app', 0), true);
  assert.strictEqual(await confirmedAppExit(driver([4]), 'app', 0), false);
  assert.strictEqual(await confirmedAppExit({}, 'app', 0), false);
});

// ---- CONTENT-BUG classifier (byte-identical rule to the web runner) ---------
test('content-bug: the ground-truth artifacts are flagged', () => {
  // Only artifacts impossible to render as legitimate copy fire.
  assert.strictEqual(contentBugReason('Hello [object Object] world'), 'object-object');
  assert.strictEqual(contentBugReason('Welcome, {{ user.name }}'), 'unrendered-template');
  assert.strictEqual(contentBugReason('Total: ${price}'), 'unrendered-template');
});

test('content-bug: bare undefined/null/NaN words are NOT flagged (FP-safe fix)', () => {
  // These occur in real copy and code samples; keying on them false-positived, so
  // the bare-word match was dropped. Only a leaked binding / object literal fires.
  assert.strictEqual(contentBugReason('Price: undefined'), null);
  assert.strictEqual(contentBugReason('Items: null'), null);
  assert.strictEqual(contentBugReason('Sum: NaN'), null);
  assert.strictEqual(contentBugReason('This is undefined behavior in C'), null);
  assert.strictEqual(contentBugReason('Ship to Null Island'), null);
});

test('content-bug: ordinary prose is NOT flagged', () => {
  assert.strictEqual(contentBugReason('Cancellation policy'), null);
  assert.strictEqual(contentBugReason('Undefined Behavior Lane'), null);
  assert.strictEqual(contentBugReason('Null Island is real'), null);
  assert.strictEqual(contentBugReason('Banana split'), null);
  assert.strictEqual(contentBugReason(''), null);
  assert.strictEqual(contentBugReason(null), null);
});

test('content-bug items: deduped, sorted, clipped, deterministic', () => {
  const raw = [
    { key: 'role:text#2', reason: 'null', text: 'Items: null' },
    { key: 'key:total', reason: 'nan', text: 'X'.repeat(120) },
    { key: 'key:total', reason: 'nan', text: 'dup' }, // same key|reason -> dropped
  ];
  const a = contentBugItems(raw);
  const b = contentBugItems(raw);
  assert.deepStrictEqual(a, b, 'deterministic');
  assert.deepStrictEqual(a.map((i) => i.key), ['key:total', 'role:text#2'], 'sorted by key');
  assert.strictEqual(a[0].text.length, 80, 'text clipped to 80');
});

// ---- OVERFLOW geometry (SPILL out of parent, VIEWPORT off-screen) -----------
test('rectOfEl: parses Android bounds and iOS x/y/w/h', () => {
  assert.deepStrictEqual(rectOfEl((n) => ({ bounds: '[10,20][110,220]' }[n] || '')), { l: 10, t: 20, r: 110, b: 220 });
  assert.deepStrictEqual(rectOfEl((n) => ({ x: '5', y: '6', width: '100', height: '50' }[n] || '')), { l: 5, t: 6, r: 105, b: 56 });
  assert.strictEqual(rectOfEl((n) => ''), null, 'no geometry -> null');
});

// ---- ANDROID JANK (gfxinfo framestats) --------------------------------------
test('jank: a janky-frame storm past the floor is flagged', () => {
  const r = jankFromGfxinfo('Total frames rendered: 120\nJanky frames: 50 (41.67%)\n');
  assert.ok(r, 'past 30% floor');
  assert.strictEqual(r.bucket, 30);
  assert.strictEqual(r.count, 50);
});

test('jank: a clean render under the floor is silent (no false positive)', () => {
  assert.strictEqual(jankFromGfxinfo('Janky frames: 2 (1.67%)'), null);
  assert.strictEqual(jankFromGfxinfo('no framestats here'), null);
  assert.strictEqual(jankFromGfxinfo(''), null);
  assert.strictEqual(jankFromGfxinfo(null), null);
});

// ---- ANDROID LEAK (meminfo PSS) ---------------------------------------------
test('leak: PSS is read in KB and emitted in bytes', () => {
  assert.strictEqual(pssFromMeminfo('App Summary\n  TOTAL PSS:   123456   TOTAL RSS: ...'), 123456 * 1024);
  assert.strictEqual(pssFromMeminfo('\n        TOTAL    98765    12000     0'), 98765 * 1024);
  assert.strictEqual(pssFromMeminfo('no total here'), null);
  assert.strictEqual(pssFromMeminfo(null), null);
});

// ---- HANG bucket ------------------------------------------------------------
test('hang: only a freeze past the 2s floor buckets (jitter cannot flip it)', () => {
  assert.strictEqual(hangBucket(2500), 2000);
  assert.strictEqual(hangBucket(1999), null);
  assert.strictEqual(hangBucket(50), null);
  assert.strictEqual(hangBucket(-10), null);
});

// ---- BLANK-SCREEN (WSOD: zero text + zero tappables in a non-zero window) ----
test('blank: an empty tree in a non-zero window fires one root record', () => {
  const items = blankScreenItems([], [], {}, { l: 0, t: 0, r: 390, b: 844 });
  assert.deepStrictEqual(items, [{ key: 'root', w: 390, h: 844 }]);
});

test('blank: ANY content suppresses (text, tappable, textfield, image)', () => {
  const screen = { l: 0, t: 0, r: 390, b: 844 };
  assert.deepStrictEqual(blankScreenItems(['Hello'], [], {}, screen), []);
  assert.deepStrictEqual(blankScreenItems([], [{ sel: 'key:go' }], {}, screen), []);
  assert.deepStrictEqual(blankScreenItems([], [], { textfield: 1 }, screen), []);
  assert.deepStrictEqual(blankScreenItems([], [], { image: 2 }, screen), []);
});

test('blank: no window geometry or a zero-size window never fires (no guess)', () => {
  assert.deepStrictEqual(blankScreenItems([], [], {}, null), []);
  assert.deepStrictEqual(blankScreenItems([], [], {}, { l: 0, t: 0, r: 0, b: 0 }), []);
});

// ---- SAFE-AREA (a tappable frame intersecting a device inset band) ----------
const SCREEN = { l: 0, t: 0, r: 390, b: 844 };
test('safe-area: a control under the top status-bar/notch inset fires', () => {
  const items = safeAreaItems(
    [{ key: 'key:done', rect: { l: 20, t: 0, r: 100, b: 30 } }], // top edge at y=0
    { top: 47, bottom: 34, left: 0, right: 0 }, // iPhone-ish notch + home indicator
    SCREEN,
  );
  // 30px into the 47px top inset -> overlap 30.
  assert.deepStrictEqual(items, [{ key: 'key:done', edge: 'top', by: 30 }]);
});

test('safe-area: a control under the bottom home-indicator inset fires', () => {
  const items = safeAreaItems(
    [{ key: 'key:next', rect: { l: 20, t: 820, r: 370, b: 844 } }], // bottom band [810,844]
    { top: 47, bottom: 34, left: 0, right: 0 },
    SCREEN,
  );
  // band top = 844-34 = 810; overlap = 844 - max(820,810) = 24.
  assert.deepStrictEqual(items, [{ key: 'key:next', edge: 'bottom', by: 24 }]);
});

test('safe-area: a control clear of every inset is silent', () => {
  const items = safeAreaItems(
    [{ key: 'key:mid', rect: { l: 40, t: 400, r: 350, b: 460 } }],
    { top: 47, bottom: 34, left: 0, right: 0 },
    SCREEN,
  );
  assert.deepStrictEqual(items, []);
});

test('safe-area: zero insets (no notch / no driver source) never fire', () => {
  const items = safeAreaItems(
    [{ key: 'key:done', rect: { l: 20, t: 0, r: 100, b: 30 } }],
    { top: 0, bottom: 0, left: 0, right: 0 },
    SCREEN,
  );
  assert.deepStrictEqual(items, []);
  // Missing insets/screenRect also stay silent (no guess-and-flag).
  assert.deepStrictEqual(safeAreaItems([{ key: 'k', rect: { l: 0, t: 0, r: 9, b: 9 } }], null, SCREEN), []);
  assert.deepStrictEqual(safeAreaItems([{ key: 'k', rect: { l: 0, t: 0, r: 9, b: 9 } }], { top: 47 }, null), []);
});

test('safe-area: a 1px flush touch is tolerated (rounding, not a collision)', () => {
  const items = safeAreaItems(
    [{ key: 'key:a', rect: { l: 0, t: 46, r: 80, b: 100 } }], // top inset 47, overlap = 47-46 = 1
    { top: 47, bottom: 0, left: 0, right: 0 },
    SCREEN,
  );
  assert.deepStrictEqual(items, []);
});

test('safe-area: items are deduped by key|edge, sorted by key then edge', () => {
  const items = safeAreaItems(
    [
      { key: 'key:b', rect: { l: 20, t: 0, r: 100, b: 20 } },   // top
      { key: 'key:a', rect: { l: 0, t: 830, r: 60, b: 844 } },  // bottom (band 810..844)
    ],
    { top: 47, bottom: 34, left: 0, right: 0 },
    SCREEN,
  );
  assert.deepStrictEqual(items, [
    { key: 'key:a', edge: 'bottom', by: 14 },
    { key: 'key:b', edge: 'top', by: 20 },
  ]);
});

// ---- BROKEN-ASSET (tofu: a rendered U+FFFD) ----------------------------------
test('tofu: a rendered U+FFFD is flagged; clean text is silent', () => {
  assert.strictEqual(tofuReason('glyph � here'), 'tofu');
  assert.strictEqual(tofuReason('all glyphs resolve'), null);
  assert.strictEqual(tofuReason(''), null);
  assert.strictEqual(tofuReason(null), null);
});

test('tofu items: deduped on key, sorted, detail trimmed + clipped to 60', () => {
  const raw = [
    { key: 'role:text#2', reason: 'tofu', detail: '  b�d  ' },
    { key: 'key:desc', reason: 'tofu', detail: '�'.repeat(120) },
    { key: 'key:desc', reason: 'tofu', detail: 'dup' }, // same key|reason -> dropped
  ];
  const a = brokenAssetItems(raw);
  assert.deepStrictEqual(a, brokenAssetItems(raw), 'deterministic');
  assert.deepStrictEqual(a.map((i) => i.key), ['key:desc', 'role:text#2'], 'sorted by key');
  assert.strictEqual(a[0].detail.length, 60, 'detail clipped to 60');
  assert.strictEqual(a[1].detail, 'b�d', 'detail trimmed');
  assert.ok(a.every((i) => i.reason === 'tofu'));
});

// ---- snapshot wiring (fake driver: the walk collects what the reducers eat) --
// snapshot() only needs getPageSource on the driver, so the whole tree walk
// (DFS intervals for the overlap exclusion, tofu text, blank facts, the
// AppiumAUT/hierarchy geometry-less wrapper) is exercised with no device.
test('snapshot wiring: tofu/blank collected from a parsed page source', async () => {
  const xml = `
    <AppiumAUT>
      <XCUIElementTypeApplication x="0" y="0" width="390" height="844" visible="true">
        <XCUIElementTypeOther x="0" y="0" width="390" height="844" visible="true">
          <XCUIElementTypeButton name="tinyBtn" label="Go" x="10" y="10" width="10" height="10" visible="true"/>
          <XCUIElementTypeButton name="alpha" label="A" x="100" y="100" width="48" height="48" visible="true"/>
          <XCUIElementTypeButton name="beta" label="B" x="124" y="124" width="48" height="48" visible="true"/>
          <XCUIElementTypeButton name="outer" label="Wrap" x="200" y="500" width="96" height="96" visible="true">
            <XCUIElementTypeButton name="inner" label="Core" x="224" y="524" width="48" height="48" visible="true"/>
          </XCUIElementTypeButton>
          <XCUIElementTypeStaticText name="desc" label="glyph &#xFFFD; here" x="0" y="300" width="200" height="20" visible="true"/>
        </XCUIElementTypeOther>
      </XCUIElementTypeApplication>
    </AppiumAUT>`;
  const snap = await snapshot({ getPageSource: async () => xml }, []);
  assert.deepStrictEqual(snap.brokenAssets, [
    { key: 'key:desc', reason: 'tofu', detail: 'glyph � here' },
  ]);
  assert.deepStrictEqual(snap.blank, [], 'a screen with content is not blank');

  // A WSOD page source: containers only, zero text, zero tappables. The window
  // frame comes from the application element under the geometry-less wrapper.
  const blankXml = `
    <AppiumAUT>
      <XCUIElementTypeApplication x="0" y="0" width="390" height="844" visible="true">
        <XCUIElementTypeOther x="0" y="0" width="390" height="844" visible="true"/>
      </XCUIElementTypeApplication>
    </AppiumAUT>`;
  const blank = await snapshot({ getPageSource: async () => blankXml }, []);
  assert.deepStrictEqual(blank.blank, [{ key: 'root', w: 390, h: 844 }]);
  assert.deepStrictEqual(blank.brokenAssets, []);
});

// ---- APP-INVARIANT marker scrape (SDK-self-triggered -> EXPLORE:INVARIANT) ---
// The RN/iOS/Android SDKs log `REPROIT_INVARIANT {...}` on the device diagnostic
// channel when a registered invariant is violated under the fuzzer; the runner
// scrapes that channel each settle and maps it into the EXPLORE:INVARIANT line
// the Rust core parses, substituting the sig it is currently on. No device
// needed: these drive the pure parser + a fake driver whose getLogs returns the
// marker lines.

test('parseInvariantMarker: extracts JSON tolerant of log framing + trailing text', () => {
  // logcat-style framing before the token, clean object at the end.
  const a = parseInvariantMarker(
    '07-06 12:00:00.123  1234  1234 I reproit : REPROIT_INVARIANT {"sig":"","items":[{"id":"x","message":"boom"}]}'
  );
  assert.deepStrictEqual(a, { sig: '', items: [{ id: 'x', message: 'boom' }] });
  // NSLog-style framing + trailing content after the object.
  const b = parseInvariantMarker(
    '2026-07-06 12:00:00 App[42:99] REPROIT_INVARIANT {"sig":"","items":[{"id":"y","message":""}]} extra'
  );
  assert.deepStrictEqual(b, { sig: '', items: [{ id: 'y', message: '' }] });
  assert.strictEqual(parseInvariantMarker('nothing to see here'), null);
});

// Capture the runner's stdout marker stream while running an async fn.
async function captureLog(fn) {
  const orig = process.stdout.write;
  const lines = [];
  process.stdout.write = (chunk) => { lines.push(String(chunk)); return true; };
  try { await fn(); } finally { process.stdout.write = orig; }
  return lines.join('').split('\n').filter((l) => l.length);
}

test('scrapeInvariants: a VIOLATING marker emits EXPLORE:INVARIANT with the current sig', async () => {
  invariantEmitted.clear();
  const driver = {
    getLogs: async () => [
      { message: 'REPROIT_INVARIANT {"sig":"","items":[{"id":"cart","message":"went negative"}]}' },
    ],
  };
  const out = await captureLog(() => scrapeInvariants(driver, 'abcd1234', '/cart'));
  assert.strictEqual(out.length, 1);
  const obj = JSON.parse(out[0].slice('EXPLORE:INVARIANT '.length));
  assert.strictEqual(obj.sig, 'abcd1234'); // runner substitutes ITS sig, not the SDK's ""
  assert.strictEqual(obj.route, '/cart');
  assert.deepStrictEqual(obj.items, [{ id: 'cart', message: 'went negative' }]);
});

test('scrapeInvariants: a CLEAN state (no marker) is silent', async () => {
  invariantEmitted.clear();
  const driver = { getLogs: async () => [{ message: 'ReactNativeJS: ordinary log line' }] };
  const out = await captureLog(() => scrapeInvariants(driver, 'feedface', null));
  assert.deepStrictEqual(out, []);
});

test('scrapeInvariants: de-dups the same violation across settles of the same state', async () => {
  invariantEmitted.clear();
  const driver = {
    getLogs: async () => [
      { message: 'REPROIT_INVARIANT {"sig":"","items":[{"id":"a","message":"m"}]}' },
    ],
  };
  const first = await captureLog(() => scrapeInvariants(driver, 'state1', null));
  const second = await captureLog(() => scrapeInvariants(driver, 'state1', null));
  assert.strictEqual(first.length, 1, 'first settle emits');
  assert.strictEqual(second.length, 0, 'same sig|id|message on re-settle is suppressed');
  // The SAME violation in a DIFFERENT state is a distinct finding and emits.
  const other = await captureLog(() => scrapeInvariants(driver, 'state2', null));
  assert.strictEqual(other.length, 1, 'a different state re-emits');
});

// ---- WAKELOCK LEAK (Android dumpsys power parse + leak reducer) --------------
const PKG = 'com.example.myapp';
const DUMPSYS_POWER = `
Power Manager State:
  mWakefulness=Awake
  Wake Locks: size=3
    PARTIAL_WAKE_LOCK              'com.example.myapp:VideoPlayback' ON_AFTER_RELEASE ACQ=-4s12ms (uid=10234 pid=1234 ws=WorkSource{10234 com.example.myapp})
    PARTIAL_WAKE_LOCK              'AudioMix' ACQ=-1s (uid=1000 pid=555)
    PARTIAL_WAKE_LOCK              '*job*/com.android.systemui' ACQ=-2s (uid=1000)
  Suspend Blockers:
    PowerManagerService.WakeLocks: ref count=1
`;

test('wakelock: dumpsys power keeps only app-owned awake locks', () => {
  const held = wakelocksFromDumpsysPower(DUMPSYS_POWER, PKG);
  // Only the target-package lock survives: 'AudioMix' (system uid) and the
  // *job* system lock do not name the package, so they are excluded.
  assert.deepStrictEqual([...held], ['com.example.myapp:VideoPlayback']);
  // No package / no text -> empty set, never throws.
  assert.strictEqual(wakelocksFromDumpsysPower(DUMPSYS_POWER, '').size, 0);
  assert.strictEqual(wakelocksFromDumpsysPower('', PKG).size, 0);
});

test('wakelock: focused-window FLAG_KEEP_SCREEN_ON is detected for the package', () => {
  const win = `
    Window #2 Window{aaaa u0 com.other/com.other.Home}:
      mAttrs=... fl=... flags:
      LAYOUT_IN_SCREEN
    Window #3 Window{bbbb u0 com.example.myapp/com.example.myapp.PlayerActivity}:
      mAttrs=WM.LayoutParams(...)
      fl=KEEP_SCREEN_ON LAYOUT_IN_SCREEN
  `;
  assert.strictEqual(keepScreenOnFromDumpsys(win, PKG), true);
  // The flag on a DIFFERENT app's window does not count for us.
  const other = win.replace('com.example.myapp/com.example.myapp.PlayerActivity', 'com.other/com.other.Vid');
  assert.strictEqual(keepScreenOnFromDumpsys(other, PKG), false);
});

test('wakelock reducer: a lock acquired on X still held on Y is a leak, reported once', () => {
  const baseline = new Set();          // nothing held at launch
  let st = { origin: new Map(), reported: new Set() };
  const lock = new Set(['com.example.myapp:VideoPlayback']);
  // Dwell on the video screen (self-transition records nothing / no leak).
  // Leave video -> home with the lock STILL held: one leak, attributed to video.
  let step = wakelockLeakStep(st, baseline, lock, lock, 'video', 'home');
  assert.deepStrictEqual(step.leaks, ['com.example.myapp:VideoPlayback']);
  st = { origin: step.origin, reported: step.reported };
  // home -> settings, still held: NOT re-flagged (reported once until released).
  step = wakelockLeakStep(st, baseline, lock, lock, 'home', 'settings');
  assert.deepStrictEqual(step.leaks, []);
  // The finding item carries the tag + kind the Rust core parses.
  assert.deepStrictEqual(wakelockItem('com.example.myapp:VideoPlayback'), { tag: 'com.example.myapp:VideoPlayback', kind: 'wakelock' });
  assert.deepStrictEqual(wakelockItem('KEEP_SCREEN_ON'), { tag: 'KEEP_SCREEN_ON', kind: 'keep-screen-on' });
});

test('wakelock reducer: FP-safe on baseline, released, and short-lived locks', () => {
  // A baseline (app-global) lock held on X and Y is never flagged.
  const globalLock = new Set(['com.example.myapp:Sync']);
  let step = wakelockLeakStep({ origin: new Map(), reported: new Set() }, globalLock, globalLock, globalLock, 'x', 'y');
  assert.deepStrictEqual(step.leaks, [], 'app-global baseline lock is not a leak');
  // A lock acquired on X but RELEASED before Y (gone from the after-sample) is
  // healthy and never fires.
  const held = new Set(['com.example.myapp:VideoPlayback']);
  step = wakelockLeakStep({ origin: new Map(), reported: new Set() }, new Set(), held, new Set(), 'video', 'home');
  assert.deepStrictEqual(step.leaks, [], 'a released lock is healthy');
  // A same-screen transition (no navigation away) never flags, even if held.
  step = wakelockLeakStep({ origin: new Map(), reported: new Set() }, new Set(), held, held, 'video', 'video');
  assert.deepStrictEqual(step.leaks, [], 'no leak without leaving the screen');
});

// ---- DEFECT 1: multi-seed BATCH replay contract (the check-replay gap) -------
// `reproit check` with gate.runs > 1 writes {"batch":[<cfg>,...]}; the runner
// MUST parse that into per-seed configs (so the stored `replay` is honored) and
// bracket each in SEED:BEGIN/SEED:END. Before the fix loadFuzz() handed the whole
// {batch:..} object back as ONE config whose `replay`/`seed` were undefined, so
// the runner silently fell into a fresh explore walk and never replayed the
// stored actions -> a real crash repro re-confirmed clean (PASS). loadBatch reads
// the SAME env channel (REPROIT_FUZZ_CONFIG) the Rust core writes, so faking that
// file exercises exactly the check -> runner handoff.
function withFuzzConfig(obj, fn) {
  const dir = mkdtempSync(join(tmpdir(), 'reproit-rn-batch-'));
  const p = join(dir, 'fuzz_config.json');
  writeFileSync(p, JSON.stringify(obj));
  const prev = process.env.REPROIT_FUZZ_CONFIG;
  process.env.REPROIT_FUZZ_CONFIG = p;
  try { return fn(); } finally {
    if (prev === undefined) delete process.env.REPROIT_FUZZ_CONFIG;
    else process.env.REPROIT_FUZZ_CONFIG = prev;
  }
}

test('batch: a multi-seed {batch:[...]} check config yields one seed per replay', () => {
  const cfg = {
    batch: [
      { seed: 5, replay: ['tap:role:button#2'] },
      { seed: 5, replay: ['tap:role:button#2'] },
      { seed: 5, replay: ['tap:role:button#2'] },
    ],
  };
  const { seeds, isBatch } = withFuzzConfig(cfg, loadBatch);
  assert.strictEqual(isBatch, true, 'the {batch:..} shape is a batch');
  assert.strictEqual(seeds.length, 3, 'one config per replay (gate.runs=3)');
  // Each seed carries the STORED replay actions verbatim, so the runner replays
  // `tap:role:button#2` (the crash tap) instead of exploring button#0/#1.
  for (const s of seeds) {
    assert.deepStrictEqual(s.replay, ['tap:role:button#2']);
    assert.strictEqual(s.seed, 5);
  }
});

test('batch: a bare single {seed,replay} config is NOT a batch (no SEED markers)', () => {
  const cfg = { seed: 5, replay: ['tap:role:button#2'] };
  const { seeds, isBatch } = withFuzzConfig(cfg, loadBatch);
  assert.strictEqual(isBatch, false, 'the compact single-replay shape stays un-bracketed');
  assert.strictEqual(seeds.length, 1);
  assert.deepStrictEqual(seeds[0].replay, ['tap:role:button#2']);
});

test('batch: an absent config is a single empty seed (a plain explore/map run)', () => {
  const prev = process.env.REPROIT_FUZZ_CONFIG;
  delete process.env.REPROIT_FUZZ_CONFIG;
  try {
    const { seeds, isBatch } = loadBatch();
    assert.strictEqual(isBatch, false);
    assert.strictEqual(seeds.length, 1);
    assert.deepStrictEqual(seeds[0], {});
  } finally {
    if (prev !== undefined) process.env.REPROIT_FUZZ_CONFIG = prev;
  }
});

test('batch: a {batch:[]} empty array degrades to a single seed (never zero walks)', () => {
  const { seeds, isBatch } = withFuzzConfig({ batch: [] }, loadBatch);
  assert.strictEqual(isBatch, false);
  assert.strictEqual(seeds.length, 1);
});

// ---- DEFECT 2: baseline-relative jank (software-compositor FP guard) ---------
// Under an emulator's SOFTWARE GPU trivial Activity transitions drop tens of
// percent of frames purely from the compositor, tripping the absolute 30% floor.
// The floor is raised by the device baseline (+margin) and clamped to a near-
// total-drop floor when a software renderer is detected; real hardware (baseline
// ~0, no software floor) is unchanged.
test('jank floor: real hardware keeps the absolute 30% floor', () => {
  assert.strictEqual(jankFloorFor(0, false), 30);
  assert.strictEqual(jankFloorFor(2, false), 30, 'a tiny baseline does not lower the floor');
  assert.strictEqual(jankFloorFor(null, false), 30, 'no calibration => absolute floor');
});

test('jank floor: baseline + software renderer raise the floor', () => {
  // baseline 40% + 25 margin = 65, clamped up to the 80% software floor.
  assert.strictEqual(jankFloorFor(40, true), 80);
  // A high real-device baseline (no software GPU) still lifts the floor by margin.
  assert.strictEqual(jankFloorFor(50, false), 75);
  // Software GPU with a low idle baseline still gets the software floor.
  assert.strictEqual(jankFloorFor(5, true), 80);
});

test('jank: raw parse is exposed and floor-independent', () => {
  assert.deepStrictEqual(
    jankyPctFromGfxinfo('Total frames rendered: 100\nJanky frames: 41 (41.00%)\n'),
    { pct: 41, count: 41 },
  );
  assert.strictEqual(jankyPctFromGfxinfo('no framestats'), null);
});

test('jank: a software-compositor transition below the raised floor is silent', () => {
  const gfx = 'Total frames rendered: 60\nJanky frames: 25 (41.67%)\n';
  // Default (real-hardware) floor: a 41.67% storm fires.
  assert.ok(jankFromGfxinfo(gfx), 'fires at the absolute 30% floor');
  // Software-GPU floor (80%): the same trivial-transition jank is now silenced.
  assert.strictEqual(jankFromGfxinfo(gfx, 80), null, 'no FP under a software compositor');
});

test('jank: a REAL main-thread stall still fires under the software floor', () => {
  // A planted long-task jank storm drops nearly every frame -> well past 80%.
  const gfx = 'Total frames rendered: 60\nJanky frames: 57 (95.00%)\n';
  const r = jankFromGfxinfo(gfx, 80);
  assert.ok(r, 'a genuine stall clears even the software floor');
  assert.strictEqual(r.bucket, 30, 'the marker still carries the fixed bucket (deterministic id)');
  assert.strictEqual(r.count, 57);
});

// ---- BACK-TRAP decision (narrow dead-end slice; Android back swallowed) ------
// isBackTrap(before, first, retry, launch) is the pure gate behind the runner's
// back-swallow detector. Snapshots are {sig, content, anchor}; it must fire ONLY
// for a non-root screen whose back press self-loops on BOTH signature and content,
// twice (first + retry). Faked signatures below, no device needed.
const LAUNCH = { sig: 'home', anchor: 'com.bugzoo/.MainActivity' };
const TRAP = { sig: 'deadend', content: 'c-deadend', anchor: 'com.bugzoo/.DeadEndActivity' };
const selfLoop = (s) => ({ sig: s.sig, content: s.content, anchor: s.anchor });

test('back-trap: a non-root screen that swallows back twice IS a trap', () => {
  // Both the first press and the retry leave sig+content pinned on a non-root
  // activity: the screen ate the system back. This is the planted bugzoo dead end.
  assert.strictEqual(isBackTrap(TRAP, selfLoop(TRAP), selfLoop(TRAP), LAUNCH), true);
});

test('back-trap: the root/home activity is never a trap (back exits there)', () => {
  // Same self-loop, but the screen IS the launch activity: back is expected to be a
  // no-op or app-exit on root, so it must never fire (guard 1: non-root).
  const root = { sig: 'home', content: 'c-home', anchor: LAUNCH.anchor };
  assert.strictEqual(isBackTrap(root, selfLoop(root), selfLoop(root), LAUNCH), false);
  // Also guarded when the signature (not just the anchor) equals the launch sig.
  const rootBySig = { sig: 'home', content: 'c-home', anchor: 'com.bugzoo/.OtherActivity' };
  assert.strictEqual(isBackTrap(rootBySig, selfLoop(rootBySig), selfLoop(rootBySig), LAUNCH), false);
});

test('back-trap: a back that closed a dialog/sheet is not a trap (sig moved)', () => {
  // The first press dismissed an overlay: the signature changed, so it is a normal
  // back, not a swallow. Guard 2: the FIRST observation must be a pure self-loop.
  const afterDialog = { sig: 'deadend-base', content: 'c-base', anchor: TRAP.anchor };
  assert.strictEqual(isBackTrap(TRAP, afterDialog, afterDialog, LAUNCH), false);
});

test('back-trap: a content-only change (value state) is not a swallow', () => {
  // Sig unchanged but the content fingerprint moved: back had an observable effect,
  // so it is effective, not swallowed. Both sig AND content must be pinned.
  const contentMoved = { sig: TRAP.sig, content: 'c-different', anchor: TRAP.anchor };
  assert.strictEqual(isBackTrap(TRAP, contentMoved, contentMoved, LAUNCH), false);
});

test('back-trap: a self-loop that clears on the retry is animation, not a trap', () => {
  // First press read as a self-loop (mid-animation), but the retry moved: this was a
  // slow transition, never a trap. Guard 3: the retry must ALSO self-loop.
  const moved = { sig: 'nextscreen', content: 'c-next', anchor: 'com.bugzoo/.NextActivity' };
  assert.strictEqual(isBackTrap(TRAP, selfLoop(TRAP), moved, LAUNCH), false);
});

test('back-trap: a missing anchor never fires (best-effort activity read)', () => {
  // getCurrentActivity can be unavailable; without an anchor we cannot prove the
  // screen is non-root, so the oracle stays silent (no FP on an unknown activity).
  const noAnchor = { sig: 'deadend', content: 'c-deadend', anchor: null };
  assert.strictEqual(isBackTrap(noAnchor, selfLoop(noAnchor), selfLoop(noAnchor), LAUNCH), false);
});
