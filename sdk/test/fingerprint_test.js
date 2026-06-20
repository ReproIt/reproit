/*!
 * Host-runnable unit test for reproit-web's PII-safe fingerprintValue.
 *
 * No test framework required: run with `node test/fingerprint_test.js` from the
 * sdk/ directory. Loads reproit-web.js as a CommonJS module (it has no DOM
 * dependency for the pure function) and asserts the fingerprint FEATURES, never
 * raw values. Mirrors the parity cases in the other four SDKs.
 */
"use strict";
var assert = require("assert");
var ReproIt = require("../reproit-web.js");
var fp = ReproIt.fingerprintValue;

var tests = 0;
function check(name, fn) {
  fn();
  tests++;
  console.log("ok - " + name);
}

check("Jose-emoji counts code points and is unicode+emoji", function () {
  var r = fp("José🎉"); // "José🎉"
  assert.strictEqual(r.len, 5, "code-point length is 5");
  assert.strictEqual(r.charset, "unicode");
  assert.strictEqual(r.hasEmoji, true);
  assert.strictEqual(r.isEmpty, false);
  assert.strictEqual(r.isRtl, false);
});

check("numeric string", function () {
  var r = fp("12345");
  assert.strictEqual(r.len, 5);
  assert.strictEqual(r.charset, "numeric");
  assert.strictEqual(r.hasEmoji, false);
  assert.strictEqual(r.isEmpty, false);
});

check("ascii word", function () {
  var r = fp("hello");
  assert.strictEqual(r.charset, "ascii");
  assert.strictEqual(r.len, 5);
  assert.strictEqual(r.isRtl, false);
});

check("empty string", function () {
  var r = fp("");
  assert.strictEqual(r.isEmpty, true);
  assert.strictEqual(r.len, 0);
  // empty is not numeric (no digits) -> falls to ascii
  assert.strictEqual(r.charset, "ascii");
});

check("whitespace-only is empty", function () {
  var r = fp("   ");
  assert.strictEqual(r.isEmpty, true);
});

check("Arabic string is RTL + unicode", function () {
  var r = fp("مرحبا"); // "مرحبا"
  assert.strictEqual(r.isRtl, true);
  assert.strictEqual(r.charset, "unicode");
  assert.strictEqual(r.isEmpty, false);
  assert.strictEqual(r.hasEmoji, false);
});

check("Hebrew string is RTL", function () {
  var r = fp("שלום"); // "שלום"
  assert.strictEqual(r.isRtl, true);
});

check("Turkish dotless i is unicode not ascii", function () {
  var r = fp("ıstanbul"); // "ıstanbul"
  assert.strictEqual(r.charset, "unicode");
  assert.strictEqual(r.isRtl, false);
});

check("312-char name reports exact length", function () {
  var r = fp("a".repeat(312));
  assert.strictEqual(r.len, 312);
  assert.strictEqual(r.charset, "ascii");
});

check("null/undefined treated as empty", function () {
  assert.strictEqual(fp(null).isEmpty, true);
  assert.strictEqual(fp(undefined).isEmpty, true);
});

check("fingerprint never echoes the raw value", function () {
  var raw = "secret-pii-value";
  var r = fp(raw);
  assert.ok(!JSON.stringify(r).includes(raw), "no raw value in fingerprint");
});

// ---- v2 features (bytes / scripts / combining / zero-width / newline / ws) ----

check("bytes is UTF-8 length, distinct from code-point len", function () {
  var r = fp("José\u{1f389}"); // J o s é(2B) 🎉(4B) -> 9 bytes, 5 code points
  assert.strictEqual(r.len, 5);
  assert.strictEqual(r.bytes, 9);
  assert.strictEqual(fp("hello").bytes, 5); // ascii: bytes == len
});

check("scripts lists buckets present, sorted, mixed-script", function () {
  assert.deepStrictEqual(fp("hello").scripts, ["Latin"]);
  assert.deepStrictEqual(fp("مرحبا").scripts, ["Arabic"]);
  assert.deepStrictEqual(fp("hi مرحبا").scripts, ["Arabic", "Latin"]);
  assert.deepStrictEqual(fp("日本語").scripts, ["CJK"]);
  assert.deepStrictEqual(fp("12345").scripts, []); // digits are no script
});

check("hasNewline detects LF and CR", function () {
  assert.strictEqual(fp("line1\nline2").hasNewline, true);
  assert.strictEqual(fp("a\rb").hasNewline, true);
  assert.strictEqual(fp("oneline").hasNewline, false);
});

check("hasZeroWidth detects invisible code points", function () {
  assert.strictEqual(fp("a​b").hasZeroWidth, true); // ZWSP
  assert.strictEqual(fp("a‍b").hasZeroWidth, true); // ZWJ
  assert.strictEqual(fp("ab").hasZeroWidth, false);
});

check("hasCombiningMarks detects decomposed accents", function () {
  assert.strictEqual(fp("é").hasCombiningMarks, true); // e + combining acute
  assert.strictEqual(fp("e").hasCombiningMarks, false);
  assert.strictEqual(fp("é").hasCombiningMarks, false); // precomposed é
});

check("leadingTrailingWhitespace flags edge whitespace", function () {
  assert.strictEqual(fp(" hello").leadingTrailingWhitespace, true);
  assert.strictEqual(fp("hello ").leadingTrailingWhitespace, true);
  assert.strictEqual(fp("hello").leadingTrailingWhitespace, false);
  assert.strictEqual(fp("a\tb").leadingTrailingWhitespace, false); // interior only
});

check("FP_VERSION is exported and is 2", function () {
  assert.strictEqual(ReproIt.FP_VERSION, 2);
});

console.log("\n" + tests + " tests passed");
