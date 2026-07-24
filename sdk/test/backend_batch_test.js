/*
 * Functional cross-SDK contract test for the backend SDK family (Node, Python,
 * and Go; the Rust reference pins the same contract in-crate through
 * reproit_protocol::EventBatch::validate). For each SDK this builds a real 5xx
 * capture batch and asserts:
 *   1. the batch is valid event-batch-v1 (event_batch_v1.js protocol mirror);
 *   2. the error finding is tagged with the `backend-server-error` oracle;
 *   3. the scan-time response header name is `x-reproit-events` and decodes;
 *   4. obvious secret-shaped fields are structurally redacted before upload.
 *
 * Run: node sdk/test/backend_batch_test.js
 * (The Python sample needs `uv` on PATH; the Go sample needs `go`.)
 */
'use strict';

var assert = require('assert');
var child_process = require('child_process');
var path = require('path');

var root = path.join(__dirname, '..');
var validateEventBatch = require('./event_batch_v1.js').validateEventBatch;

var HEADER_NAME = 'x-reproit-events';

function checkSdk(label, sample) {
  validateEventBatch(sample.batch);
  var findings = sample.batch.frames.filter(function (frame) {
    return frame.event.kind === 'finding';
  });
  assert.strictEqual(findings.length, 1, label + ': expected exactly one finding frame');
  var finding = findings[0].event;
  assert.strictEqual(
    finding.identity.oracle,
    'backend-server-error',
    label + ': finding must be tagged with the backend-server-error oracle',
  );
  var replay = finding.context.reproitCapture;
  assert.strictEqual(replay.format, 'reproit-backend-capture', label + ': capture format');
  var kinds = replay.events.map(function (event) {
    return event.kind;
  });
  assert.deepStrictEqual(kinds, ['start', 'effect', 'return'], label + ': capture sequence');

  assert.strictEqual(sample.headerName, HEADER_NAME, label + ': response header name');
  var padded = sample.header + '='.repeat((4 - (sample.header.length % 4)) % 4);
  var events = JSON.parse(Buffer.from(padded, 'base64').toString('utf8'));
  assert.strictEqual(events[0].traceId, 'trace-a', label + ': header decodes to trace events');

  var input = events[0].input;
  assert.strictEqual(
    input.password.$reproit.redacted,
    true,
    label + ': password field must be redacted',
  );
  assert.strictEqual(
    input.apiKey.$reproit.redacted,
    true,
    label + ': api key field must be redacted',
  );
  assert.strictEqual(events[0].input.item, 'widget', label + ': non-secret fields survive');
  console.log('PASS: ' + label + ' batch is valid, tagged, and redacted');
}

// One shared scenario per SDK: a scan-time trace (for the header) and a 5xx
// capture batch built from a failed operation.

function nodeSample() {
  var sdk = require(path.join(root, 'reproit-backend-node/index.js'));
  var context = {
    traceId: 'trace-a',
    actor: null,
    actionIndex: 0,
    build: null,
    configContract: null,
  };
  var trace = sdk.BackendTrace.begin(context, 'createOrder', {
    input: { item: 'widget', password: 'hunter22', apiKey: 'sk_live_leak' },
  });
  trace.effect('write', { resource: 'orders', key: '1' });
  trace.finish({ error: 'boom' }, 500, false, true);
  var capture = sdk.Capture.create({
    endpoint: 'http://c/v1/events',
    apiKey: 'sk',
    appId: 'app-demo',
    build: '1.2.3',
  });
  var batch = capture._buildBatch([
    { operation: 'createOrder', status: 500, events: trace.events().slice() },
  ]);
  return { batch: batch, header: trace.header(), headerName: HEADER_NAME };
}

function pythonSample() {
  var script = [
    'import json',
    'from reproit_backend_py import BackendTrace, Capture',
    'context = {"trace_id": "trace-a", "actor": None, "action_index": 0,',
    '           "build": None, "config_contract": None}',
    'trace = BackendTrace.begin(context, "createOrder",',
    '    input={"item": "widget", "password": "hunter22", "apiKey": "sk_live_leak"})',
    'trace.effect("write", resource="orders", key="1")',
    'trace.finish({"error": "boom"}, 500, False, True)',
    'capture = Capture.create("http://c/v1/events", "sk", "app-demo", build="1.2.3")',
    'batch = capture._build_batch([',
    '    {"operation": "createOrder", "status": 500, "events": list(trace.events())}])',
    'print(json.dumps({"batch": batch, "header": trace.header(),',
    '                  "headerName": "x-reproit-events"}))',
  ].join('\n');
  var result = child_process.spawnSync(
    'uv',
    ['run', '--project', path.join(root, 'reproit-backend-py'), 'python', '-c', script],
    { encoding: 'utf8' },
  );
  assert.strictEqual(result.status, 0, 'python sample failed: ' + result.stderr);
  var lines = result.stdout.trim().split('\n');
  return JSON.parse(lines[lines.length - 1]);
}

function goSample() {
  var result = child_process.spawnSync('go', ['run', './contractsample'], {
    cwd: path.join(root, 'reproit-backend-go'),
    encoding: 'utf8',
  });
  assert.strictEqual(result.status, 0, 'go sample failed: ' + result.stderr);
  var lines = result.stdout.trim().split('\n');
  return JSON.parse(lines[lines.length - 1]);
}

function rubySample() {
  var script = [
    'require "json"',
    '$LOAD_PATH.unshift(File.join(%q{' + root + '}, "reproit-backend-rb/lib"))',
    'require "reproit_backend_rb"',
    'context = { "trace_id" => "trace-a", "actor" => nil, "action_index" => 0,',
    '            "build" => nil, "config_contract" => nil }',
    'trace = ReproitBackendRb::BackendTrace.begin(context, "createOrder",',
    '  input: { "item" => "widget", "password" => "hunter22", "apiKey" => "sk_live_leak" })',
    'trace.effect("write", resource: "orders", key: "1")',
    'trace.finish({ "error" => "boom" }, 500, false, true)',
    'capture = ReproitBackendRb::Capture.create(endpoint: "http://c/v1/events",',
    '  api_key: "sk", app_id: "app-demo", build: "1.2.3")',
    'batch = capture.build_batch([',
    '  { "operation" => "createOrder", "status" => 500, "events" => trace.events.dup }])',
    'puts JSON.generate({ batch: batch, header: trace.header,',
    '                     headerName: "x-reproit-events" })',
  ].join('\n');
  var result = child_process.spawnSync('ruby', ['-e', script], { encoding: 'utf8' });
  assert.strictEqual(result.status, 0, 'ruby sample failed: ' + result.stderr);
  var lines = result.stdout.trim().split('\n');
  return JSON.parse(lines[lines.length - 1]);
}

function phpSample() {
  var script = [
    'require %q@' + path.join(root, 'reproit-backend-php/reproit.php') + '@;',
    'use ReproitBackend\\BackendTrace; use ReproitBackend\\Capture;',
    '$context = ["traceId" => "trace-a", "actor" => null, "actionIndex" => 0,',
    '            "build" => null, "configContract" => null];',
    '$trace = BackendTrace::begin($context, "createOrder", ["input" =>',
    '  ["item" => "widget", "password" => "hunter22", "apiKey" => "sk_live_leak"]]);',
    '$trace->effect("write", ["resource" => "orders", "key" => "1"]);',
    '$trace->finish(["error" => "boom"], 500, false, true);',
    '$capture = Capture::create(["endpoint" => "http://c/v1/events",',
    '  "apiKey" => "sk", "appId" => "app-demo", "build" => "1.2.3"]);',
    '$batch = $capture->buildBatch([["operation" => "createOrder",',
    '  "status" => 500, "events" => $trace->events()]]);',
    'echo json_encode(["batch" => $batch, "header" => $trace->header(),',
    '  "headerName" => "x-reproit-events"]);',
  ]
    .join('\n')
    .replace(/%q@([^@]*)@/, "'$1'");
  var result = child_process.spawnSync('php', ['-r', script], { encoding: 'utf8' });
  assert.strictEqual(result.status, 0, 'php sample failed: ' + result.stderr);
  var lines = result.stdout.trim().split('\n');
  return JSON.parse(lines[lines.length - 1]);
}

checkSdk('Node backend SDK', nodeSample());
checkSdk('Python backend SDK', pythonSample());
checkSdk('Go backend SDK', goSample());
checkSdk('Ruby backend SDK', rubySample());
checkSdk('PHP backend SDK', phpSample());

console.log('PASS: backend SDK batches match event-batch-v1 and the oracle/redaction contract');
