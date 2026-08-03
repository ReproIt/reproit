<?php
declare(strict_types=1);

require dirname(__DIR__) . '/vanilla.php';

use ReproitBackend\BackendTrace;
use function ReproitBackend\handle_request;

if (getenv('REPROIT_BENCH_MOUNTED') === '1') {
    handle_request(null, fn (?BackendTrace $trace): array => [
        200, ['account' => ['id' => 42, 'ok' => true]],
    ]);
} else {
    header('Content-Type: application/json');
    echo '{"account":{"id":42,"ok":true}}';
}
