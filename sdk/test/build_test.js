/*!
 * Host-runnable unit test for reproit-web's developer-provided BUILD identity.
 *
 * No test framework required: run with `node test/build_test.js` from the sdk/
 * directory. Covers the pure normalizeBuild bucketer and the end-to-end flush:
 * init({ build }) -> the batch carries context.build = { version, commit } (only
 * the provided fields); init without build -> no ctx (back-compat). Mirrors the
 * RN SDK's build.test.ts. The cloud reads context.build.version/.commit to
 * segment bugs by build (regressed in / resolved since).
 */
"use strict";
var assert = require("assert");
var ReproIt = require("../reproit-web.js");

var tests = 0;
function check(name, fn) {
  fn();
  tests++;
  console.log("ok - " + name);
}

// ---- normalizeBuild (pure) ------------------------------------------------

check("normalizeBuild keeps both provided fields", function () {
  assert.deepStrictEqual(ReproIt.normalizeBuild({ version: "1.4.2", commit: "abc123" }), {
    version: "1.4.2",
    commit: "abc123",
  });
});

check("normalizeBuild keeps only version", function () {
  assert.deepStrictEqual(ReproIt.normalizeBuild({ version: "1.4.2" }), { version: "1.4.2" });
});

check("normalizeBuild keeps only commit", function () {
  assert.deepStrictEqual(ReproIt.normalizeBuild({ commit: "abc123" }), { commit: "abc123" });
});

check("normalizeBuild returns null for absent / empty / non-string", function () {
  assert.strictEqual(ReproIt.normalizeBuild(null), null);
  assert.strictEqual(ReproIt.normalizeBuild(undefined), null);
  assert.strictEqual(ReproIt.normalizeBuild({}), null);
  assert.strictEqual(ReproIt.normalizeBuild({ version: "", commit: "" }), null);
  assert.strictEqual(ReproIt.normalizeBuild({ version: 42 }), null);
});

// ---- end-to-end flush: context.build rides the batch ----------------------
// Minimal DOM + fetch stubs so init()/_flush() run headlessly in node. We don't
// exercise the DOM walk here (signature_test.js owns that); we feed one event in
// directly and assert the batch shape the cloud reads.

function withStubs(run) {
  var sent = [];
  global.document = {
    body: null,
    documentElement: null,
    visibilityState: "visible",
    querySelectorAll: function () {
      return [];
    },
  };
  global.addEventListener = function () {};
  global.fetch = function (url, opts) {
    sent.push(JSON.parse(opts.body));
    return { catch: function () {} };
  };
  global.history = { pushState: function () {}, replaceState: function () {} };
  try {
    run(sent);
  } finally {
    delete global.document;
    delete global.addEventListener;
    delete global.fetch;
    delete global.history;
    // Reset the singleton so the next case starts clean.
    ReproIt._on = false;
    ReproIt._cfg = null;
    ReproIt._buf = [];
    ReproIt._cur = null;
    ReproIt._path = [];
    ReproIt._build = null;
    if (ReproIt._timer) clearInterval(ReproIt._timer);
  }
}

check("init WITH build -> batch.ctx.build = { version, commit }", function () {
  withStubs(function (sent) {
    ReproIt.init({
      appId: "app",
      endpoint: "https://ingest.example/v1/events",
      build: { version: "1.4.2", commit: "abc123" },
    });
    // Seed one event and flush.
    ReproIt._buf.push({ kind: "edge", action: "load", to: "deadbeef", t: 1 });
    ReproIt._flush();
    assert.strictEqual(sent.length, 1);
    assert.deepStrictEqual(sent[0].ctx, { build: { version: "1.4.2", commit: "abc123" } });
  });
});

check("init WITH only-version -> batch.ctx.build has version, no commit", function () {
  withStubs(function (sent) {
    ReproIt.init({ appId: "app", endpoint: "https://ingest.example/v1/events", build: { version: "9.9.9" } });
    ReproIt._buf.push({ kind: "edge", action: "load", to: "deadbeef", t: 1 });
    ReproIt._flush();
    assert.deepStrictEqual(sent[0].ctx, { build: { version: "9.9.9" } });
  });
});

check("init WITHOUT build -> no ctx (back-compat, today's behavior)", function () {
  withStubs(function (sent) {
    ReproIt.init({ appId: "app", endpoint: "https://ingest.example/v1/events" });
    ReproIt._buf.push({ kind: "edge", action: "load", to: "deadbeef", t: 1 });
    ReproIt._flush();
    assert.strictEqual(sent.length, 1);
    assert.strictEqual(sent[0].ctx, undefined);
  });
});

console.log("\n" + tests + " tests passed");
