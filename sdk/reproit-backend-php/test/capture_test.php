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

use const ReproitBackend\AGENT_GUARDRAIL_ORACLE;
use const ReproitBackend\AGENT_LOOP_BOUND_ORACLE;
use const ReproitBackend\CAPTURE_FORMAT;
use const ReproitBackend\CI_RESULT_MARKER;
use const ReproitBackend\CI_SPOOL_MARKER;
use const ReproitBackend\SERVER_ERROR_ORACLE;
use const ReproitBackend\TEST_FAILURE_ORACLE;

use function ReproitBackend\marked_oracle;

require __DIR__ . '/../reproit.php';
require __DIR__ . '/../ci.php';
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

// agent oracle markers ride the trace and reject unknown ids
$capture = Capture::create([
    'endpoint' => 'http://c/v1/events', 'apiKey' => 'sk', 'appId' => 'app',
]);
$trace = BackendTrace::begin($capture->context(), 'POST /assist', ['input' => null]);
check_throws(
    fn () => $trace->oracle('made-up-oracle'),
    'InvalidOperation',
    'unknown oracle id rejected'
);
$trace->oracle(AGENT_GUARDRAIL_ORACLE, ['tool' => 'delete_order']);
$trace->finish(['error' => 'guardrail'], 500, false, true);
check_same(
    AGENT_GUARDRAIL_ORACLE,
    marked_oracle($trace->events()),
    'marked oracle found on the finished trace'
);

// a marked agent operation is captured even without a 5xx
$markedTrace = BackendTrace::begin($capture->context(), 'POST /assist', ['input' => null]);
$markedTrace->oracle(AGENT_LOOP_BOUND_ORACLE, ['iterations' => 9, 'bound' => 4]);
$markedTrace->finish(['note' => 'gave up'], 200, true, true);
$capture->record($markedTrace);
check_same(1, $capture->stats()['capturedOperations'], 'marked operation captured without 5xx');

// a marked failure observation carries the agent oracle id
$batch = $capture->buildBatch([
    ['operation' => 'POST /assist', 'status' => 500, 'events' => $trace->events()],
]);
$observation = $batch['events'][\count($batch['events']) - 1]['event'];
check_same('observation', $observation['kind'], 'marked batch ends in the observation');
check_same(
    AGENT_GUARDRAIL_ORACLE . ':POST /assist',
    $observation['failure']['signature'],
    'marked signature carries the agent oracle id'
);
check_same(
    'contract-violation',
    $observation['failure']['observation'],
    'marked observation is a contract violation'
);
// The replayable capture payload carries the marked id in place of the 5xx
// default too.
[$payload] = \ReproitBackend\capture_payload([
    'operation' => 'POST /assist', 'status' => 500, 'events' => $trace->events(),
]);
check_same(AGENT_GUARDRAIL_ORACLE, $payload['oracle'], 'capture payload carries the marked id');
$reflectedQueue->setValue($capture, []); // keep the process-end shutdown drain a no-op

// --- CI capture mode (ci.php): each scenario runs the plain-script wrapper
// in a child process because capture/replay mode is decided by env at
// suite() time and the wrapper owns the process exit code. Mirrors
// sdk/reproit-backend-node/test/ci.test.js.

// unknown suite options are rejected, not ignored
$rejected = false;
try {
    \ReproitBackend\Ci::suite('s', ['retries' => 2]);
} catch (\InvalidArgumentException $error) {
    $rejected = str_contains($error->getMessage(), 'unknown option');
}
check($rejected, 'unknown suite option rejected');

// One upstream call, one assertion that fails unless FIXED=1. The upstream
// stub only boots outside replay, exactly like a real suite's dependencies.
$ciDir = sys_get_temp_dir() . '/reproit-php-ci-' . getmypid();
@mkdir($ciDir, 0o777, true);
file_put_contents($ciDir . '/upstream.php', <<<'PHP'
<?php
header('Content-Type: application/json');
echo json_encode(['n' => 7]);
PHP);
$sdkDir = \dirname(__DIR__);
file_put_contents($ciDir . '/fixture.php', <<<PHP
<?php
require '$sdkDir/reproit.php';
require '$sdkDir/ci.php';
\$test = \\ReproitBackend\\Ci::suite('unit');
\$test('asserts the upstream answer', function (): void {
    \$response = \\ReproitBackend\\Instrument::http('GET', 'http://127.0.0.1:19981/n');
    \$body = json_decode(\$response->body, true);
    \$n = \$body['n'] ?? null;
    \$expected = getenv('FIXED') === '1' ? 7 : 8;
    if (\$n !== \$expected) {
        throw new RuntimeException(\$n . ' !== ' . \$expected);
    }
});
PHP);
$upstream = proc_open(
    [PHP_BINARY, '-S', '127.0.0.1:19981', $ciDir . '/upstream.php'],
    [1 => ['file', '/dev/null', 'w'], 2 => ['file', '/dev/null', 'w']],
    $upstreamPipes
);
for ($i = 0; $i < 100 && @file_get_contents('http://127.0.0.1:19981/n') === false; $i++) {
    usleep(50000);
}

/** Run the child fixture with `$env` on top of a clean inherited set. */
function run_ci(string $fixture, array $env): array
{
    $spec = [1 => ['pipe', 'w'], 2 => ['pipe', 'w']];
    $proc = proc_open([PHP_BINARY, $fixture], $spec, $pipes, null, $env + getenv());
    $stdout = stream_get_contents($pipes[1]);
    $stderr = stream_get_contents($pipes[2]);
    $status = proc_close($proc);
    return [$status, (string) $stderr, (string) $stdout];
}

// a failing test spools a test-trigger capsule with the exchange
$spool = $ciDir . '/spool';
[$status, $stderr] = run_ci($ciDir . '/fixture.php', [
    'REPROIT_CI_CAPTURE' => '1', 'REPROIT_CI_SPOOL' => $spool,
]);
check($status !== 0, 'failing capture-mode run exits non-zero');
check(str_contains($stderr, CI_SPOOL_MARKER), 'spool marker written');
$capsules = glob($spool . '/capsule-*.json') ?: [];
check_same(1, \count($capsules), 'exactly one capsule spooled');
$capsule = json_decode((string) file_get_contents($capsules[0]), true);
check_same(CAPTURE_FORMAT, $capsule['format'], 'capsule format');
check_same(2, $capsule['version'], 'capsule version 2');
check_same('test:unit#asserts the upstream answer', $capsule['operation'], 'test identity');
check_same(TEST_FAILURE_ORACLE, $capsule['oracle'], 'authored-invariant oracle');
check(\is_string($capsule['envelope']['replaySeed'] ?? null), 'envelope carries a replay seed');
$exchanges = array_values(array_filter(
    $capsule['events'],
    fn (array $event): bool => \is_array($event['exchange'] ?? null)
));
check_same(1, \count($exchanges), 'capsule carries the upstream exchange');
check_same(7, $exchanges[0]['exchange']['response']['body']['n'], 'recorded response body');
$returned = $capsule['events'][\count($capsule['events']) - 1];
check_same(false, $returned['success'], 'return records the failure');
check(str_contains((string) $returned['output']['error'], '7 !== 8'), 'bounded failure message');

// replay re-runs the named test and reports failed, then passed. No live
// upstream is needed; the SDK serves the recording in process.
[$status, $stderr] = run_ci($ciDir . '/fixture.php', ['REPROIT_REPLAY' => $capsules[0]]);
check($status !== 0, 'replay of the unfixed test exits non-zero');
$line = null;
foreach (explode("\n", $stderr) as $candidate) {
    if (str_starts_with($candidate, CI_RESULT_MARKER)) {
        $line = json_decode(substr($candidate, \strlen(CI_RESULT_MARKER)), true);
        break;
    }
}
check(\is_array($line), 'replay writes the structured result marker');
check_same('failed', $line['status'] ?? null, 'replay reports failed');
check_same('test:unit#asserts the upstream answer', $line['operation'] ?? null, 'named test');
check(str_contains((string) ($line['failure'] ?? ''), '7 !== 8'), 'replay failure message');
[$status, $stderr] = run_ci($ciDir . '/fixture.php', [
    'REPROIT_REPLAY' => $capsules[0], 'FIXED' => '1',
]);
check_same(0, $status, 'replay of the fixed test exits zero');
check(str_contains($stderr, '"status":"passed"'), 'replay reports passed');

// a full spool drops the capsule and counts the drop
$full = $ciDir . '/full-spool';
@mkdir($full, 0o777, true);
file_put_contents($full . '/existing.json', str_repeat('x', 4 * 1024));
[$status, $stderr] = run_ci($ciDir . '/fixture.php', [
    'REPROIT_CI_CAPTURE' => '1',
    'REPROIT_CI_SPOOL' => $full,
    'REPROIT_CI_SPOOL_MAX' => (string) (4 * 1024),
]);
check($status !== 0, 'over-cap run still fails the test');
check_same(0, \count(glob($full . '/capsule-*.json') ?: []), 'over-cap capsule not written');
check_same(1, (int) file_get_contents($full . '/dropped.count'), 'drop counted on disk');

// without capture or replay env the wrapper is inert plain execution
[$status, $stderr] = run_ci($ciDir . '/fixture.php', []);
check($status !== 0, 'plain failing run exits non-zero');
check(!str_contains($stderr, CI_SPOOL_MARKER), 'no spool marker without capture env');
check(!str_contains($stderr, CI_RESULT_MARKER), 'no result marker without replay env');

proc_terminate($upstream);
proc_close($upstream);
shell_exec('rm -rf ' . escapeshellarg($ciDir));

report('capture_test');
