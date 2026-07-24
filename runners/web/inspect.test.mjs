import assert from 'node:assert/strict';
import test from 'node:test';
import { chromium } from 'playwright';
import {
  boundedInspectWaitMs,
  humanizeInspectAction,
  inspectReplayFinished,
  inspectReplayStep,
  inspectStepModel,
} from './inspect.mjs';

test('inspection bounds its wait and humanizes stored actions', () => {
  assert.equal(boundedInspectWaitMs('bad'), 240_000);
  assert.equal(boundedInspectWaitMs('1'), 1_000);
  assert.equal(boundedInspectWaitMs('99999999'), 900_000);
  assert.equal(humanizeInspectAction('tap:key:testid:checkout'), 'Tap key:testid:checkout');
  assert.equal(
    humanizeInspectAction('type:key:name:email=long'),
    'Type "long" in key:name:email',
  );
});

test('step model identifies the visible target and final trigger', () => {
  const model = inspectStepModel('tap:key:testid:checkout', 2, 2, {
    tappables: [
      {
        sel: 'key:testid:checkout',
        label: 'Checkout',
        bounds: [10, 20, 100, 30],
      },
    ],
  });
  assert.equal(model.targetLabel, 'Checkout');
  assert.deepEqual(model.targetBounds, [10, 20, 100, 30]);
  assert.equal(model.isTrigger, true);
});

test('browser inspector gates a step and removes itself before execution', async () => {
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    await page.setContent('<button data-testid="checkout">Checkout</button>');
    const decision = inspectReplayStep(
      page,
      {
        actionLabel: 'Tap Checkout',
        stepIndex: 1,
        totalSteps: 2,
        isTrigger: false,
        targetLabel: 'Checkout',
        targetBounds: [8, 8, 90, 30],
      },
      5_000,
    );
    await page.getByRole('button', { name: 'Run next action' }).click();
    assert.equal(await decision, 'step');
    assert.equal(await page.locator('#__reproit_inspector').count(), 0);
  } finally {
    await browser.close();
  }
});

test('browser inspector can continue and explicitly finish review', async () => {
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    await page.setContent('<main>App</main>');
    const decision = inspectReplayStep(
      page,
      {
        actionLabel: 'Tap Checkout',
        stepIndex: 2,
        totalSteps: 2,
        isTrigger: true,
        targetLabel: null,
        targetBounds: null,
      },
      5_000,
    );
    await page.getByRole('button', { name: 'Continue to failure' }).click();
    assert.equal(await decision, 'continue');

    const finished = inspectReplayFinished(page, 5_000);
    await page.getByRole('button', { name: 'Close inspector' }).click();
    await finished;
    assert.equal(await page.locator('#__reproit_inspector').count(), 0);
  } finally {
    await browser.close();
  }
});
