<?php

// e2e fixture: stub Cloud ingest served by `php -S`. Appends every received
// POST /v1/events (authorization header + decoded batch) as one JSON line to
// REPROIT_E2E_LOG and answers `{"accepted":true}`.

declare(strict_types=1);

$path = parse_url($_SERVER['REQUEST_URI'] ?? '/', PHP_URL_PATH);
if (($_SERVER['REQUEST_METHOD'] ?? '') === 'POST' && $path === '/v1/events') {
    $record = json_encode([
        'authorization' => $_SERVER['HTTP_AUTHORIZATION'] ?? null,
        'batch' => json_decode((string) file_get_contents('php://input'), true),
    ]);
    file_put_contents((string) getenv('REPROIT_E2E_LOG'), $record . "\n", FILE_APPEND | LOCK_EX);
    header('Content-Type: application/json');
    echo '{"accepted":true}';
} else {
    http_response_code(404);
    echo '{"error":"not found"}';
}
