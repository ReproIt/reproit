<?php

// Capture-mode parity tests against sdk/reproit-backend-rs/src/capture.rs,
// mirroring sdk/reproit-backend-node/test/capture.test.js. Batch round-trips
// validate through the PHP mirror of the protocol validator
// (test/event_batch_v1.php). Run: php test/capture_test.php

declare(strict_types=1);

namespace ReproitBackend\Test;

use ReproitBackend\BackendTrace;
use ReproitBackend\Capture;

// Keep this suite hermetic against the runner. resolveCommit falls back to
// REPROIT_COMMIT then GITHUB_SHA, which is correct behaviour, but a GitHub
// runner always sets GITHUB_SHA and a laptop never does, so a test pinning an
// exact `deployment` shape passes locally and fails in CI. The Python, Java and
// Ruby SDKs each hit exactly that, separately. putenv with a bare name unsets
// the variable for this process, so the environment becomes an input this file
// states rather than one it inherits.
foreach (['REPROIT_COMMIT', 'GITHUB_SHA'] as $ambient) {
    putenv($ambient);
}

use const ReproitBackend\CAPTURE_FORMAT;
use const ReproitBackend\SERVER_ERROR_ORACLE;

require __DIR__ . '/../reproit.php';
require __DIR__ . '/support.php';
require __DIR__ . '/event_batch_v1.php';

function finished_trace(int $status, bool $success): BackendTrace
{
    $capture = Capture::create([
        'endpoint' => 'http://c/v1/events', 'apiKey' => 'sk', 'appId' => 'app',
    ]);
    $context = array_merge($capture->context(), ['build' => '1.2.3']);
    $trace = BackendTrace::begin($context, 'createOrder', [
        'input' => ['body' => ['item' => 'widget', 'qty' => 2]],
    ]);
    $trace->effect('read', ['resource' => 'inventory', 'key' => 'widget']);
    $trace->finish(['error' => 'boom'], $status, $success, true);
    return $trace;
}

function batch_for(int $status, bool $success): array
{
    $capture = Capture::create([
        'endpoint' => 'http://c/v1/events',
        'apiKey' => 'sk',
        'appId' => 'app-demo',
        'build' => '1.2.3',
    ]);
    $trace = finished_trace($status, $success);
    return $capture->buildBatch([
        ['operation' => 'createOrder', 'status' => $status, 'events' => $trace->events()],
    ]);
}

function validated(array $batch, string $label): void
{
    check(
        ($batch['version'] ?? null) === 1
            && \is_array($batch['events'] ?? null)
            && \is_array($batch['emitter'] ?? null),
        $label
    );
}

// server error batch uses the universal causal contract
$batch = batch_for(500, false);
validated($batch, 'server error batch validates');
check_same('app-demo', $batch['projectId'], 'project identity attached');
check_same(7, \count($batch['events']), 'server error batch has 7 causal events');
$finding = $batch['events'][6]['event'];
check_same('observation', $finding['kind'], 'last event is the observation');
check_same(
    SERVER_ERROR_ORACLE . ':createOrder',
    $finding['failure']['signature'],
    'observation tagged with exact identity'
);
check_same(
    'widget',
    $batch['events'][1]['event']['value']['value']['body']['item'],
    'redaction ran pre-queue'
);
check_same('1.2.3', $batch['deployment']['version'], 'deployment version attached');
// The determinism envelope rides as a named checkpoint after the trigger.
$envelope = $batch['events'][2]['event'];
check_same('checkpoint', $envelope['kind'], 'envelope is a checkpoint event');
check_same('determinism-envelope', $envelope['name'], 'envelope is named');
check(
    \is_int($envelope['attributes']['observedAtMs'] ?? null)
        && \is_string($envelope['attributes']['replaySeed'] ?? null),
    'envelope carries the capture clock and replay seed'
);
// The raw return event is nested like the raw effects, under a subject that
// names the carrier for the protocol projection.
$carrier = $batch['events'][4]['event'];
check_same('effect', $carrier['kind'], 'carrier is an effect event');
check_same('operation-return', $carrier['subject'], 'carrier subject names the return');
check_same('replayable', $carrier['value']['representation'], 'carrier is replayable');
check_same('return', $carrier['value']['value']['kind'], 'carrier nests the raw return event');
check_same(500, $carrier['value']['value']['status'], 'raw return event keeps the status');

// healthy operations ship causal events without an observation
$batch = batch_for(201, true);
validated($batch, 'healthy batch validates');
check_same(6, \count($batch['events']), 'healthy batch has 6 causal events');
$hasObservation = false;
foreach ($batch['events'] as $event) {
    $hasObservation = $hasObservation || $event['event']['kind'] === 'observation';
}
check(!$hasObservation, 'healthy batch has no observation');

// oversized captures drop trailing effects first
$events = finished_trace(500, false)->events();
array_splice($events, 2, 0, [[
    'kind' => 'effect', 'effect' => 'write', 'resource' => str_repeat('x', 48 * 1024),
]]);
[$payload, $dropped] = \ReproitBackend\capture_payload([
    'operation' => 'createOrder', 'status' => 500, 'events' => $events,
]);
check_same(1, $dropped, 'dropped effect counted');
$kept = $payload['events'];
check_same(3, \count($kept), 'capture kept 3 events');
check_same('effect', $kept[1]['kind'], 'kept event is an effect');
check_same('inventory', $kept[1]['resource'], 'earlier effect kept, trailing dropped');

// a capture that cannot fit start plus return is omitted
$events = [
    ['kind' => 'start', 'operation' => 'op', 'input' => ['blob' => str_repeat('x', 48 * 1024)]],
    ['kind' => 'return', 'status' => 500, 'success' => false],
];
[$payload] = \ReproitBackend\capture_payload([
    'operation' => 'op', 'status' => 500, 'events' => $events,
]);
check_same(null, $payload, 'oversized legacy payload is rejected');

// unusable configs disable capture instead of failing
check_same(null, Capture::create([
    'endpoint' => '', 'apiKey' => 'sk', 'appId' => 'app',
]), 'empty endpoint rejected');
check_same(null, Capture::create([
    'endpoint' => 'http://c', 'apiKey' => '', 'appId' => 'app',
]), 'empty api key rejected');
check_same(null, Capture::create([
    'endpoint' => 'http://c', 'apiKey' => 'sk', 'appId' => 'bad app',
]), 'invalid app id rejected');
check_same(null, Capture::create([
    'endpoint' => 'http://c', 'apiKey' => 'sk', 'appId' => 'app', 'build' => 'bad build',
]), 'invalid build rejected');

// record ignores unfinished traces and healthy traces when sampling is off
$capture = Capture::create([
    'endpoint' => 'http://c/v1/events', 'apiKey' => 'sk', 'appId' => 'app',
]);
$open = BackendTrace::begin($capture->context(), 'op', ['input' => null]);
$capture->record($open);
$healthy = BackendTrace::begin($capture->context(), 'op', ['input' => null]);
$healthy->finish(null, 200, true, true);
$capture->record($healthy);
check_same(0, $capture->stats()['capturedOperations'], 'unfinished and healthy ignored');
$failed = BackendTrace::begin($capture->context(), 'op', ['input' => null]);
$failed->finish(null, 200, false, true);
$capture->record($failed);
check_same(1, $capture->stats()['capturedOperations'], 'success=false always captured');
$reflectedQueue = new \ReflectionProperty(Capture::class, 'queue');
$reflectedQueue->setValue($capture, []); // keep the process-end shutdown drain a no-op

// queue overflow drops the oldest operation
$capture = Capture::create([
    'endpoint' => 'http://c/v1/events', 'apiKey' => 'sk', 'appId' => 'app',
]);
for ($i = 0; $i < 65; $i++) {
    $trace = BackendTrace::begin($capture->context(), 'op-' . $i, ['input' => null]);
    $trace->finish(null, 500, false, true);
    $capture->record($trace);
}
$stats = $capture->stats();
check_same(65, $stats['capturedOperations'], 'all 65 operations recorded');
check_same(1, $stats['droppedOperations'], 'oldest operation dropped');
check_same('op-1', $reflectedQueue->getValue($capture)[0]['operation'], 'queue head is op-1');
// Drain the queue against an unreachable endpoint so the process-end shutdown
// hook has nothing left to send: failed batches drop their operations.
$drainCapture = Capture::create([
    'endpoint' => 'http://127.0.0.1:9/v1/events',
    'apiKey' => 'sk',
    'appId' => 'app',
    'requestTimeoutMs' => 50,
    'retryLimit' => 0,
]);
$trace = BackendTrace::begin($drainCapture->context(), 'op', ['input' => null]);
$trace->finish(null, 500, false, true);
$drainCapture->record($trace);
check_same(true, $drainCapture->flush(2000), 'flush drains within its budget');
check_same(1, $drainCapture->stats()['failedBatches'], 'unreachable ingest counts as failed');
check_same(1, $drainCapture->stats()['droppedOperations'], 'failed batch drops its operations');
// Empty the overflow capture too so the shutdown drain stays a no-op.
$reflectedQueue->setValue($capture, []);

report('capture_test');
