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
  if (rel === 'reproit_flutter/lib/reproit_flutter.dart') {
    src += fs.readFileSync(
      path.join(root, 'reproit_flutter/lib/src/runtime.dart'),
      'utf8',
    );
  }
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

// Every backend uploader now emits the same source-neutral observation event.
// Language-specific syntax differs, so pin both the stable constant and the
// causal schema vocabulary in each implementation.
var backendPorts = [
  ['reproit-backend-rs/src/capture.rs', 'Rust backend', /SERVER_ERROR_ORACLE/],
  ['reproit-backend-node/capture.js', 'Node backend', /SERVER_ERROR_ORACLE/],
  ['reproit-backend-py/reproit_backend_py/capture.py', 'Python backend', /SERVER_ERROR_ORACLE/],
  ['reproit-backend-go/capture.go', 'Go backend', /ServerErrorOracle/],
  ['reproit-backend-rb/lib/reproit_backend_rb/capture.rb', 'Ruby backend', /SERVER_ERROR_ORACLE/],
  ['reproit-backend-php/capture.php', 'PHP backend', /SERVER_ERROR_ORACLE/],
  [
    'reproit-backend-java/src/main/java/dev/reproit/backend/Capture.java',
    'Java backend',
    /SERVER_ERROR_ORACLE/,
  ],
  ['reproit-backend-dotnet/ReproitBackend/Capture.cs', '.NET backend', /ServerErrorOracle/],
];
for (var j = 0; j < backendPorts.length; j++) {
  var portRel = backendPorts[j][0];
  var portLabel = backendPorts[j][1];
  var portSrc = fs.readFileSync(path.join(root, portRel), 'utf8');
  assert.ok(
    /backend-server-error/.test(portSrc),
    portLabel + ' (' + portRel + '): expected the backend-server-error identity',
  );
  assert.ok(
    backendPorts[j][2].test(portSrc) &&
      /observation/.test(portSrc) &&
      /(signature|Signature)/.test(portSrc),
    portLabel + ' (' + portRel + '): missing causal observation signature',
  );
}

console.log(
  'PASS: every backend SDK (Rust, Node, Python, Go, Ruby, PHP, Java, .NET) preserves ' +
    'the `backend-server-error` identity',
);
