/*!
 * Host-runnable unit test for reproit-web's developer-provided BUILD identity.
 *
 * No test framework required: run with `node test/build_test.js` from the sdk/
 * directory. Covers the pure normalizeBuild bucketer and the end-to-end flush:
 * init({ build }) -> each finding frame carries context.build =
 * { version, commit } (only the provided fields); init without build still
 * carries safe environment context. Mirrors the
 * RN SDK's build.test.ts. The cloud reads context.build.version/.commit to
 * segment bugs by build (regressed in / resolved since).
 */
'use strict';
var assert = require('assert');
var ReproIt = require('../reproit-web.js');

var tests = 0;
function check(name, fn) {
  fn();
  tests++;
  console.log('ok - ' + name);
}

// ---- normalizeBuild (pure) ------------------------------------------------

check('normalizeBuild keeps both provided fields', function () {
  assert.deepStrictEqual(ReproIt.normalizeBuild({ version: '1.4.2', commit: 'abc123' }), {
    version: '1.4.2',
    commit: 'abc123',
  });
});

check('normalizeBuild keeps only version', function () {
  assert.deepStrictEqual(ReproIt.normalizeBuild({ version: '1.4.2' }), { version: '1.4.2' });
});

check('normalizeBuild keeps only commit', function () {
  assert.deepStrictEqual(ReproIt.normalizeBuild({ commit: 'abc123' }), { commit: 'abc123' });
});

check('normalizeBuild returns null for absent / empty / non-string', function () {
  assert.strictEqual(ReproIt.normalizeBuild(null), null);
  assert.strictEqual(ReproIt.normalizeBuild(undefined), null);
  assert.strictEqual(ReproIt.normalizeBuild({}), null);
  assert.strictEqual(ReproIt.normalizeBuild({ version: '', commit: '' }), null);
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
    visibilityState: 'visible',
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
    ReproIt._batchSequence = 0;
    if (ReproIt._timer) clearInterval(ReproIt._timer);
  }
}

function seedFinding() {
  ReproIt._buf.push({
    kind: 'error',
    oracle: 'crash',
    sig: 'deadbeef',
    message: 'boom',
    path: [],
    t: 1,
  });
}

check('init WITH build -> finding context has version and commit', function () {
  withStubs(function (sent) {
    ReproIt.init({
      appId: 'app',
      endpoint: 'https://ingest.example/v1/events',
      build: { version: '1.4.2', commit: 'abc123' },
    });
    seedFinding();
    ReproIt._flush();
    assert.strictEqual(sent.length, 1);
    assert.strictEqual(sent[0].version, 1);
    assert.deepStrictEqual(sent[0].deployment, {
      version: '1.4.2',
      commit: 'abc123',
    });
    assert.deepStrictEqual(sent[0].frames[0].event.context.build, {
      version: '1.4.2',
      commit: 'abc123',
    });
    assert.strictEqual(sent[0].frames[0].event.context.platform, 'web');
  });
});

check('init WITH only-version -> finding context has version only', function () {
  withStubs(function (sent) {
    ReproIt.init({
      appId: 'app',
      endpoint: 'https://ingest.example/v1/events',
      build: { version: '9.9.9' },
    });
    seedFinding();
    ReproIt._flush();
    assert.deepStrictEqual(sent[0].frames[0].event.context.build, { version: '9.9.9' });
  });
});

check('init WITHOUT build -> safe environment context, no build', function () {
  withStubs(function (sent) {
    ReproIt.init({ appId: 'app', endpoint: 'https://ingest.example/v1/events' });
    seedFinding();
    ReproIt._flush();
    assert.strictEqual(sent.length, 1);
    assert.strictEqual(sent[0].frames[0].event.context.platform, 'web');
    assert.strictEqual(sent[0].frames[0].event.context.build, undefined);
    assert.strictEqual(sent[0].deployment, undefined);
  });
});

check('protocol batch maps graph edges to scoped frames', function () {
  var batch = ReproIt.protocolBatch(
    'app',
    [{ kind: 'edge', from: 'a', action: 'tap', to: 'b' }],
    {},
    42,
    3,
  );
  assert.deepStrictEqual(batch, {
    version: 1,
    batchId: 'sdk-42-3',
    appId: 'app',
    frames: [
      {
        runId: 'sdk-42-3',
        sequence: 1,
        scope: { domain: 'shared' },
        event: { kind: 'graph-edge', from: 'a', action: 'tap', to: 'b' },
      },
    ],
    evidence: [],
  });
});

console.log('\n' + tests + ' tests passed');
