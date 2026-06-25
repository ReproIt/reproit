// Validates the CHOICE-ANOMALY oracle in both directions:
//   1. the host-pure pieces (layoutDelta / medianOf / classifyChoiceOutlier) on
//      synthetic magnitudes -- a lone outlier fires, uniform choices stay silent,
//      a near-floor bump stays silent (no browser needed);
//   2. the self-contained in-page pass (choiceAnomalyInPage, FEATURE 1: native
//      <select> as a choice component) on a REAL Chromium over a static fixture:
//      it must FIRE on the buggy <select> (one option shifts the whole page) and
//      stay SILENT on the clean <select> (every option behaves the same), and it
//      must RESTORE both selects' original values afterward (non-destructive).
// This is the same code unit -- choiceAnomalyInPage -- that electron runs via
// page.evaluate and tauri injects via executeAsync, so passing here covers all
// three ports' detector. Run with `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { chromium } from 'playwright';
import {
  layoutDelta, medianOf, classifyChoiceOutlier, choiceAnomalyInPage,
  CHOICE_OUTLIER_RATIO, CHOICE_MIN_MAGNITUDE, CHOICE_ROLES,
} from './choice-oracle.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_URL = pathToFileURL(join(HERE, 'choice-fixture.html')).href;

test('medianOf: odd and even length', () => {
  assert.strictEqual(medianOf([3, 1, 2]), 2);
  assert.strictEqual(medianOf([4, 1, 2, 3]), 2.5);
  assert.strictEqual(medianOf([]), 0);
});

test('layoutDelta: horizontal overflow + anchor displacement', () => {
  const base = { hOverflow: 0, anchors: [[0, 0], [48, 0]] };
  const cur = { hOverflow: 100, anchors: [[0, 0], [148, 0]] };
  // 100 (overflow) + 100 (second anchor moved 100px down) = 200.
  assert.strictEqual(layoutDelta(base, cur), 200);
  assert.strictEqual(layoutDelta(null, cur), 0);
});

test('classifyChoiceOutlier: a lone outlier fires past the floor + ratio', () => {
  // Siblings ~5px, one option ~300px: 300 >= 24 (floor) and >= 3x median(5).
  const r = classifyChoiceOutlier([0, 5, 4, 300, 6]);
  assert.ok(r, 'expected an outlier finding');
  assert.strictEqual(r.magnitude, 300);
  assert.strictEqual(r.siblingMedian, 5);
});

test('classifyChoiceOutlier: uniform choices produce nothing', () => {
  assert.strictEqual(classifyChoiceOutlier([0, 40, 42, 41, 39]), null);
});

test('classifyChoiceOutlier: an outlier below the magnitude floor stays silent', () => {
  // 20px max is a 10x outlier vs 2px siblings, but below the 24px floor.
  assert.strictEqual(classifyChoiceOutlier([0, 2, 2, 20, 2]), null);
});

test('classifyChoiceOutlier: needs >= 3 valid options', () => {
  assert.strictEqual(classifyChoiceOutlier([0, 100]), null);
});

test('choiceAnomalyInPage fires on the buggy <select>, is silent on the clean one, and restores values', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.goto(FIXTURE_URL);

    const origLang = await page.evaluate(() => document.getElementById('lang').value);
    const origSize = await page.evaluate(() => document.getElementById('size').value);

    const findings = await page.evaluate(choiceAnomalyInPage, {
      settleMs: 80, ratio: CHOICE_OUTLIER_RATIO, minMag: CHOICE_MIN_MAGNITUDE, choiceRoles: CHOICE_ROLES,
    });

    // FIRES: exactly one finding, a native <select>, whose outlier is the "Broken"
    // option (the one that pushes the page into horizontal overflow).
    const selectFindings = findings.filter((f) => f.kind === 'select');
    assert.strictEqual(selectFindings.length, 1,
      `expected exactly one <select> anomaly, got ${JSON.stringify(findings)}`);
    assert.strictEqual(selectFindings[0].outlier, 'Broken',
      `expected the Broken option to be the outlier, got ${JSON.stringify(selectFindings[0])}`);
    assert.ok(selectFindings[0].magnitude >= CHOICE_MIN_MAGNITUDE);

    // SILENT: the clean <select> (id=size) must not appear -- its options are
    // uniform, so the differential oracle flags nothing for it. (The single
    // finding above already implies this, but assert it explicitly.)
    // There is no second select finding, so nothing more to check.

    // NON-DESTRUCTIVE: both selects are restored to their original values.
    const afterLang = await page.evaluate(() => document.getElementById('lang').value);
    const afterSize = await page.evaluate(() => document.getElementById('size').value);
    assert.strictEqual(afterLang, origLang, 'buggy <select> not restored');
    assert.strictEqual(afterSize, origSize, 'clean <select> not restored');

    // And the page is no longer in horizontal overflow (the overlay was hidden
    // again when the value was restored).
    const overflow = await page.evaluate(
      () => Math.max(0, document.documentElement.scrollWidth - window.innerWidth));
    assert.strictEqual(overflow, 0, 'page left in horizontal overflow after restore');
  } finally {
    await browser.close();
  }
});
