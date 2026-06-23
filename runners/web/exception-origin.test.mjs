// Unit tests for the first-party exception filter (the cross-origin-exception
// false positive: errors thrown entirely inside third-party scripts - analytics,
// ad SDKs - were being reported as app crashes). See runner.mjs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { exceptionIsFirstParty } from './runner.mjs';

test('drops a throw entirely inside a third-party script (Facebook Pixel)', () => {
  const stack =
    'TypeError: Promise.allSettled is not a function\n' +
    '    at r (https://connect.facebook.net/en_US/fbevents.js:53:502)\n' +
    '    at value (https://connect.facebook.net/en_US/fbevents.js:127:11610)';
  assert.equal(exceptionIsFirstParty(stack, 'https://demos.creative-tim.com'), false);
});

test('drops a throw entirely inside an ad SDK (Google IMA)', () => {
  const stack =
    'AdError 1101\n    at https://imasdk.googleapis.com/js/sdkloader/ima3.js:1:200';
  assert.equal(exceptionIsFirstParty(stack, 'https://www.bbc.com'), false);
});

test('keeps a first-party error (frame on the app origin)', () => {
  const stack =
    "TypeError: Cannot read properties of null (reading 'querySelectorAll')\n" +
    '    at o (https://vuejs.org/assets/chunks/src._afsJy80.js:2:37627)';
  assert.equal(exceptionIsFirstParty(stack, 'https://vuejs.org'), true);
});

test('keeps a mixed-frame error if ANY frame is first-party', () => {
  const stack =
    'Error: hydration mismatch\n' +
    '    at https://static.files.bbci.co.uk/bundle.js:1:5\n' +
    '    at https://www.bbc.com/app.js:9:1';
  assert.equal(exceptionIsFirstParty(stack, 'https://www.bbc.com'), true);
});

test('keeps an error with no resolvable http(s) frame (do not drop on missing evidence)', () => {
  assert.equal(exceptionIsFirstParty('TypeError: x is undefined\n    at <anonymous>', 'https://app.com'), true);
  assert.equal(exceptionIsFirstParty('', 'https://app.com'), true);
});

test('no app origin known -> never drop', () => {
  const stack = 'Error\n    at https://connect.facebook.net/fbevents.js:1:1';
  assert.equal(exceptionIsFirstParty(stack, ''), true);
});
