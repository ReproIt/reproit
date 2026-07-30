<?php

/*!
 * PHP hermetic acceptance fixture, mirroring the Node, Rust, and Ruby money
 * tests.
 *
 * The /quote operation 500s because an upstream pricing service returns
 * {"prices": null} and the handler indexes into it.
 *
 * MODE=capture: boots the upstream plus the app, fires the failing request,
 * and writes a version-2 reproit-backend-capture (exchanges + envelope) to
 * CAPTURE_OUT. Default (server) mode: serves ONLY the app on $PORT; with
 * REPROIT_REPLAY set the SDK serves the recorded exchanges, so no upstream
 * and no database exist. FIXED=1 applies the fix.
 */

declare(strict_types=1);

namespace ReproitBackend\Fixture;

use ReproitBackend\BackendTrace;
use ReproitBackend\Instrument;

use function ReproitBackend\canonical_json;

require_once __DIR__ . '/../reproit.php';

const UPSTREAM_PORT = 19991;
const APP_PORT = 19990;

/**
 * A database stand-in that must never be reached for real: in capture mode a
 * canned result stands in for a live driver; in replay mode the SDK serves
 * the recorded exchange before this closure ever runs.
 */
function load_issuer(string $symbol): array
{
    return Instrument::db(
        'SELECT id, symbol FROM issuers WHERE symbol = $1',
        [$symbol],
        function () use ($symbol): array {
            if (getenv('MODE') !== 'capture') {
                throw new \RuntimeException('live database reached during hermetic replay');
            }
            return [
                'command' => 'SELECT',
                'rowCount' => 1,
                'rows' => [['id' => 7, 'symbol' => $symbol]],
            ];
        }
    );
}

/** @return array{0: int, 1: array} */
function quote(string $symbol): array
{
    try {
        load_issuer($symbol);
        $response = Instrument::http(
            'GET',
            'http://127.0.0.1:' . UPSTREAM_PORT . '/prices?tier=gold'
        );
        $body = $response->json();
        $prices = \is_array($body) ? ($body['prices'] ?? null) : null;
        if (getenv('FIXED') === '1' && !\is_array($prices)) {
            return [200, ['first' => null, 'note' => 'no prices available']];
        }
        if (!\is_array($prices)) {
            throw new \RuntimeException('prices is not a list');
        }
        return [200, ['first' => $prices[0] ?? null]];
    } catch (\Throwable) {
        return [500, ['error' => 'internal']];
    }
}

/** Serve one request from an already-accepted socket, returning the status. */
function serve_connection($connection, ?object $capture): void
{
    $request = fread($connection, 8192);
    if (!\is_string($request) || $request === '') {
        fclose($connection);
        return;
    }
    $line = strtok($request, "\r\n");
    $parts = explode(' ', (string) $line);
    $target = $parts[1] ?? '/';
    $path = parse_url($target, PHP_URL_PATH) ?: '/';
    parse_str((string) parse_url($target, PHP_URL_QUERY), $query);

    // Only /quote runs the handler: a readiness probe on any other path must
    // not consume recorded exchanges, which would diverge the replay before
    // the request under test even arrives.
    if ($path !== '/quote') {
        $body = '{"error":"not found"}';
        fwrite($connection, "HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\n"
            . 'content-length: ' . \strlen($body) . "\r\nconnection: close\r\n\r\n" . $body);
        fclose($connection);
        return;
    }

    $trace = null;
    if ($capture !== null) {
        $trace = BackendTrace::begin($capture->context(), 'GET /quote', [
            'input' => ['query' => $query],
        ]);
        Instrument::setTrace($trace);
    }
    [$status, $output] = quote((string) ($query['symbol'] ?? ''));
    $body = json_encode($output);
    if ($trace !== null) {
        $trace->finish($output, $status, $status < 500, true);
        Instrument::setTrace(null);
        $capture->record($trace);
    }
    fwrite($connection, "HTTP/1.1 $status OK\r\ncontent-type: application/json\r\n"
        . 'content-length: ' . \strlen((string) $body) . "\r\nconnection: close\r\n\r\n" . $body);
    fclose($connection);
}

/**
 * Capture sink that writes the replayable payload to disk instead of
 * uploading, so the acceptance script needs no cloud.
 */
final class FileCapture
{
    public function context(): array
    {
        return [
            'traceId' => 'cap-php-1',
            'actor' => null,
            'actionIndex' => 0,
            'build' => 'php-fixture',
            'configContract' => null,
            'captureEnvelope' => true,
        ];
    }

    public function record(BackendTrace $trace): void
    {
        $events = $trace->events();
        $payload = [
            'format' => 'reproit-backend-capture',
            'version' => 2,
            'operation' => $events[0]['operation'],
            'oracle' => 'backend-server-error',
            'envelope' => [
                'observedAtMs' => (int) (microtime(true) * 1000),
                'tz' => date_default_timezone_get(),
                'runtime' => 'php ' . PHP_VERSION,
                'replaySeed' => 'c0ffee00c0ffee00',
            ],
            'events' => $events,
        ];
        file_put_contents((string) getenv('CAPTURE_OUT'), canonical_json($payload));
    }
}

if (getenv('MODE') === 'capture') {
    // The upstream runs in a child process so one script produces the whole
    // capture without any external service.
    $upstream = proc_open(
        [PHP_BINARY, '-r', '$s=stream_socket_server("tcp://127.0.0.1:' . UPSTREAM_PORT
            . '");while($c=@stream_socket_accept($s,10)){fread($c,4096);'
            . '$b=\'{"prices":null}\';fwrite($c,"HTTP/1.1 200 OK\r\n'
            . 'content-type: application/json\r\ncontent-length: ".strlen($b)."\r\n'
            . 'connection: close\r\n\r\n".$b);fclose($c);}'],
        [1 => ['file', '/dev/null', 'w'], 2 => ['file', '/dev/null', 'w']],
        $pipes
    );
    usleep(300000);
    $server = stream_socket_server('tcp://127.0.0.1:' . APP_PORT, $errno, $errstr);
    $client = proc_open(
        [PHP_BINARY, '-r', 'echo @file_get_contents("http://127.0.0.1:' . APP_PORT
            . '/quote?symbol=ACME");'],
        [1 => ['pipe', 'w'], 2 => ['file', '/dev/null', 'w']],
        $clientPipes
    );
    $connection = stream_socket_accept($server, 10);
    if ($connection !== false) {
        serve_connection($connection, new FileCapture());
    }
    fclose($clientPipes[1]);
    proc_close($client);
    fclose($server);
    proc_terminate($upstream);
    proc_close($upstream);
    fwrite(STDERR, "capture fixture done\n");
    exit(0);
}

$port = (int) (getenv('PORT') ?: APP_PORT);
$server = stream_socket_server('tcp://127.0.0.1:' . $port, $errno, $errstr);
if ($server === false) {
    fwrite(STDERR, "fixture cannot listen on $port: $errstr\n");
    exit(1);
}
while (true) {
    $connection = @stream_socket_accept($server, 30);
    if ($connection === false) {
        continue;
    }
    serve_connection($connection, null);
}
