<?php

/*!
 * Hermetic replay tests: with REPROIT_REPLAY set, the outbound boundary and
 * the database helper serve recorded exchanges with no socket and no driver,
 * divergence fails closed with the structured marker, and the envelope pins
 * the timezone and the seeded stream.
 *
 * Run: php test/replay_test.php
 */

declare(strict_types=1);

namespace ReproitBackend\Test;

use ReproitBackend\DivergenceError;
use ReproitBackend\Instrument;

use const ReproitBackend\DIVERGENCE_MARKER;

$capture = [
    'format' => 'reproit-backend-capture',
    'version' => 2,
    'operation' => 'GET /quote',
    'oracle' => 'backend-server-error',
    'envelope' => [
        'observedAtMs' => 1753747200000,
        'tz' => 'Europe/Berlin',
        'runtime' => 'php',
        'replaySeed' => '00ff00ff00ff00ff',
    ],
    'events' => [
        ['kind' => 'start', 'operation' => 'GET /quote', 'sequence' => 1],
        [
            'kind' => 'effect', 'effect' => 'read', 'sequence' => 2,
            'exchange' => [
                'protocol' => 'db',
                'request' => [
                    'text' => 'SELECT id FROM issuers WHERE symbol = $1',
                    'values' => ['ACME'],
                ],
                'response' => ['command' => 'SELECT', 'rowCount' => 1, 'rows' => [['id' => 7]]],
            ],
        ],
        [
            'kind' => 'effect', 'effect' => 'call', 'sequence' => 3,
            'exchange' => [
                'protocol' => 'http',
                'request' => [
                    'method' => 'GET',
                    'url' => 'http://pricing.internal/prices?tier=gold',
                ],
                'response' => [
                    'status' => 200,
                    'headers' => ['content-type' => 'application/json'],
                    'body' => ['prices' => null],
                ],
            ],
        ],
        ['kind' => 'return', 'status' => 500, 'success' => false, 'sequence' => 4],
    ],
];

$path = sys_get_temp_dir() . '/reproit-php-replay-' . getmypid() . '.json';
file_put_contents($path, json_encode($capture));
putenv('REPROIT_REPLAY=' . $path);

require_once __DIR__ . '/support.php';
require_once __DIR__ . '/../reproit.php';

// the envelope pins the timezone and seeds the stream
check(Instrument::replaying(), 'replay mode is active');
check_same('Europe/Berlin', date_default_timezone_get(), 'envelope pins the timezone');
$rng = Instrument::replayRng();
check($rng !== null, 'the capture seeds a replay stream');
$draw = $rng->nextFloat();
check($draw >= 0 && $draw < 1, 'the seeded stream yields a unit draw');

// database calls serve recorded rows without a driver
$result = Instrument::db('SELECT id FROM issuers WHERE symbol = $1', ['ACME'], function (): array {
    throw new \RuntimeException('the live driver must never be reached during hermetic replay');
});
check_same([['id' => 7]], $result['rows'], 'recorded rows served without a driver');

// http calls serve the recorded response without a socket
$response = Instrument::http('GET', 'http://pricing.internal/prices?tier=gold');
check_same(200, $response->status, 'recorded status served');
check_same(['prices' => null], $response->json(), 'recorded body served');

// an unmatched call diverges: 599 plus the structured marker at line start
$errorLog = sys_get_temp_dir() . '/reproit-php-replay-stderr-' . getmypid() . '.txt';
$descriptors = [1 => ['pipe', 'w'], 2 => ['file', $errorLog, 'w']];
$script = 'putenv("REPROIT_REPLAY=' . $path . '");'
    . 'require "' . __DIR__ . '/../reproit.php";'
    . '$r = ReproitBackend\\Instrument::http("GET", "http://pricing.internal/unknown");'
    . 'echo $r->status;';
$process = proc_open([PHP_BINARY, '-r', $script], $descriptors, $pipes);
$status = stream_get_contents($pipes[1]);
fclose($pipes[1]);
proc_close($process);
check_same('599', trim($status), 'an unmatched call answers 599');
$stderr = (string) file_get_contents($errorLog);
$marker = null;
foreach (explode("\n", $stderr) as $line) {
    if (str_starts_with($line, DIVERGENCE_MARKER)) {
        $marker = $line;
        break;
    }
}
// The line must START with the marker: the CLI matches on the prefix, so
// anything PHP prepends would make the divergence invisible to the verdict
// machinery.
check($marker !== null, 'structured divergence marker emitted at line start');
$report = json_decode(substr((string) $marker, \strlen(DIVERGENCE_MARKER)), true);
check_same('http', $report['protocol'] ?? null, 'divergence names the protocol');
check_same('GET', $report['got']['method'] ?? null, 'divergence names the live call');

// a diverged database call raises rather than guessing
$raised = false;
try {
    Instrument::db('SELECT nothing', null, fn (): array => ['rows' => []]);
} catch (DivergenceError) {
    $raised = true;
}
check($raised, 'a diverged database call fails closed');

@unlink($path);
@unlink($errorLog);
report('replay_test');
