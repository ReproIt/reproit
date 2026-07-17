/*
 * Regression test for cloud replay action shape:
 * SDK auto-capture must keep `action` structural and put human text in `label`.
 */
var assert = require('assert');
var fs = require('fs');
var path = require('path');
var ReproIt = require('../reproit-web.js');

function fakeEl(attrs) {
  return {
    getAttribute: function (k) {
      return attrs[k] || '';
    },
  };
}

assert.strictEqual(ReproIt._actionKeyOf(fakeEl({ 'data-testid': 'pay' })), 'key:testid:pay');
assert.strictEqual(ReproIt._actionKeyOf(fakeEl({ id: 'submit' })), 'key:id:submit');
assert.strictEqual(ReproIt._actionKeyOf(fakeEl({ name: 'coupon' })), 'key:name:coupon');

var root = path.join(__dirname, '..');
var files = [
  'reproit-web.js',
  'reproit-android/src/main/kotlin/com/reproit/android/Engine.kt',
  'reproit-ios/Sources/ReproIt/Capture.swift',
  'reproit-ios/Sources/ReproIt/CaptureAppKit.swift',
  'reproit-react-native/src/index.ts',
  'reproit_flutter/lib/reproit_flutter.dart',
  'reproit-windows/src/ReproIt.Core/Engine.cs',
];

var forbidden = [
  /pendingAction\s*=\s*[^;\n]*["'`]tap:\$\{/,
  /pendingAction\s*=\s*[^;\n]*["']tap:["']\s*\+\s*label/,
  /_pendingAction\s*=\s*[^;\n]*["']tap:["']\s*\+\s*label/,
  /_pendingAction\s*=\s*[^;\n]*['"]tap:\$label/,
  /setPendingAction\(label\.map\s*\{\s*"tap:/,
  /NoteTap\(string label\)/,
];

for (var i = 0; i < files.length; i++) {
  var rel = files[i];
  var src = fs.readFileSync(path.join(root, rel), 'utf8');
  for (var j = 0; j < forbidden.length; j++) {
    assert.ok(!forbidden[j].test(src), rel + ' reintroduced label-as-action capture');
  }
}

var webSource = fs.readFileSync(path.join(root, 'reproit-web.js'), 'utf8');
assert.ok(
  /self\._observe\(self\._pending \|\| ["']nav["']\)/.test(webSource),
  'web navigation must preserve the structural click that triggered it',
);
assert.ok(
  /addEventListener\(["']hashchange["'], observeNavigation\)/.test(webSource),
  'hash-router navigation must be captured',
);

console.log('PASS: SDK action contract keeps replay structural and labels ' + 'display-only');
