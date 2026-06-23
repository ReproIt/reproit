// Unit tests for the first-party exception filter (the cross-origin-exception
// false positive: errors thrown entirely inside third-party scripts - analytics,
// ad SDKs - were being reported as app crashes). See runner.mjs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { exceptionIsFirstParty, exceptionIsBenign } from './runner.mjs';

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

// exceptionIsBenign: known-benign browser-policy errors (stackless, so the
// origin filter can't catch them) must be dropped by message.
test('drops the cross-origin iframe SecurityError (stackless)', () => {
  const msg = "Failed to read a named property 'document' from 'Window': " +
    'Blocked a frame with origin "https://www.bvp.com" from accessing a cross-origin frame.';
  assert.equal(exceptionIsBenign(msg), true);
});

test('drops the Firefox cross-origin variant and ResizeObserver loop noise', () => {
  assert.equal(exceptionIsBenign('Permission denied to access property "document" on cross-origin object'), true);
  assert.equal(exceptionIsBenign('ResizeObserver loop completed with undelivered notifications.'), true);
  assert.equal(exceptionIsBenign('ResizeObserver loop limit exceeded'), true);
});

test('does NOT suppress the real crashes the sweep found', () => {
  // These are genuine first-party bugs and must keep firing.
  assert.equal(exceptionIsBenign('Minified React error #418; visit https://react.dev/errors/418'), false);
  assert.equal(exceptionIsBenign("Identifier 'resizeTimeout' has already been declared"), false); // not "ResizeObserver loop"
  assert.equal(exceptionIsBenign("Cannot read properties of null (reading 'addEventListener')"), false);
  assert.equal(exceptionIsBenign('how is not defined'), false);
  assert.equal(exceptionIsBenign(''), false);
});
