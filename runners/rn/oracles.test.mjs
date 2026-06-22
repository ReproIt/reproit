// Validates the RN runner's pure oracle reducers (no device / Appium / emulator
// needed): the content-bug classifier, the overflow geometry test, the gfxinfo
// jank parser, and the meminfo PSS parser. These mirror the web runner's oracle
// rules and feed the SAME EXPLORE:OVERFLOW / EXPLORE:CONTENTBUG / EXPLORE:JANK /
// MEMORY:SAMPLE markers the Rust core already parses. Run: `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import {
  contentBugReason, contentBugItems, rectOfEl, overflowOf, overflowItems,
  jankFromGfxinfo, pssFromMeminfo, hangBucket,
} from './runner.mjs';

// ---- CONTENT-BUG classifier (byte-identical rule to the web runner) ---------
test('content-bug: the canonical artifacts are flagged', () => {
  assert.strictEqual(contentBugReason('Hello [object Object] world'), 'object-object');
  assert.strictEqual(contentBugReason('Welcome, {{ user.name }}'), 'unrendered-template');
  assert.strictEqual(contentBugReason('Total: ${price}'), 'unrendered-template');
  assert.strictEqual(contentBugReason('Price: undefined'), 'undefined');
  assert.strictEqual(contentBugReason('Items: null'), 'null');
  assert.strictEqual(contentBugReason('Sum: NaN'), 'nan');
});

test('content-bug: ordinary prose is NOT flagged (whole-word guard)', () => {
  assert.strictEqual(contentBugReason('Cancellation policy'), null);     // contains "null"
  assert.strictEqual(contentBugReason('Undefined Behavior Lane'), null); // capitalized prose word
  assert.strictEqual(contentBugReason('Null Island is real'), null);     // capitalized prose word
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

test('overflow: a child spilling out of its parent is flagged', () => {
  const parent = { l: 0, t: 0, r: 100, b: 100 };
  const child = { l: 0, t: 0, r: 180, b: 40 }; // 80px past the parent right edge
  const items = overflowOf('key:label', child, parent, { l: 0, t: 0, r: 400, b: 800 });
  assert.strictEqual(items.length, 1);
  assert.strictEqual(items[0].kind, 'spill');
  assert.strictEqual(items[0].by, 80);
});

test('overflow: an element pushed off-screen is a viewport overflow', () => {
  const screen = { l: 0, t: 0, r: 400, b: 800 };
  const el = { l: 0, t: 0, r: 460, b: 40 }; // 60px past the screen right edge
  const items = overflowOf('key:row', el, null, screen);
  assert.strictEqual(items.length, 1);
  assert.strictEqual(items[0].kind, 'viewport');
  assert.strictEqual(items[0].by, 60);
});

test('overflow: a contained child within tolerance is NOT flagged (no false positive)', () => {
  const parent = { l: 0, t: 0, r: 100, b: 100 };
  const child = { l: 1, t: 1, r: 101, b: 99 }; // 1px over, within OVERFLOW_TOL=2
  const screen = { l: 0, t: 0, r: 400, b: 800 };
  assert.deepStrictEqual(overflowOf('key:c', child, parent, screen), []);
});

test('overflow items: deduped + sorted, deterministic byte-for-byte', () => {
  const raw = [
    { key: 'role:text#1', kind: 'spill', by: 12 },
    { key: 'key:a', kind: 'viewport', by: 30 },
    { key: 'key:a', kind: 'viewport', by: 99 }, // dup key|kind -> dropped
  ];
  const a = overflowItems(raw);
  assert.deepStrictEqual(a, overflowItems(raw));
  assert.deepStrictEqual(a.map((i) => i.key), ['key:a', 'role:text#1']);
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
