<?php

// Functional end-to-end test, mirroring reproit-backend-node/test/e2e.test.js:
// a real `php -S` server running the vanilla adapter with a planted 500, a
// stub ingest `php -S` server, real HTTP requests. Asserts the finding batch
// arrives correctly tagged with the reproitCapture sequence, and that a
// scan-time request round-trips the x-reproit-events header.
// Run: php test/e2e_test.php

declare(strict_types=1);

namespace ReproitBackend\Test;

use const ReproitBackend\CAPTURE_FORMAT;
use const ReproitBackend\SERVER_ERROR_ORACLE;

require __DIR__ . '/../reproit.php';
require __DIR__ . '/support.php';
require __DIR__ . '/event_batch_v1.php';

const READY_TIMEOUT_S = 10;
const INGEST_POLL_TIMEOUT_S = 8;

function free_port(): int
{
    $server = stream_socket_server('tcp://127.0.0.1:0', $code, $message);
    if ($server === false) {
        throw new \RuntimeException('no free port: ' . $message);
    }
    $name = stream_socket_get_name($server, false);
    fclose($server);
    return (int) substr((string) $name, strrpos((string) $name, ':') + 1);
}

/** @return resource */
function start_server(int $port, string $router, array $env)
{
    $command = [PHP_BINARY, '-S', '127.0.0.1:' . $port, $router];
    $spec = [1 => ['file', '/dev/null', 'w'], 2 => ['file', '/dev/null', 'w']];
    $process = proc_open($command, $spec, $pipes, __DIR__, array_merge([
        'PATH' => (string) getenv('PATH'),
    ], $env));
    if (!\is_resource($process)) {
        throw new \RuntimeException('failed to start php -S on port ' . $port);
    }
    $deadline = microtime(true) + READY_TIMEOUT_S;
    while (microtime(true) < $deadline) {
        $probe = @fsockopen('127.0.0.1', $port, $code, $message, 0.25);
        if ($probe !== false) {
            fclose($probe);
            return $process;
        }
        usleep(50000);
    }
    proc_terminate($process);
    throw new \RuntimeException('php -S on port ' . $port . ' did not become ready');
}

/** @return array{0: int, 1: array, 2: string} status, lowercased headers, body */
function request(
    string $url,
    string $method = 'GET',
    ?string $body = null,
    array $headers = [],
): array {
    $lines = '';
    foreach ($headers as $name => $value) {
        $lines .= $name . ': ' . $value . "\r\n";
    }
    $context = stream_context_create(['http' => [
        'method' => $method,
        'header' => $lines,
        'content' => $body ?? '',
        'timeout' => 10,
        'ignore_errors' => true,
    ]]);
    $responseBody = file_get_contents($url, false, $context);
    $raw = \function_exists('http_get_last_response_headers')
        ? http_get_last_response_headers()
        : ($http_response_header ?? null);
    if ($responseBody === false || !isset($raw[0])) {
        throw new \RuntimeException('request failed: ' . $url);
    }
    preg_match('#^HTTP/\S+\s+(\d{3})#', $raw[0], $matches);
    $responseHeaders = [];
    foreach (\array_slice($raw, 1) as $line) {
        $split = strpos($line, ':');
        if ($split !== false) {
            $name = strtolower(substr($line, 0, $split));
            $responseHeaders[$name] = trim(substr($line, $split + 1));
        }
    }
    return [(int) $matches[1], $responseHeaders, $responseBody];
}

function received_lines(string $log): array
{
    $raw = @file_get_contents($log);
    if ($raw === false || trim($raw) === '') {
        return [];
    }
    return array_map(
        fn (string $line) => json_decode($line, true),
        explode("\n", trim($raw)),
    );
}

$log = sys_get_temp_dir() . '/reproit-php-e2e-' . getmypid() . '.jsonl';
@unlink($log);
$ingestPort = free_port();
$appPort = free_port();
$ingest = start_server($ingestPort, __DIR__ . '/e2e_ingest.php', ['REPROIT_E2E_LOG' => $log]);
$app = start_server($appPort, __DIR__ . '/e2e_app.php', [
    'REPROIT_E2E_INGEST' => 'http://127.0.0.1:' . $ingestPort . '/v1/events',
]);
$base = 'http://127.0.0.1:' . $appPort;

try {
    // Planted 500 with a secret-shaped body field.
    [$status, , $body] = request($base . '/boom', 'POST', json_encode([
        'item' => 'widget',
        'apiKey' => 'sk_live_leak',
    ]), ['Content-Type' => 'application/json']);
    check_same(500, $status, 'planted route returns 500');
    check_same(['error' => 'boom'], json_decode($body, true), 'planted route body intact');

    // The capture flushes during the app request's shutdown; poll the stub log.
    $deadline = microtime(true) + INGEST_POLL_TIMEOUT_S;
    $received = [];
    while (microtime(true) < $deadline) {
        $received = received_lines($log);
        if ($received !== []) {
            break;
        }
        usleep(100000);
    }
    check_same(1, \count($received), 'exactly one batch reached the stub ingest');
    check_same('Bearer sk_live_test', $received[0]['authorization'] ?? null, 'Bearer auth');
    $batch = $received[0]['batch'];
    try {
        validate_event_batch($batch);
        check(true, 'batch passes the event-batch-v1 validator');
    } catch (\RuntimeException $error) {
        check(false, 'batch passes the event-batch-v1 validator (' . $error->getMessage() . ')');
    }
    check_same('app-e2e', $batch['appId'], 'appId');
    check_same('9.9.9', $batch['deployment']['version'] ?? null, 'deployment version');
    $findings = array_values(array_filter(
        $batch['frames'],
        fn (array $frame) => $frame['event']['kind'] === 'finding',
    ));
    check_same(1, \count($findings), 'exactly one finding frame');
    $finding = $findings[0]['event'];
    check_same(SERVER_ERROR_ORACLE, $finding['identity']['oracle'], 'tagged backend-server-error');
    check_same('reproit-backend-php', $finding['context']['capture'], 'sdk id in context.capture');
    $replay = $finding['context']['reproitCapture'];
    check_same(CAPTURE_FORMAT, $replay['format'], 'capture format');
    check_same(SERVER_ERROR_ORACLE, $replay['oracle'], 'capture oracle');
    $kinds = array_map(fn (array $event) => $event['kind'], $replay['events']);
    check_same(['start', 'effect', 'return'], $kinds, 'capture is start/effect/return');
    check_same('orders', $replay['events'][1]['resource'], 'effect resource');
    check_same(500, $replay['events'][2]['status'], 'return status 500');
    check_same(false, $replay['events'][2]['success'], 'return success false');
    $start = $replay['events'][0];
    check_same(
        true,
        $start['input']['body']['apiKey']['$reproit']['redacted'] ?? null,
        'secret-shaped input field structurally redacted before upload',
    );
    check_same('widget', $start['input']['body']['item'] ?? null, 'non-secret input untouched');

    // Scan-time request: header round-trip, no capture of the healthy call.
    [$status, $headers] = request($base . '/ok', 'GET', null, [
        'x-reproit-trace' => 'trace-e2e',
        'x-reproit-actor' => 'alice',
    ]);
    check_same(200, $status, 'scan-time request succeeds');
    $header = $headers['x-reproit-events'] ?? '';
    check($header !== '', 'x-reproit-events response header present');
    $padded = strtr($header, '-_', '+/') . str_repeat('=', -\strlen($header) % 4 & 3);
    $events = json_decode((string) base64_decode($padded), true);
    check_same('trace-e2e', $events[0]['traceId'], 'decoded trace id');
    check_same('alice', $events[0]['actor'], 'decoded actor');
    check_same('return', $events[\count($events) - 1]['kind'], 'last decoded event is return');
    check_same(200, $events[\count($events) - 1]['status'], 'decoded return status');
    usleep(500000);
    check_same(1, \count(received_lines($log)), 'healthy scan-time request not captured');
} finally {
    proc_terminate($app);
    proc_terminate($ingest);
    @unlink($log);
}

report('e2e_test');
