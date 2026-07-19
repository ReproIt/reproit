import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chromium } from 'playwright';
import {
  collectNativeAccessibilityStateInPage,
  confirmAccessibilityStateParity,
  evaluateAccessibilityStateParity,
  scanAccessibilityStateParity,
} from './accessibility-state-oracle.mjs';

const control = (value = 'true') => [
  {
    identity: 'key:id:notifications',
    backendDOMNodeId: 7,
    settled: true,
    states: [{ property: 'checked', value }],
  },
];
const ax = (value = 'true') => [
  {
    backendDOMNodeId: 7,
    ignored: false,
    properties: [{ name: 'checked', value: { type: 'tristate', value } }],
  },
];

test('matching authoritative DOM and accessibility states are SATISFIED', () => {
  const result = evaluateAccessibilityStateParity(control(), ax());
  assert.equal(result.outcome, 'SATISFIED');
  assert.deepEqual(result.items, []);
  assert.deepEqual(result.checks, [
    {
      identity: 'key:id:notifications',
      property: 'checked',
      fingerprint: 'sha256:f264f36f3b511e4ae5993d43',
      expected: 'true',
      actual: 'true',
      outcome: 'SATISFIED',
    },
  ]);
});

test('an exact native-versus-semantic contradiction is a VIOLATION', () => {
  const result = evaluateAccessibilityStateParity(control('true'), ax('false'));
  assert.equal(result.outcome, 'VIOLATION');
  assert.deepEqual(result.items, [
    {
      identity: 'key:id:notifications',
      property: 'checked',
      fingerprint: 'sha256:f264f36f3b511e4ae5993d43',
      expected: 'true',
      actual: 'false',
      outcome: 'VIOLATION',
      reason: 'semantic-state-mismatch',
    },
  ]);
});

test('missing, ignored, and unsettled evidence explicitly ABSTAINS', () => {
  assert.equal(evaluateAccessibilityStateParity(control(), []).outcome, 'ABSTAIN');
  assert.equal(
    evaluateAccessibilityStateParity(control(), [{ ...ax()[0], ignored: true }]).outcome,
    'ABSTAIN',
  );
  assert.deepEqual(
    evaluateAccessibilityStateParity([{ ...control()[0], settled: false }], ax()).checks[0],
    {
      identity: 'key:id:notifications',
      property: 'checked',
      fingerprint: 'sha256:f264f36f3b511e4ae5993d43',
      expected: 'true',
      outcome: 'ABSTAIN',
      reason: 'control-not-settled',
    },
  );
});

test('an authored ARIA override ABSTAINS without a third authority', () => {
  const overridden = control('false');
  overridden[0].states[0].semanticOverride = true;
  const result = evaluateAccessibilityStateParity(overridden, ax('mixed'));
  assert.equal(result.outcome, 'ABSTAIN');
  assert.equal(result.checks[0].reason, 'authored-semantic-override');
  assert.deepEqual(result.items, []);
});

test('confirmation requires the same identity, values, and outcome twice', () => {
  const violation = evaluateAccessibilityStateParity(control('true'), ax('false'));
  assert.equal(confirmAccessibilityStateParity(violation, violation).outcome, 'VIOLATION');

  const changed = evaluateAccessibilityStateParity(control('false'), ax('false'));
  const unstable = confirmAccessibilityStateParity(violation, changed);
  assert.equal(unstable.outcome, 'ABSTAIN');
  assert.equal(unstable.checks[0].reason, 'state-not-settled');
  assert.deepEqual(unstable.items, []);
});

test('DOM authority accepts only visible native controls with stable unique ids', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(`<!doctype html><body>
      <input id="kept" type="checkbox" checked>
      <input type="checkbox" checked>
      <input id="hidden" type="checkbox" hidden checked>
      <div id="custom" role="checkbox" aria-checked="true">custom</div>
    </body>`);
    const controls = await page.evaluate(collectNativeAccessibilityStateInPage);
    assert.deepEqual(
      controls.map(({ identity, states }) => ({ identity, states })),
      [
        {
          identity: 'key:id:kept',
          states: [{ property: 'checked', value: 'true', semanticOverride: false }],
        },
      ],
    );
  } finally {
    await browser.close();
  }
});

test('real Chromium AX tree satisfies native state and preserves mixed state', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(`<!doctype html><body>
      <input id="checked" type="checkbox" checked>
      <input id="mixed" type="checkbox">
      <button id="disabled" disabled>Save</button>
    </body>`);
    await page.locator('#mixed').evaluate((element) => {
      element.indeterminate = true;
    });
    const result = await scanAccessibilityStateParity(page, 10);
    assert.equal(result.outcome, 'SATISFIED');
    assert.deepEqual(result.items, []);
    assert.ok(
      result.checks.some(
        (check) =>
          check.identity === 'key:id:mixed' &&
          check.property === 'checked' &&
          check.actual === 'mixed',
      ),
    );
  } finally {
    await browser.close();
  }
});

test('ARIA-disabled on an enabled native control is outside the proof boundary', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(
      '<!doctype html><body><button id="contradiction" aria-disabled="true">Save</button>',
    );
    const result = await scanAccessibilityStateParity(page, 10);
    assert.equal(result.outcome, 'ABSTAIN');
    assert.deepEqual(result.items, []);
  } finally {
    await browser.close();
  }
});

// Historical red/green pair: mui/material-ui#20476, fixed by PR #48147.
// MUI represented indeterminate intent in component state before adding
// aria-checked="mixed". The native checked property is not an independent
// authority for that intent, so the old revision is a clean miss and the fixed
// revision must not be mislabeled as a contradiction.
test('MUI indeterminate historical pair does not create a false positive', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(
      '<input id="mui-checkbox" type="checkbox" data-indeterminate="true">',
    );
    const buggy = await scanAccessibilityStateParity(page, 10);
    assert.equal(buggy.outcome, 'SATISFIED');
    assert.deepEqual(buggy.items, []);

    await page.locator('#mui-checkbox').evaluate((element) => {
      element.setAttribute('aria-checked', 'mixed');
    });
    const fixed = await scanAccessibilityStateParity(page, 10);
    assert.equal(fixed.outcome, 'ABSTAIN');
    assert.equal(fixed.checks[0].reason, 'authored-semantic-override');
    assert.deepEqual(fixed.items, []);
  } finally {
    await browser.close();
  }
});
