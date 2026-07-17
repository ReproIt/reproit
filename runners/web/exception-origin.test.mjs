// Unit tests for the first-party exception filter (the cross-origin-exception
// false positive: errors thrown entirely inside third-party scripts - analytics,
// ad SDKs - were being reported as app crashes). See runner.mjs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  exceptionIsFirstParty,
  exceptionIsBenign,
  exceptionThrownInTracker,
  exceptionIsNonDeterministic,
} from './runner.mjs';

test('drops a throw entirely inside a third-party script (Facebook Pixel)', () => {
  const stack =
    'TypeError: Promise.allSettled is not a function\n' +
    '    at r (https://connect.facebook.net/en_US/fbevents.js:53:502)\n' +
    ('    at value (https://connect.facebook.net/en_US/fbevents.js:127:11610)' + '');
  assert.equal(exceptionIsFirstParty(stack, 'https://demos.creative-tim.com'), false);
});

test('drops a throw entirely inside an ad SDK (Google IMA)', () => {
  const stack =
    'AdError 1101\n    at https://imasdk.googleapis.com/js/sdkloader/ima3.js:' + '1:200';
  assert.equal(exceptionIsFirstParty(stack, 'https://www.bbc.com'), false);
});

test('keeps a first-party error (frame on the app origin)', () => {
  const stack =
    "TypeError: Cannot read properties of null (reading 'querySelectorAll')\n" +
    '' +
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

test(
  'keeps an error with no resolvable http(s) frame (do not drop on ' + 'missing evidence)',
  () => {
    assert.equal(
      exceptionIsFirstParty('TypeError: x is undefined\n    at <anonymous>', 'https://app.com'),
      true,
    );
    assert.equal(exceptionIsFirstParty('', 'https://app.com'), true);
  },
);

test('no app origin known -> never drop', () => {
  const stack = 'Error\n    at https://connect.facebook.net/fbevents.js:1:1';
  assert.equal(exceptionIsFirstParty(stack, ''), true);
});

// exceptionIsBenign: known-benign browser-policy errors (stackless, so the
// origin filter can't catch them) must be dropped by message.
test('drops the cross-origin iframe SecurityError (stackless)', () => {
  const msg =
    "Failed to read a named property 'document' from 'Window': " +
    ('Blocked a frame with origin "https://www.bvp.com" from accessing a ' + 'cross-origin frame.');
  assert.equal(exceptionIsBenign(msg), true);
});

test('drops the Firefox cross-origin variant and ResizeObserver loop noise', () => {
  assert.equal(
    exceptionIsBenign('Permission denied to access property "document" on cross-origin object'),
    true,
  );
  assert.equal(
    exceptionIsBenign('ResizeObserver loop completed with undelivered notifications.'),
    true,
  );
  assert.equal(exceptionIsBenign('ResizeObserver loop limit exceeded'), true);
});

test('does NOT suppress the real crashes the scan found', () => {
  // These are genuine first-party bugs and must keep firing.
  assert.equal(
    exceptionIsBenign('Minified React error #418; visit https://react.dev/errors/418'),
    false,
  );
  // Unlike "ResizeObserver loop", this is not benign.
  assert.equal(exceptionIsBenign("Identifier 'resizeTimeout' has already been declared"), false);
  assert.equal(
    exceptionIsBenign("Cannot read properties of null (reading 'addEventListener')"),
    false,
  );
  assert.equal(exceptionIsBenign('how is not defined'), false);
  assert.equal(exceptionIsBenign(''), false);
});

// exceptionThrownInTracker: an analytics/tag/tracking script that the app
// self-hosts on its OWN CDN throws -> the origin filter keeps it (an app-origin
// frame is present), so it is dropped by the SCRIPT identity of the top frame.
test('drops a throw inside a self-hosted Adobe analytics script ' + '(awshome_s_code.js)', () => {
  // The throwing (top) frame is the analytics script on the app's own static CDN;
  // a deeper frame is on the app origin, so exceptionIsFirstParty would KEEP it.
  const stack =
    'Error: AWSMA: The pageview call cannot be made because the CDP request ' +
    'did not return\n' +
    '    at f (https://a0.awsstatic.com/languages/awshome_s_code.js:1:2)\n' +
    '    at g (https://docs.aws.amazon.com/assets/r/main.js:3:4)';
  // The origin filter alone keeps this stack.
  assert.equal(exceptionIsFirstParty(stack, 'https://docs.aws.amazon.com'), true);
  assert.equal(exceptionThrownInTracker(stack), true); // ...but the script identity drops it
});

test('drops GTM / GA / Pixel / Hotjar / Segment throws by top-frame identity', () => {
  const tr = (url) => exceptionThrownInTracker('Error\n    at x (' + url + ':1:1)');
  assert.equal(tr('https://cdn.app.com/assets/gtm.js'), true);
  assert.equal(tr('https://www.googletagmanager.com/gtag/js'), true);
  assert.equal(tr('https://app.com/static/analytics.js'), true);
  assert.equal(tr('https://connect.facebook.net/en_US/fbevents.js'), true);
  assert.equal(tr('https://static.hotjar.com/c/hotjar-123.js'), true);
  assert.equal(tr('https://cdn.segment.com/analytics.js/v1/abc/analytics.min.js'), true);
});

test('does NOT drop a real app bundle, even with an analytics-ish path', () => {
  // The throwing frame is the app's own bundle (BBC case) - no tracker token.
  const tr = (url) => exceptionThrownInTracker('Error\n    at x (' + url + ':1:1)');
  assert.equal(tr('https://static.files.bbci.co.uk/bundle.js'), false);
  assert.equal(tr('https://app.com/assets/app.4f2a.js'), false);
  assert.equal(tr('https://app.com/checkout/main.js'), false);
  assert.equal(exceptionThrownInTracker(''), false); // stackless -> not a tracker
});

test('drops ONLY when the analytics script is the THROWING frame, not a ' + 'bystander', () => {
  // App bundle throws (top frame), GTM merely appears deeper -> keep (real bug).
  const stack =
    "TypeError: Cannot read properties of undefined (reading 'id')\n" +
    '    at https://app.com/assets/cart.js:9:1\n' +
    '    at https://www.googletagmanager.com/gtm.js:1:1';
  assert.equal(exceptionThrownInTracker(stack), false);
});

// exceptionIsNonDeterministic: a STACKLESS network/parse rejection (fetch got an
// HTML error page) is non-reproducible and must not be a crash finding; an
// app-code JSON.parse bug WITH a stack frame is kept.
test('drops a stackless JSON-parse-got-HTML rejection (third-party fetch)', () => {
  assert.equal(
    exceptionIsNonDeterministic(`Unexpected token '<', "<!DOCTYPE "... is not valid JSON`, ''),
    true,
  );
  assert.equal(exceptionIsNonDeterministic('Failed to fetch', ''), true);
  assert.equal(exceptionIsNonDeterministic('Unexpected end of JSON input', undefined), true);
});

test('KEEPS an app-code JSON.parse bug that carries an app stack frame', () => {
  const stack =
    'SyntaxError: "x" is not valid JSON\n    at https://app.com/assets/parse.' + 'js:2:2';
  // A stack frame prevents this path from dropping the exception.
  assert.equal(exceptionIsNonDeterministic('"x" is not valid JSON', stack), false);
});

test('does NOT drop an ordinary app crash by the non-deterministic filter', () => {
  assert.equal(
    exceptionIsNonDeterministic("Cannot read properties of null (reading 'x')", ''),
    false,
  );
});
