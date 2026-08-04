import assert from 'node:assert/strict';
import test from 'node:test';

import { closeDriverSession } from './runner.mjs';

test('Appium session deletion uses a short request budget', async () => {
  const driver = {
    options: {
      connectionRetryTimeout: 1_200_000,
      connectionRetryCount: 3,
    },
    async deleteSession() {},
  };

  const outcome = await closeDriverSession(driver);

  assert.equal(outcome, 'deleted');
  assert.equal(driver.options.connectionRetryTimeout, 10_000);
  assert.equal(driver.options.connectionRetryCount, 0);
});

test('Appium session deletion delegates unavailable cleanup to its owners', async () => {
  const driver = {
    options: {},
    async deleteSession() {
      throw new Error('Appium stopped responding');
    },
  };

  const outcome = await closeDriverSession(driver);

  assert.equal(outcome, 'fallback');
});
