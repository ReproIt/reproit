/*
 * Regression test for the cross-SDK oracle contract:
 * every production SDK's error event must carry an `oracle` tag so the cloud
 * can gate ingest on oracle-grade findings. A genuine uncaught error / native
 * crash / fatal signal IS the `crash` oracle firing, so its error event ships
 * `oracle: "crash"` right next to `kind: "error"`. This scans each SDK's
 * error-emit source so a future SDK cannot regress to untagged errors.
 */
var assert = require('assert');
var fs = require('fs');
var path = require('path');

var root = path.join(__dirname, '..');

// Each SDK builds the error event in one place; the emit is the single funnel
// for uncaught errors / signals, so tagging it once covers every crash path.
var sources = [
  ['reproit-web.js', 'web (also Electron/Tauri)'],
  ['reproit-android/src/main/kotlin/com/reproit/android/Engine.kt', 'Android'],
  ['reproit-ios/Sources/ReproIt/Core.swift', 'iOS'],
  ['reproit-react-native/src/index.ts', 'React Native'],
  ['reproit_flutter/lib/reproit_flutter.dart', 'Flutter'],
  ['reproit-windows/src/ReproIt.Core/Engine.cs', 'Windows'],
  ['reproit-linux/reproit_linux/reporter.py', 'Linux (Python)'],
];

// `kind: "error"` (in any language's quote/assignment idiom) followed, within a
// short window that tolerates an intervening comment, by `oracle: "crash"`. The
// window keeps the two tied to the SAME event, not merely both present in file.
var taggedError = new RegExp(
  '["\']?kind["\']?\\]?\\s*[:=]\\s*\\[?\\s*["\']error["\'][\\s\\S]{0,400}?["\']' +
    '?oracle["\']?\\]?\\s*[:=]\\s*["\']crash["\']',
  '',
);

// A bare error event with no adjacent oracle tag is the regression we forbid.
var kindError = /["']?kind["']?\]?\s*[:=]\s*\[?\s*["']error["']/;

for (var i = 0; i < sources.length; i++) {
  var rel = sources[i][0];
  var label = sources[i][1];
  var src = fs.readFileSync(path.join(root, rel), 'utf8');
  assert.ok(kindError.test(src), label + ' (' + rel + '): expected an error event emit to scan');
  assert.ok(
    taggedError.test(src),
    label +
      ' (' +
      rel +
      '): error event is missing an adjacent `oracle` tag ' +
      '(expected `oracle: "crash"` next to `kind: "error"`)',
  );
}

console.log('PASS: every production SDK tags its error event with the `crash` ' + 'oracle');

// The backend SDKs' production capture mode is the backend counterpart of the
// same contract: their 5xx finding frame must carry the first-class
// `backend-server-error` registry id so ingest's oracle gate accepts it. All
// three ports (Rust reference, Node, Python) are pinned identically.
var backendSrc = fs.readFileSync(path.join(root, 'reproit-backend-rs/src/capture.rs'), 'utf8');
assert.ok(
  /SERVER_ERROR_ORACLE:\s*&str\s*=\s*"backend-server-error"/.test(backendSrc),
  'backend-rs capture: expected the backend-server-error oracle id constant',
);
assert.ok(
  /"kind":\s*"finding"[\s\S]{0,400}?"oracle":\s*SERVER_ERROR_ORACLE/.test(backendSrc),
  'backend-rs capture: finding identity is missing the `backend-server-error` oracle tag',
);

var backendPorts = [
  ['reproit-backend-node/capture.js', 'Node backend'],
  ['reproit-backend-py/reproit_backend_py/capture.py', 'Python backend'],
];
var portConstant = /SERVER_ERROR_ORACLE\s*=\s*["']backend-server-error["']/;
var portTaggedFinding =
  /["']?kind["']?\s*[:=]\s*["']finding["'][\s\S]{0,500}?["']?oracle["']?\s*:\s*SERVER_ERROR_ORACLE/;
for (var j = 0; j < backendPorts.length; j++) {
  var portRel = backendPorts[j][0];
  var portLabel = backendPorts[j][1];
  var portSrc = fs.readFileSync(path.join(root, portRel), 'utf8');
  assert.ok(
    portConstant.test(portSrc),
    portLabel + ' (' + portRel + '): expected the backend-server-error oracle id constant',
  );
  assert.ok(
    portTaggedFinding.test(portSrc),
    portLabel +
      ' (' +
      portRel +
      '): finding identity is missing the `backend-server-error` oracle tag',
  );
}

console.log(
  'PASS: every backend SDK (Rust, Node, Python) tags its capture finding with ' +
    '`backend-server-error`',
);
