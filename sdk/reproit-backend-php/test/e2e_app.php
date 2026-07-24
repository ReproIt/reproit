<?php

// e2e fixture: vanilla PHP app served by `php -S`, wrapped with handle_request
// and a capture that posts to the stub ingest (REPROIT_E2E_INGEST). /boom is
// the planted 500 that records a write effect; /ok is healthy.

declare(strict_types=1);

require __DIR__ . '/../reproit.php';
require __DIR__ . '/../vanilla.php';

use ReproitBackend\BackendTrace;
use ReproitBackend\Capture;

use function ReproitBackend\handle_request;

$capture = Capture::create([
    'endpoint' => (string) getenv('REPROIT_E2E_INGEST'),
    'apiKey' => 'sk_live_test',
    'appId' => 'app-e2e',
    'build' => '9.9.9',
    'flushIntervalMs' => 100,
    'shutdownTimeoutMs' => 2000,
]);

$path = parse_url($_SERVER['REQUEST_URI'] ?? '/', PHP_URL_PATH);
$method = $_SERVER['REQUEST_METHOD'] ?? 'GET';

if ($path === '/ok' && $method === 'GET') {
    handle_request($capture, fn (?BackendTrace $trace) => [200, ['ok' => true]]);
} elseif ($path === '/boom' && $method === 'POST') {
    handle_request($capture, function (?BackendTrace $trace) {
        $trace?->effect('write', ['resource' => 'orders', 'key' => '1']);
        return [500, ['error' => 'boom']];
    });
} else {
    http_response_code(404);
    header('Content-Type: application/json');
    echo '{"error":"not found"}';
}
