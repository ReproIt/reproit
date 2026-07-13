/*!
 * The production SDK is an ORACLE runner, not an error firehose: it reports only
 * high-signal findings. For the error path that means the `crash` oracle (a
 * genuine uncaught app error) is reported and tagged, while the environment /
 * third-party noise every browser emits through window.onerror is dropped AT THE
 * SOURCE. This locks the noise gate (isCrashNoise) so it can't silently regress
 * into re-ingesting junk.
 *
 * No framework: run with `node --test test/crash_noise_test.js` from sdk/.
 */
"use strict";
const { test } = require("node:test");
const assert = require("node:assert");
const ReproIt = require("../reproit-web.js");
const isNoise = ReproIt.isCrashNoise;

test("crash noise gate drops junk, keeps genuine app crashes", () => {
  // NOISE: never the app crashing, so never reported.
  const noise = [
    ["Script error.", ""], // cross-origin opaque
    ["Script error", ""],
    ["ResizeObserver loop limit exceeded", ""],
    ["ResizeObserver loop completed with undelivered notifications", ""],
    ["boom", "chrome-extension://abc/inject.js"], // extension source
    ["oops", "moz-extension://x/c.js"],
    ["Failed to fetch", "https://app.example.com/main.js"], // network flake
    ["NetworkError when attempting to fetch resource.", ""],
    ["Load failed", ""], // Safari fetch
    ["The user aborted a request.", ""],
    ["", "https://app.example.com/main.js"], // empty message: no signal
  ];
  for (const [msg, src] of noise) {
    assert.strictEqual(isNoise(msg, src), true, `should drop: ${JSON.stringify(msg)} @ ${src}`);
  }

  // GENUINE app crashes: same-origin, real message -> reported (the crash oracle).
  const real = [
    ["TypeError: Cannot read properties of null (reading 'total')", "https://app.example.com/app.js"],
    ["ReferenceError: x is not defined", "https://app.example.com/app.js"],
    ["Uncaught RangeError: Maximum call stack size exceeded", ""],
  ];
  for (const [msg, src] of real) {
    assert.strictEqual(isNoise(msg, src), false, `should keep: ${JSON.stringify(msg)}`);
  }
});
