// ReproIt Tauri runner. Tauri renders in the system webview (WKWebView /
// WebView2 / WebKitGTK), which is driven over the W3C WebDriver protocol by
// `tauri-driver`. We connect with webdriverio and run the SAME DOM a11y
// snapshot as the web/Electron runners via browser.execute(), producing the
// SAME CANONICAL structural signature; only the transport differs (WebDriver
// instead of CDP). The webview builds a canonical Node tree, returns it to the
// host, and the host hashes it with the byte-identical canonical pipeline.
//
// Prereqs (on the host): `tauri-driver` and the platform webdriver
// (msedgedriver / WebKitWebDriver) on PATH. Start `tauri-driver` first, or set
// REPROIT_WEBDRIVER_URL to a running endpoint.
//
// Env: REPROIT_APP (built Tauri binary), REPROIT_FUZZ_CONFIG, REPROIT_WEBDRIVER_URL.
// Status: validated end-to-end against a real Tauri v2 Linux app via
// tauri-driver + WebKitWebDriver under Xvfb (Ubuntu 24.04 in Docker).

// webdriverio is imported dynamically inside main() so this module stays
// import-safe (the parity test imports the host-pure signature functions
// below without needing the heavy runtime dependency installed).
import { readFileSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve as resolvePath, join as joinPath } from 'node:path';
// Canonical signature + scenario plumbing shared with the Electron runner.
// The specifiers are OUTPUT-relative (this file ships as runners/tauri.mjs
// next to shared/), like the './web/*.mjs' oracle imports.
import {
  signatureOf,
  descriptorOf,
  valueClass,
  fnv1a,
  loadValueNodes,
} from './shared/signature.mjs';
import { loadFuzz, rng, INJECTED_VALUES, expandEnv } from './shared/fuzz.mjs';
import { execFileSync, spawn } from 'node:child_process';
import { platform as osPlatform } from 'node:os';
// CHOICE-ANOMALY oracle, shared with the web + electron runners. We inject the
// SAME self-contained in-page pass into the webview via executeAsync() (the way
// every other oracle is injected on Tauri, which has no CDP); it works over WebKit
// or WebView2 alike because it only touches the live DOM + layout. The constants
// are the single source of truth for the outlier thresholds. Host-pure +
// dependency-free, so a static import keeps this module import-safe for the parity
// test (it imports the signature functions without the webdriverio runtime).
import {
  CHOICE_ANOMALY_IN_PAGE_SRC,
  CHOICE_OUTLIER_RATIO,
  CHOICE_MIN_MAGNITUDE,
  CHOICE_ROLES,
} from './web/choice-oracle.mjs';
import {
  occlusionScan,
  confirmOcclusions,
  securityScan,
  focusLossArm,
  focusLossCheck,
  blankScreenScan,
  brokenAssetScan,
  zoomTappableKeys,
  zoomReflowScan,
  scrollRoundTripScan,
} from './web/hygiene-oracles.mjs';
import { layoutOverflowScan, confirmLayoutOverflow } from './web/overflow-oracle.mjs';
import { zeroContrastScan } from './web/zero-contrast-oracle.mjs';
import { inspectPlatformStep } from './inspect-control.mjs';

// Hygiene oracles NOT ported to this runner, deliberately (no probe beats a
// wrong finding):
//   - duplicate-submit: the probe attributes first-party non-GET requests via
//     a DRIVER-level request event (Playwright page.on('request')). WebDriver
//     has no request stream, and in-page patching (fetch/XHR wrappers) cannot
//     see a plain form POST or reliably attribute a request to the probe's
//     double-click window, so a port here would guess.

// The choice-anomaly pass as an executeAsync() body. WebDriver executeAsync passes
// a `done` callback as the FINAL argument; the choice pass is async (it waits for
// layout to settle between options), so we run it then hand its findings to done.
// Built from CHOICE_ANOMALY_IN_PAGE_SRC (the exact function unit-tested via the web
// runner's page.evaluate) so there is no second copy to drift. The thresholds are
// interpolated from the shared constants.
const CHOICE_ANOMALY_ASYNC_JS = `
  var __reproitChoiceFn = ${CHOICE_ANOMALY_IN_PAGE_SRC};
  var __reproitDone = arguments[arguments.length - 1];
  __reproitChoiceFn({
    settleMs: 600,
    ratio: ${CHOICE_OUTLIER_RATIO},
    minMag: ${CHOICE_MIN_MAGNITUDE},
    choiceRoles: ${JSON.stringify(CHOICE_ROLES)},
  }).then(function (findings) { __reproitDone(findings || []); })
    .catch(function () { __reproitDone([]); });
`;

// The scroll-round-trip scan as an executeAsync() body. scrollRoundTripScan is
// async (it awaits animation frames between the away/back jumps so virtualization
// settles), so the webview runs it via executeAsync and hands its findings to the
// done callback. Built from the shared function's source so there is no second
// copy to drift.
const SCROLLROUNDTRIP_ASYNC_JS = `
  var __srtFn = ${scrollRoundTripScan.toString()};
  var __srtDone = arguments[arguments.length - 1];
  Promise.resolve(__srtFn()).then(function (items) { __srtDone(items || []); })
    .catch(function () { __srtDone([]); });
`;

const APP = process.env.REPROIT_APP;
const WD_URL = process.env.REPROIT_WEBDRIVER_URL || 'http://127.0.0.1:4444';
const VIDEO_DIR = process.env.REPROIT_VIDEO_DIR || undefined;
// Probe mode (REPROIT_PROBE=1): the web tier's destructive probe pass. This
// runner has no probe of its own, but the flag still gates the window-resizing
// zoom-reflow check below, matching the web runner's guard.
const PROBE = process.env.REPROIT_PROBE === '1';
const ACTION_BUDGET = 36;
const MAX_LABEL_LEN = 40;
// Layer-1 value-class cap (docs/signature.md "Value-state"): once a structural
// node has shown more than this many DISTINCT value-class combinations, the
// runner drops it to structural-only so an adversarial value generator cannot
// explode the graph. The oracle is stateless; the cap is purely runner-local.
const VALUE_CLASS_CAP = 8;

function log(line) {
  process.stdout.write(line + '\n');
}

// Screenshot-capture contract (drive.rs): on a named "shoot" point, capture the
// current webview to $REPROIT_SHOTS_DIR/<name>.png, then print `SHOOT:<name>` so
// the orchestrator confirms the file and logs it. `name` is restricted to
// [A-Za-z0-9_/-] (the orchestrator filters to those anyway). Capture is the W3C
// WebDriver "Take Screenshot" command (browser.takeScreenshot in webdriverio),
// which returns the PNG as base64; we write those bytes to the path. If
// REPROIT_SHOTS_DIR is unset we skip the capture but STILL print the marker, so
// non-screenshot runs are unaffected.
async function shoot(browser, name) {
  const dir = process.env.REPROIT_SHOTS_DIR;
  if (dir) {
    try {
      mkdirSync(dir, { recursive: true });
      const b64 = await browser.takeScreenshot();
      writeFileSync(joinPath(dir, name + '.png'), Buffer.from(b64, 'base64'));
    } catch (e) {
      /* capture is best-effort; still emit the marker below */
    }
  }
  log('SHOOT:' + name);
}

export { signatureOf, descriptorOf, valueClass };

// The DOM walk runs INSIDE the webview via execute(); identical canonical
// DOM->Node logic to runners/web/runner.mjs. It returns a canonical Node tree
// (role + id + type + icon + children) plus display-only labels and the
// structural selectors for each tappable. ALL user-facing text is excluded from
// the tree; visible text is kept only as a display label for `map show`.
// Elements are addressed by stable selector preference
// (data-testid > id > name > aria-role + structural index); a tappable lacking
// any stable id falls back to role+index and is flagged `nokey`. The host then
// hashes the tree with the canonical signature, byte-identical to the oracle.
import { snapshotJs } from './tauri-snapshot.mjs';

async function snapshot(browser, valueNodeSelectors) {
