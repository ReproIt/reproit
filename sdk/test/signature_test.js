/*!
 * Canonical structural-signature PARITY test for reproit-web.js.
 *
 * This is the web mirror of the Rust parity gate
 * (crates/reproit/src/model/signature.rs::tests::golden_vectors_match) and the
 * RN/Flutter equivalents. It LOADS signature_vectors.json and, for every
 * vector, asserts signatureOf(anchor, tree) === expected_sig. The vector `tree`
 * is already in the canonical Node form the SDK hashes, so we feed it straight
 * in: this proves the SDK's descriptor is byte-identical to the Rust oracle.
 *
 * No framework: run with `node test/signature_test.js` from the sdk/ directory.
 * reproit-web.js loads as a CommonJS module (the signature core is pure, no DOM).
 */
"use strict";
var assert = require("assert");
var path = require("path");
var fs = require("fs");
var ReproIt = require("../reproit-web.js");

var signatureOf = ReproIt.signatureOf;
var descriptorOf = ReproIt.descriptorOf;

// signature_vectors.json lives at the repo root: <repo>/sdk/test -> ../../
var vectorsPath = path.join(__dirname, "..", "..", "signature_vectors.json");
var vectors = JSON.parse(fs.readFileSync(vectorsPath, "utf8"));

var tests = 0;
var failures = 0;
function check(name, fn) {
  try {
    fn();
    tests++;
    console.log("ok - " + name);
  } catch (e) {
    failures++;
    console.log("NOT OK - " + name);
    console.log("    " + (e && e.message ? e.message : e));
  }
}

// ---- THE parity gate: every golden vector must match the oracle -----------
// 25 vectors total: 15 structural/anchor + 9 value-state (Layer 2) + 1 non-ASCII
// Unicode vector. Every one must hash byte-for-byte to the oracle, INCLUDING the
// Unicode vector (which only passes once the hash folds UTF-8 bytes and the V:
// section sorts by UTF-8 byte order).
assert.ok(vectors.length >= 25, "need >= 25 golden vectors, got " + vectors.length);
var valueVectors = vectors.filter(function (v) { return /^VALUE:/.test(v.description); });
assert.ok(valueVectors.length >= 9, "need >= 9 value-state vectors, got " + valueVectors.length);

vectors.forEach(function (v) {
  check("golden vector: " + v.description.slice(0, 60), function () {
    var got = signatureOf(v.anchor == null ? null : v.anchor, v.tree);
    assert.strictEqual(
      got,
      v.expected_sig,
      "mismatch.\n    descriptor = " +
        JSON.stringify(descriptorOf(v.anchor == null ? null : v.anchor, v.tree)) +
        "\n    expected " + v.expected_sig + " got " + got
    );
  });
});

// ---- cross-vector relationships the spec promises -------------------------
function sigByNeedle(needle) {
  var v = vectors.find(function (x) { return x.description.indexOf(needle) >= 0; });
  if (!v) throw new Error("no vector matching " + JSON.stringify(needle));
  return v.expected_sig;
}

check("text-exclusion + transient-drop collapse to basic login", function () {
  var login = sigByNeedle("basic login");
  assert.strictEqual(login, sigByNeedle("locale-invariance"));
  assert.strictEqual(login, sigByNeedle("transient-drop (spinner)"));
  assert.strictEqual(login, sigByNeedle("transient-drop (snackbar"));
});

check("repeated-collapse drops the count (3 == 5)", function () {
  assert.strictEqual(
    sigByNeedle("repeated-collapse (3 items)"),
    sigByNeedle("repeated-collapse (5 items")
  );
});

check("discriminators split (type, icon)", function () {
  var login = sigByNeedle("basic login");
  assert.notStrictEqual(login, sigByNeedle("collision-fix via input type"));
  assert.notStrictEqual(login, sigByNeedle("collision-fix via icon"));
  assert.notStrictEqual(
    sigByNeedle("collision-fix via input type"),
    sigByNeedle("collision-fix via icon")
  );
});

check("anchor semantics (route is part of identity)", function () {
  var settings = sigByNeedle("same route + same structure");
  assert.notStrictEqual(settings, sigByNeedle("different route + same structure"));
  assert.notStrictEqual(settings, sigByNeedle("same route + different structure"));
  assert.strictEqual(
    sigByNeedle("parameterized route (item 42)"),
    sigByNeedle("parameterized route (item 99)")
  );
});

// ---- FNV-1a known values + descriptor shape -------------------------------
check("empty anchor still has the A: prefix line", function () {
  assert.strictEqual(descriptorOf(null, { role: "screen" }), "A:\n0:screen");
  assert.strictEqual(descriptorOf("", { role: "screen" }), "A:\n0:screen");
});

check("unknown role normalizes to node", function () {
  assert.strictEqual(descriptorOf(null, { role: "carousel" }), "A:\n0:node");
});

check("token field order is :type #icon @id then *", function () {
  assert.strictEqual(
    descriptorOf(null, { role: "textfield", type: "password", icon: "lock", id: "pwd" }),
    "A:\n0:textfield:password#lock@pwd"
  );
});

// ---- Layer 2: value-state (the V: section is canonical) -------------------
check("value-state EMPTY / ZERO / POS1 are three distinct states", function () {
  var empty = sigByNeedle("empty value-class");
  var zero = sigByNeedle("zero value-class");
  var pos1 = sigByNeedle("POS1 value-class");
  assert.notStrictEqual(empty, zero);
  assert.notStrictEqual(empty, pos1);
  assert.notStrictEqual(zero, pos1);
});

check("numeric counter 0 vs 5 -> ZERO vs POS1 distinct", function () {
  assert.notStrictEqual(sigByNeedle("counter at 0"), sigByNeedle("counter at 5"));
});

check("two POS1 values (3 and 7) bucket the same", function () {
  assert.strictEqual(
    sigByNeedle("two different POS1 values bucket the same (3)"),
    sigByNeedle("two different POS1 values bucket the same (7)")
  );
});

check("chrome label with a value is NOT value-bearing", function () {
  // The chrome-label vector must hash identically to the same structure with no
  // value at all, proving chrome roles never emit a V: section.
  var withValue = sigByNeedle("chrome label with text");
  var noValue = signatureOf("/home", { role: "screen", children: [{ role: "header", id: "title" }] });
  assert.strictEqual(withValue, noValue);
});

check("grouped/locale number is locale-safe (NONEMPTY), distinct from numerics", function () {
  var grouped = sigByNeedle("grouped/locale number");
  assert.notStrictEqual(grouped, sigByNeedle("POS1 value-class"));
  assert.notStrictEqual(grouped, sigByNeedle("zero value-class"));
});

check("V: section appended only for value-bearing nodes (byte-identical else)", function () {
  // A textfield WITHOUT a value -> no V: section (pre-value-state descriptor).
  assert.strictEqual(
    descriptorOf(null, { role: "textfield", id: "email" }),
    "A:\n0:textfield@email"
  );
  // WITH a value -> a V: section is appended (status normalizes to node in body).
  assert.strictEqual(
    descriptorOf(null, { role: "status", id: "count", value: "5" }),
    "A:\n0:node@count\nV:key:count=POS1"
  );
});

check("keyless value nodes collapse in body but keep distinct V: by index", function () {
  assert.strictEqual(
    descriptorOf(null, {
      role: "screen",
      children: [
        { role: "textfield", value: "3" },
        { role: "textfield", value: "99" },
      ],
    }),
    "A:\n0:screen;1:textfield*\nV:role:textfield#0=POS1;role:textfield#1=POS2"
  );
});

check("opt-in value_node folds a chrome node's value-class into V:", function () {
  // A `text` role is chrome: even with a value it emits no V: section...
  assert.strictEqual(
    descriptorOf(null, { role: "text", id: "display", value: "42" }),
    "A:\n0:text@display"
  );
  // ...unless explicitly flagged via value_node (Layer 3 opt-in).
  assert.strictEqual(
    descriptorOf(null, { role: "text", id: "display", value: "42", value_node: true }),
    "A:\n0:text@display\nV:key:display=POS2"
  );
});

check("value_class buckets match the oracle exactly", function () {
  var vc = ReproIt.valueClass;
  var cases = [
    ["", "EMPTY"], ["   ", "EMPTY"], ["0", "ZERO"], ["0.0", "ZERO"], ["-0", "ZERO"],
    ["-3", "NEG"], ["-0.5", "NEG"], ["3", "POS1"], ["9.99", "POS1"], ["+7", "POS1"],
    ["10", "POS2"], ["99", "POS2"], ["100", "POS3"], ["999.99", "POS3"],
    ["1000", "POSL"], ["123456", "POSL"], ["  42  ", "POS2"],
    ["1,234", "NONEMPTY"], ["1.234.567", "NONEMPTY"], ["1 234", "NONEMPTY"],
    ["$5", "NONEMPTY"], ["5%", "NONEMPTY"], ["1e3", "NONEMPTY"], ["0x10", "NONEMPTY"],
    [".", "NONEMPTY"], ["3.", "NONEMPTY"], [".5", "NONEMPTY"], ["--5", "NONEMPTY"],
    ["hello", "NONEMPTY"], ["١٢٣", "NONEMPTY"],
  ];
  cases.forEach(function (c) {
    assert.strictEqual(vc(c[0]), c[1], "value_class(" + JSON.stringify(c[0]) + ")");
  });
});

console.log("\n" + tests + " passed, " + failures + " failed");
process.exit(failures ? 1 : 0);
