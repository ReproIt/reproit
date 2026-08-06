/*
 * Functional cross-SDK contract test for the backend SDK family. For each SDK
 * this builds a real 5xx
 * capture batch and asserts:
 *   1. the batch is a portable capture-batch-v1;
 *   2. the error finding is tagged with the `backend-server-error` oracle;
 *   3. the scan-time response header name is `x-reproit-events` and decodes;
 *   4. obvious secret-shaped fields are structurally redacted before upload.
 *
 * Run: node sdk/test/backend_batch_test.js
 * The language-specific checks require their SDK toolchains.
 */
'use strict';

var assert = require('assert');
var child_process = require('child_process');
var path = require('path');

var root = path.join(__dirname, '..');
var HEADER_NAME = 'x-reproit-events';

function checkSdk(label, sample) {
  checkCausalCapture(label, sample.batch);

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

function checkCausalCapture(label, batch) {
  assert.strictEqual(batch.version, 1, label + ': causal capture version');
  assert.strictEqual(batch.projectId, 'app-demo', label + ': causal capture project');
  var observations = batch.events.filter(function (event) {
    return event.event.kind === 'observation';
  });
  assert.strictEqual(observations.length, 1, label + ': expected one failure observation');
  assert.strictEqual(
    observations[0].event.failure.signature,
    'backend-server-error:createOrder',
    label + ': observation signature must preserve the backend-server-error identity',
  );
  assert.deepStrictEqual(
    batch.events.slice(0, 2).map(function (event) {
      return event.event.kind;
    }),
    ['operation-start', 'trigger'],
    label + ': causal capture prefix',
  );
  assert.deepStrictEqual(
    batch.events.slice(-2).map(function (event) {
      return event.event.kind;
    }),
    ['operation-end', 'observation'],
    label + ': causal capture suffix',
  );
  batch.events.forEach(function (event, index) {
    assert.strictEqual(event.sequence, index + 1, label + ': dense event sequence');
  });
  var validation = child_process.spawnSync(
    'cargo',
    ['run', '-q', '-p', 'reproit-protocol', '--bin', 'capture-validate'],
    {
      cwd: path.join(root, '..'),
      input: JSON.stringify(batch),
      encoding: 'utf8',
    },
  );
  assert.strictEqual(
    validation.status,
    0,
    label + ': Rust semantic validator rejected the batch: ' + validation.stderr,
  );
}

function checkProducerSuite(label, command, args, cwd) {
  var result = child_process.spawnSync(command, args, {
    cwd: cwd,
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
  });
  assert.strictEqual(
    result.status,
    0,
    label + ': producer conformance failed: ' + (result.error || result.stderr),
  );
  console.log('PASS: ' + label + ' producer gate rejects incomplete captures');
}

function dotnetCommand() {
  if (process.env.DOTNET) {
    return process.env.DOTNET;
  }
  if (process.env.DOTNET_ROOT) {
    return path.join(process.env.DOTNET_ROOT, 'dotnet');
  }
  return path.join(process.env.HOME, '.dotnet', 'dotnet');
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
  trace.effect('write', {
    resource: 'orders',
    key: '1',
    exchange: { request: { id: '1' }, response: { stored: true } },
  });
  trace.finish({ error: 'boom' }, 500, false, true);
  var capture = sdk.Capture.create({
    endpoint: 'http://c/v1/capture-batches',
    apiKey: 'sk',
    appId: 'app-demo',
    build: '1.2.3',
  });
  var captureTrace = sdk.BackendTrace.begin(capture.context(), 'createOrder', {
    input: { item: 'widget', password: 'hunter22', apiKey: 'sk_live_leak' },
  });
  captureTrace.effect('write', {
    resource: 'orders',
    key: '1',
    exchange: { request: { id: '1' }, response: { stored: true } },
  });
  captureTrace.finish({ error: 'boom' }, 500, false, true);
  var batch = capture._buildBatch([
    { operation: 'createOrder', status: 500, events: captureTrace.events().slice() },
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
    'trace.effect("write", resource="orders", key="1",',
    '             exchange={"request": {"id": "1"}, "response": {"stored": True}})',
    'trace.finish({"error": "boom"}, 500, False, True)',
    'capture = Capture.create("http://c/v1/capture-batches", "sk", "app-demo", build="1.2.3")',
    'capture_trace = BackendTrace.begin(capture.context(), "createOrder",',
    '    input={"item": "widget", "password": "hunter22", "apiKey": "sk_live_leak"})',
    'capture_trace.effect("write", resource="orders", key="1",',
    '    exchange={"request": {"id": "1"}, "response": {"stored": True}})',
    'capture_trace.finish({"error": "boom"}, 500, False, True)',
    'batch = capture._build_batch([',
    '    {"operation": "createOrder", "status": 500, "events": list(capture_trace.events())}])',
    'print(json.dumps({"batch": batch, "header": trace.header(),',
    '                  "headerName": "x-reproit-events"}))',
  ].join('\n');
  var result = child_process.spawnSync(
    'python3',
    ['-c', script],
    {
      cwd: path.join(root, 'reproit-backend-py'),
      encoding: 'utf8',
    },
  );
  assert.strictEqual(
    result.status,
    0,
    'python sample failed: ' + (result.error || result.stderr),
  );
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
    'trace.effect("write", resource: "orders", key: "1",',
    '  exchange: { "request" => { "id" => "1" }, "response" => { "stored" => true } })',
    'trace.finish({ "error" => "boom" }, 500, false, true)',
    'capture = ReproitBackendRb::Capture.create(endpoint: "http://c/v1/capture-batches",',
    '  api_key: "sk", app_id: "app-demo", build: "1.2.3")',
    'capture_trace = ReproitBackendRb::BackendTrace.begin(capture.context, "createOrder",',
    '  input: { "item" => "widget", "password" => "hunter22", "apiKey" => "sk_live_leak" })',
    'capture_trace.effect("write", resource: "orders", key: "1",',
    '  exchange: { "request" => { "id" => "1" }, "response" => { "stored" => true } })',
    'capture_trace.finish({ "error" => "boom" }, 500, false, true)',
    'batch = capture.build_batch([',
    '  { "operation" => "createOrder", "status" => 500, "events" => capture_trace.events.dup }])',
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
    '$trace->effect("write", ["resource" => "orders", "key" => "1",',
    '  "exchange" => ["request" => ["id" => "1"], "response" => ["stored" => true]]]);',
    '$trace->finish(["error" => "boom"], 500, false, true);',
    '$capture = Capture::create(["endpoint" => "http://c/v1/capture-batches",',
    '  "apiKey" => "sk", "appId" => "app-demo", "build" => "1.2.3"]);',
    '$captureTrace = BackendTrace::begin($capture->context(), "createOrder", ["input" =>',
    '  ["item" => "widget", "password" => "hunter22", "apiKey" => "sk_live_leak"]]);',
    '$captureTrace->effect("write", ["resource" => "orders", "key" => "1",',
    '  "exchange" => ["request" => ["id" => "1"], "response" => ["stored" => true]]]);',
    '$captureTrace->finish(["error" => "boom"], 500, false, true);',
    '$batch = $capture->buildBatch([["operation" => "createOrder",',
    '  "status" => 500, "events" => $captureTrace->events()]]);',
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

checkProducerSuite(
  'Rust backend SDK',
  'cargo',
  ['test', '--quiet', '--all-features', 'capture::tests'],
  path.join(root, 'reproit-backend-rs'),
);
checkProducerSuite(
  'Java backend SDK',
  'mvn',
  ['-q', '-Dtest=CaptureTest,E2eTest', 'test'],
  path.join(root, 'reproit-backend-java'),
);
checkProducerSuite(
  '.NET backend SDK',
  dotnetCommand(),
  [
    'test',
    'ReproitBackend.Tests/ReproitBackend.Tests.csproj',
    '--nologo',
    '--filter',
    'FullyQualifiedName~CaptureTests',
  ],
  path.join(root, 'reproit-backend-dotnet'),
);

console.log('PASS: all eight backend SDKs enforce the portable capture contract');
