/*
 * Golden-byte replay parity: Node is the wire reference, and the Python SDK
 * must produce BYTE-identical replay behavior over one fixed capsule:
 *
 *   1. the served response for a recorded SSE exchange: status, body text,
 *      and the chunk split (the stream shape the app observes);
 *   2. the served 599 body for a divergence;
 *   3. the structured REPROIT:DIVERGENCE marker line for a prompt-drift
 *      probe, including the bodyDelta naming the first differing message
 *      index. The line is compared byte for byte, so field order, separators
 *      and the delta encoding are all pinned.
 *
 * Run: node sdk/test/backend_replay_parity_test.js
 * (The Python side needs `python3` on PATH; the SDK itself is stdlib-pure.)
 */
'use strict';

var assert = require('assert');
var child_process = require('child_process');
var path = require('path');

var root = path.join(__dirname, '..');

var CAPSULE = {
  format: 'reproit-backend-capture',
  version: 2,
  operation: 'GET /quote',
  oracle: 'backend-server-error',
  events: [
    {
      kind: 'effect',
      sequence: 1,
      exchange: {
        protocol: 'http',
        request: { method: 'GET', url: 'http://llm.internal/stream' },
        response: {
          status: 200,
          headers: { 'content-type': 'text/event-stream' },
          body: 'data: a\n\ndata: b\n\ndata: c\n\n',
          stream: { chunks: [9, 9, 9] },
        },
      },
    },
    {
      kind: 'effect',
      sequence: 2,
      exchange: {
        protocol: 'http',
        request: {
          method: 'POST',
          url: 'http://llm.internal/v1/chat',
          body: {
            messages: [
              { role: 'user', content: 'hello' },
              { role: 'assistant', content: 'hi' },
              { role: 'user', content: 'weather?' },
            ],
          },
        },
        response: {
          status: 200,
          headers: { 'content-type': 'application/json' },
          body: { reply: 'sunny' },
        },
      },
    },
  ],
};

var DRIFT_PROBE_MESSAGES = [
  { role: 'user', content: 'hello' },
  { role: 'assistant', content: 'hi' },
  { role: 'user', content: 'DIFFERENT QUESTION' },
];

function nodeSide() {
  var replay = require(path.join(root, 'reproit-backend-node/replay.js'));
  var session = new replay.ReplaySession(JSON.parse(JSON.stringify(CAPSULE)));
  var served = replay.serveHttp(session, {
    method: 'GET',
    url: 'http://llm.internal/stream',
  });
  var markers = [];
  var write = process.stderr.write.bind(process.stderr);
  process.stderr.write = function (chunk) {
    markers.push(String(chunk));
    return true;
  };
  var diverged;
  try {
    diverged = replay.serveHttp(session, {
      method: 'POST',
      url: 'http://llm.internal/v1/chat',
      body: { messages: DRIFT_PROBE_MESSAGES },
    });
  } finally {
    process.stderr.write = write;
  }
  return {
    serve: {
      status: served.status,
      bodyText: served.bodyText,
      chunks: served.chunks.map(function (chunk) {
        return chunk.toString('utf8');
      }),
    },
    divergedBody: diverged.bodyText,
    marker: markers
      .join('')
      .split('\n')
      .find(function (line) {
        return line.startsWith('REPROIT:DIVERGENCE ');
      }),
  };
}

function pythonSide() {
  var script = [
    'import io, json, sys',
    'from reproit_backend_py import replay',
    'capsule = json.loads(sys.stdin.read())',
    'session = replay.ReplaySession(capsule)',
    'served = replay.serve_http(session, {"method": "GET", "url": "http://llm.internal/stream"})',
    'held = io.StringIO()',
    'real = sys.stderr',
    'sys.stderr = held',
    'try:',
    '    probe = {"method": "POST", "url": "http://llm.internal/v1/chat",',
    '             "body": {"messages": [',
    '                 {"role": "user", "content": "hello"},',
    '                 {"role": "assistant", "content": "hi"},',
    '                 {"role": "user", "content": "DIFFERENT QUESTION"}]}}',
    '    diverged = replay.serve_http(session, probe)',
    'finally:',
    '    sys.stderr = real',
    'marker = next(line for line in held.getvalue().splitlines()',
    '              if line.startswith("REPROIT:DIVERGENCE "))',
    'print(json.dumps({',
    '    "serve": {"status": served["status"], "bodyText": served["body_text"],',
    '              "chunks": [c.decode("utf-8") for c in served["chunks"]]},',
    '    "divergedBody": diverged["body_text"],',
    '    "marker": marker,',
    '}))',
  ].join('\n');
  var result = child_process.spawnSync('python3', ['-c', script], {
    cwd: path.join(root, 'reproit-backend-py'),
    input: JSON.stringify(CAPSULE),
    encoding: 'utf8',
  });
  assert.strictEqual(result.status, 0, 'python side failed: ' + (result.error || result.stderr));
  var lines = result.stdout.trim().split('\n');
  return JSON.parse(lines[lines.length - 1]);
}

function rustSide() {
  var repoRoot = path.join(root, '..');
  var build = child_process.spawnSync(
    'cargo',
    [
      'build',
      '--quiet',
      '--manifest-path',
      path.join(repoRoot, 'Cargo.toml'),
      '-p',
      'reproit-backend',
      '--features',
      'instrument',
      '--example',
      'replay_parity_probe',
    ],
    { encoding: 'utf8' },
  );
  assert.strictEqual(build.status, 0, 'rust probe build failed: ' + (build.error || build.stderr));
  var probe = path.join(repoRoot, 'target/debug/examples/replay_parity_probe');
  var result = child_process.spawnSync(probe, [], {
    input: JSON.stringify(CAPSULE),
    encoding: 'utf8',
  });
  assert.strictEqual(result.status, 0, 'rust side failed: ' + (result.error || result.stderr));
  var lines = result.stdout.trim().split('\n');
  return JSON.parse(lines[lines.length - 1]);
}

var node = nodeSide();
var python = pythonSide();

assert.deepStrictEqual(python.serve, node.serve, 'served SSE exchange must match byte for byte');
assert.strictEqual(
  python.divergedBody,
  node.divergedBody,
  'the served 599 divergence body must match byte for byte',
);
assert.strictEqual(
  python.marker,
  node.marker,
  'the REPROIT:DIVERGENCE marker line must match byte for byte',
);
var report = JSON.parse(node.marker.slice('REPROIT:DIVERGENCE '.length));
assert.deepStrictEqual(report.bodyDelta, {
  kind: 'message',
  firstDifferingMessage: 2,
  recordedMessages: 3,
  liveMessages: 3,
});
console.log('PASS: python replay is byte-identical to the Node reference (serve, 599, marker)');
var rust = rustSide();
assert.deepStrictEqual(rust.serve, node.serve, 'rust served SSE exchange must match byte for byte');
assert.strictEqual(
  rust.divergedBody,
  node.divergedBody,
  'the rust served 599 divergence body must match byte for byte',
);
assert.strictEqual(
  rust.marker,
  node.marker,
  'the rust REPROIT:DIVERGENCE marker line must match byte for byte',
);
console.log('PASS: rust replay is byte-identical to the Node reference (serve, 599, marker)');
