<?php

/*!
 * Automatic outbound-stream capture tests for the PHP SDK.
 *
 * Proves the auto_prepend wrapper (autocapture.php) captures plain
 * file_get_contents on an http:// URL with no per-call change, through the
 * same recording path as Instrument::http, and that in replay mode it serves
 * the recorded body with no socket and fails closed on divergence.
 *
 * Run: php test/autocapture_test.php
 */

declare(strict_types=1);

namespace ReproitBackend\Test;

use ReproitBackend\BackendTrace;
use ReproitBackend\Instrument;
use ReproitBackend\StreamCapture;

require_once __DIR__ . '/support.php';
require_once __DIR__ . '/../reproit.php';

const AUTOCAPTURE_READY_TIMEOUT_S = 10;

function ac_free_port(): int
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
function ac_start_server(int $port, string $router)
{
    $command = [PHP_BINARY, '-S', '127.0.0.1:' . $port, $router];
    $spec = [1 => ['file', '/dev/null', 'w'], 2 => ['file', '/dev/null', 'w']];
    $process = proc_open($command, $spec, $pipes, __DIR__, ['PATH' => (string) getenv('PATH')]);
    if (!\is_resource($process)) {
        throw new \RuntimeException('failed to start php -S on port ' . $port);
    }
    $deadline = microtime(true) + AUTOCAPTURE_READY_TIMEOUT_S;
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

function ac_trace(): BackendTrace
{
    return BackendTrace::begin(
        [
            'traceId' => 'cap-auto-1',
            'actor' => null,
            'actionIndex' => 0,
            'build' => null,
            'configContract' => null,
            'captureEnvelope' => true,
        ],
        'GET /quote',
        ['input' => ['query' => ['symbol' => 'ACME']]]
    );
}

/** @return list<array> */
function ac_exchanges(BackendTrace $trace): array
{
    $found = [];
    foreach ($trace->events() as $event) {
        if (\is_array($event['exchange'] ?? null)) {
            $found[] = $event['exchange'];
        }
    }
    return $found;
}

// A router that echoes the method and the request body back as JSON, so the
// recorded exchange can be checked against a known shape.
$router = sys_get_temp_dir() . '/reproit-php-autocapture-router-' . getmypid() . '.php';
file_put_contents($router, <<<'ROUTER'
<?php
header('Content-Type: application/json');
$raw = file_get_contents('php://input');
echo json_encode([
    'method' => $_SERVER['REQUEST_METHOD'],
    'symbol' => $_GET['symbol'] ?? null,
    'echo' => $raw === '' ? null : json_decode($raw, true),
]);
ROUTER);

$port = ac_free_port();
$server = ac_start_server($port, $router);
$base = 'http://127.0.0.1:' . $port;

try {
    // install is idempotent and does not touch the trace on its own.
    StreamCapture::install();
    check(StreamCapture::installed(), 'the wrapper installs over http/https');
    \ReproitBackend\install();
    check(StreamCapture::installed(), 'a second install is a no-op, not a double register');

    // CAPTURE: a plain file_get_contents on an http:// URL, no per-call change.
    $trace = ac_trace();
    Instrument::setTrace($trace);
    $body = file_get_contents($base . '/quote?symbol=ACME');
    check($body !== false, 'the app gets the real response bytes through the wrapper');
    $decoded = json_decode((string) $body, true);
    check_same('GET', $decoded['method'] ?? null, 'the real GET reached the server');
    check_same('ACME', $decoded['symbol'] ?? null, 'the query rode through untouched');

    $exchanges = ac_exchanges($trace);
    check_same(1, \count($exchanges), 'exactly one exchange auto-recorded on the ambient trace');
    $exchange = $exchanges[0];
    check_same('http', $exchange['protocol'] ?? null, 'the exchange is tagged http');
    check_same('GET', $exchange['request']['method'] ?? null, 'recorded request method');
    check_same(
        $base . '/quote?symbol=ACME',
        $exchange['request']['url'] ?? null,
        'recorded request url'
    );
    check_same(200, $exchange['response']['status'] ?? null, 'recorded response status');
    check_same('ACME', $exchange['response']['body']['symbol'] ?? null, 'recorded response body');
    Instrument::setTrace(null);

    // CAPTURE with a context: POST method, headers, and a secret-shaped body.
    // Proves the recording path is the SAME as Instrument::http, redaction and
    // all: the request body's secret is redacted at source.
    $trace = ac_trace();
    Instrument::setTrace($trace);
    $context = stream_context_create(['http' => [
        'method' => 'POST',
        'header' => "Content-Type: application/json\r\nX-Trace: t-1",
        'content' => json_encode(['item' => 'widget', 'apiKey' => 'sk-live-leak']),
    ]]);
    $body = file_get_contents($base . '/order', false, $context);
    $decoded = json_decode((string) $body, true);
    check_same('POST', $decoded['method'] ?? null, 'the real POST reached the server');
    check_same('widget', $decoded['echo']['item'] ?? null, 'the request body rode through');
    $exchange = ac_exchanges($trace)[0] ?? null;
    check(\is_array($exchange), 'the POST auto-records an exchange');
    check_same('POST', $exchange['request']['method'] ?? null, 'recorded POST method');
    check_same(
        true,
        $exchange['request']['body']['apiKey']['$reproit']['redacted'] ?? null,
        'the request body secret is redacted at source, same path as Instrument::http'
    );
    check_same('widget', $exchange['request']['body']['item'] ?? null, 'the non-secret field kept');
    Instrument::setTrace(null);

    // With no ambient trace the wrapper still passes the request through and
    // records nothing, exactly like the explicit boundary off-trace.
    $body = file_get_contents($base . '/quote?symbol=NONE');
    check($body !== false, 'off-trace requests still pass through');

    \ReproitBackend\uninstall();
    check(!StreamCapture::installed(), 'uninstall restores the builtin wrappers');
    // After uninstall a fetch still works on the builtin wrapper.
    $body = file_get_contents($base . '/quote?symbol=BACK');
    check($body !== false, 'the builtin wrapper serves again after uninstall');
} finally {
    proc_terminate($server);
    @unlink($router);
    if (StreamCapture::installed()) {
        \ReproitBackend\uninstall();
    }
}

// REPLAY: a child process with REPROIT_REPLAY set serves the recorded body
// through the wrapper with NO server running (proof of no socket), and a
// diverging URL fails closed with DivergenceError. Run in children because the
// replay session and the pinned envelope are process-scoped (loaded once).
$capture = [
    'format' => 'reproit-backend-capture',
    'version' => 2,
    'operation' => 'GET /quote',
    'oracle' => 'backend-server-error',
    'envelope' => [
        'observedAtMs' => 1753747200000,
        'tz' => 'UTC',
        'runtime' => 'php',
        'replaySeed' => '00ff00ff00ff00ff',
    ],
    'events' => [
        ['kind' => 'start', 'operation' => 'GET /quote', 'sequence' => 1],
        [
            'kind' => 'effect', 'effect' => 'call', 'sequence' => 2,
            'exchange' => [
                'protocol' => 'http',
                'request' => [
                    'method' => 'GET',
                    'url' => 'http://pricing.internal/prices?tier=gold',
                ],
                'response' => [
                    'status' => 200,
                    'headers' => ['content-type' => 'application/json'],
                    'body' => ['prices' => [1, 2, 3]],
                ],
            ],
        ],
        ['kind' => 'return', 'status' => 200, 'success' => true, 'sequence' => 3],
    ],
];
$capturePath = sys_get_temp_dir() . '/reproit-php-autocapture-replay-' . getmypid() . '.json';
file_put_contents($capturePath, json_encode($capture));

/** Run a child that installs the wrapper in replay mode and runs $php. */
$run_child = function (string $php) use ($capturePath): array {
    $script = 'putenv("REPROIT_REPLAY=' . $capturePath . '");'
        . 'require "' . __DIR__ . '/../reproit.php";'
        . 'ReproitBackend\\install();'
        . $php;
    $descriptors = [1 => ['pipe', 'w'], 2 => ['pipe', 'w']];
    $process = proc_open([PHP_BINARY, '-r', $script], $descriptors, $pipes);
    $out = stream_get_contents($pipes[1]);
    $err = stream_get_contents($pipes[2]);
    fclose($pipes[1]);
    fclose($pipes[2]);
    proc_close($process);
    return ['out' => (string) $out, 'err' => (string) $err];
};

// The recorded URL is served with no server anywhere: proof of no socket.
$served = $run_child(
    '$b = file_get_contents("http://pricing.internal/prices?tier=gold");'
    . 'echo $b;'
);
check_same(
    ['prices' => [1, 2, 3]],
    json_decode($served['out'], true),
    'replay serves the recorded body through the wrapper with no socket'
);

// A diverging URL fails closed: DivergenceError, and the structured marker on
// stderr, exactly like Instrument::http's 599 in the explicit boundary.
$diverged = $run_child(
    'try {'
    . '  file_get_contents("http://pricing.internal/unknown");'
    . '  echo "NO-THROW";'
    . '} catch (\\ReproitBackend\\DivergenceError $e) {'
    . '  echo "DIVERGENCE";'
    . '}'
);
check_same('DIVERGENCE', trim($diverged['out']), 'a diverging URL fails closed (DivergenceError)');
check(
    str_contains($diverged['err'], \ReproitBackend\DIVERGENCE_MARKER),
    'the structured divergence marker is emitted, same as the explicit boundary'
);

@unlink($capturePath);

report('autocapture_test');
