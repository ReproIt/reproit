<?php

// The shared config service both tests talk to, served by one `php -S`
// worker. Stateful on purpose ACROSS requests: `php -S` re-executes this
// router per request, so the legacy toggle persists through a flag file in
// REPROIT_FIXTURE_STATE (set by the test script, one directory per run).
// POST /format/legacy switches the service to its legacy response format,
// which wraps the payload; GET /tax-rate answers in whichever format the
// toggle selected. Never started under replay, where the SDK serves the
// recorded exchanges in process.

declare(strict_types=1);

$state = getenv('REPROIT_FIXTURE_STATE');
$state = \is_string($state) && $state !== ''
    ? $state
    : sys_get_temp_dir() . '/reproit-php-flaky-state';
$flag = $state . '/legacy.flag';

if (($_SERVER['REQUEST_METHOD'] ?? '') === 'POST'
    && ($_SERVER['REQUEST_URI'] ?? '') === '/format/legacy'
) {
    if (!is_dir($state)) {
        @mkdir($state, 0o777, true);
    }
    file_put_contents($flag, "1\n");
    http_response_code(204);
    return;
}

header('Content-Type: application/json');
echo is_file($flag)
    ? json_encode(['value' => ['rate' => 0.25]])
    : json_encode(['rate' => 0.25]);
