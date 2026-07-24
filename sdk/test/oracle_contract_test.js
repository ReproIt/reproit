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

// The Go port names the constant idiomatically (ServerErrorOracle), so it is
// pinned with its own patterns against the same registry id.
var goSrc = fs.readFileSync(path.join(root, 'reproit-backend-go/capture.go'), 'utf8');
assert.ok(
  /ServerErrorOracle\s*=\s*"backend-server-error"/.test(goSrc),
  'Go backend (reproit-backend-go/capture.go): expected the backend-server-error oracle id',
);
assert.ok(
  /"kind":\s*"finding"[\s\S]{0,500}?"oracle":\s*ServerErrorOracle/.test(goSrc),
  'Go backend (reproit-backend-go/capture.go): finding identity is missing the ' +
    '`backend-server-error` oracle tag',
);

// The remaining ports each carry the same constant in their language's idiom;
// the tagged-finding window is tailored to how each builds the finding frame.
var otherPorts = [
  [
    'reproit-backend-rb/lib/reproit_backend_rb/capture.rb',
    'Ruby backend',
    /SERVER_ERROR_ORACLE\s*=\s*"backend-server-error"/,
    /"kind" => "finding"[\s\S]{0,400}?"oracle" => SERVER_ERROR_ORACLE/,
  ],
  [
    'reproit-backend-php/capture.php',
    'PHP backend',
    /const SERVER_ERROR_ORACLE\s*=\s*'backend-server-error'/,
    /'kind' => 'finding'[\s\S]{0,400}?'oracle' => SERVER_ERROR_ORACLE/,
  ],
  [
    'reproit-backend-java/src/main/java/dev/reproit/backend/Capture.java',
    'Java backend',
    /SERVER_ERROR_ORACLE\s*=\s*"backend-server-error"/,
    /identity\.put\("oracle", SERVER_ERROR_ORACLE\)[\s\S]{0,400}?finding\.put\("kind", "finding"\)/,
  ],
  [
    'reproit-backend-dotnet/ReproitBackend/Capture.cs',
    '.NET backend',
    /ServerErrorOracle\s*=\s*"backend-server-error"/,
    /\["kind"\] = "finding"[\s\S]{0,500}?\["oracle"\] = ServerErrorOracle/,
  ],
];
for (var k = 0; k < otherPorts.length; k++) {
  var oRel = otherPorts[k][0];
  var oLabel = otherPorts[k][1];
  var oSrc = fs.readFileSync(path.join(root, oRel), 'utf8');
  assert.ok(
    otherPorts[k][2].test(oSrc),
    oLabel + ' (' + oRel + '): expected the backend-server-error oracle id constant',
  );
  assert.ok(
    otherPorts[k][3].test(oSrc),
    oLabel + ' (' + oRel + '): finding identity is missing the `backend-server-error` oracle tag',
  );
}

console.log(
  'PASS: every backend SDK (Rust, Node, Python, Go, Ruby, PHP, Java, .NET) tags its ' +
    'capture finding with `backend-server-error`',
);
