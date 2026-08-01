<?php

/*!
 * Money-test fixture for PHP capsule parity: a `php -S` app with the reproit
 * SDK whose /quote operation 500s because an upstream pricing service
 * returns {"prices": null} and the handler indexes into it. The upstream
 * call goes through a PSR-18 RecordingClient (the decorated boundary) and
 * the database call through RecordingPdo (real sqlite at capture; in replay
 * the constructor is a connect stub, so the app boots with the DB down and
 * a DSN no driver here could even dial).
 *
 * MODE=capture (CLI) boots the upstream plus the app under `php -S`, fires
 * the failing request, and the app writes a version 2
 * reproit-backend-capture (exchanges plus envelope) to CAPTURE_OUT from a
 * shutdown function: the PHP process model serializes the trace to its
 * spool before the request-handling process ends, which is what keeps
 * per-operation ordinals stable (trace, capsule state, and replay session
 * all live inside ONE request lifecycle; `php -S` re-runs this script per
 * request, so every request starts its ordinals from zero by construction).
 *
 * Under `php -S` (the router) with REPROIT_REPLAY set the SDK serves the
 * recorded exchanges in process, so neither the upstream nor the database
 * exists. FIXED=1 applies the fix.
 */

declare(strict_types=1);

namespace ReproitFixture;

use ReproitBackend\BackendTrace;
use ReproitBackend\Instrument;
use ReproitBackend\Psr18\RecordingClient;
use ReproitBackend\RecordingPdo;

use function ReproitBackend\canonical_json;
use function ReproitBackend\determinism_envelope;

require_once __DIR__ . '/../../sdk/reproit-backend-php/reproit.php';

const UPSTREAM_PORT = 19976;
const CAPTURE_PORT = 19975;

// ---------------------------------------------------------------------------
// Minimal PSR-7/PSR-18 fixture plumbing: an inner client over stream
// contexts, the shape any real PSR-18 install (Guzzle, Symfony) provides.
// ---------------------------------------------------------------------------

final class FixtureStream implements \Psr\Http\Message\StreamInterface
{
    private int $at = 0;

    public function __construct(private readonly string $content)
    {
    }

    public function read(int $length): string
    {
        $chunk = substr($this->content, $this->at, max(0, $length));
        $this->at += \strlen($chunk);
        return $chunk;
    }

    public function eof(): bool
    {
        return $this->at >= \strlen($this->content);
    }

    public function isSeekable(): bool
    {
        return true;
    }

    public function rewind(): void
    {
        $this->at = 0;
    }

    public function getContents(): string
    {
        $rest = substr($this->content, $this->at);
        $this->at = \strlen($this->content);
        return $rest;
    }

    public function getSize(): int
    {
        return \strlen($this->content);
    }

    public function tell(): int
    {
        return $this->at;
    }

    public function close(): void
    {
    }

    public function detach()
    {
        return null;
    }

    public function seek(int $offset, int $whence = SEEK_SET): void
    {
        $this->at = $offset;
    }

    public function isReadable(): bool
    {
        return true;
    }

    public function isWritable(): bool
    {
        return false;
    }

    public function write(string $string): int
    {
        throw new \RuntimeException('fixture stream is read only');
    }

    public function getMetadata(?string $key = null)
    {
        return null;
    }

    public function __toString(): string
    {
        return $this->content;
    }
}

final class FixtureUri
{
    public function __construct(private readonly string $url)
    {
    }

    public function getHost(): string
    {
        return (string) (parse_url($this->url, PHP_URL_HOST) ?: '');
    }

    public function __toString(): string
    {
        return $this->url;
    }
}

final class FixtureRequest implements \Psr\Http\Message\RequestInterface
{
    public function __construct(
        private readonly string $method,
        private readonly string $url,
        private readonly array $headers = [],
        private readonly string $body = '',
    ) {
    }

    public function getMethod(): string
    {
        return $this->method;
    }

    public function getUri(): FixtureUri
    {
        return new FixtureUri($this->url);
    }

    public function getHeaders(): array
    {
        return array_map(fn (string $value): array => [$value], $this->headers);
    }

    public function getHeaderLine(string $name): string
    {
        return $this->headers[strtolower($name)] ?? '';
    }

    public function getBody(): FixtureStream
    {
        return new FixtureStream($this->body);
    }
}

final class FixtureResponse implements \Psr\Http\Message\ResponseInterface
{
    public function __construct(
        private readonly int $status,
        private readonly array $headers,
        private object $body,
    ) {
    }

    public function getStatusCode(): int
    {
        return $this->status;
    }

    public function getHeaders(): array
    {
        return array_map(fn (string $value): array => [$value], $this->headers);
    }

    public function getHeaderLine(string $name): string
    {
        return $this->headers[strtolower($name)] ?? '';
    }

    public function getBody(): object
    {
        return $this->body;
    }

    public function withBody(object $body): self
    {
        $clone = clone $this;
        $clone->body = $body;
        return $clone;
    }
}

/** Inner PSR-18 client over stream contexts; RecordingClient decorates it. */
final class FixtureClient implements \Psr\Http\Client\ClientInterface
{
    public function sendRequest(
        \Psr\Http\Message\RequestInterface $request,
    ): \Psr\Http\Message\ResponseInterface {
        $context = stream_context_create(['http' => [
            'method' => $request->getMethod(),
            'content' => (string) $request->getBody(),
            'timeout' => 5.0,
            'ignore_errors' => true,
        ]]);
        $raw = @file_get_contents((string) $request->getUri(), false, $context);
        $status = 0;
        $headers = [];
        foreach ($http_response_header ?? [] as $line) {
            if (preg_match('#^HTTP/\S+\s+(\d{3})#', $line, $matches) === 1) {
                $status = (int) $matches[1];
                continue;
            }
            $split = explode(':', $line, 2);
            if (\count($split) === 2) {
                $headers[strtolower(trim($split[0]))] = trim($split[1]);
            }
        }
        return new FixtureResponse($status, $headers, new FixtureStream(
            $raw === false ? '' : $raw
        ));
    }
}

// ---------------------------------------------------------------------------
// Capture sink: the replayable payload spools to disk from a SHUTDOWN
// function, the PHP-model hand-off (capture.php documents the same design
// for the cloud sampler): the trace lives and dies inside one request.
// ---------------------------------------------------------------------------

final class FileCapture
{
    public function context(): array
    {
        return [
            'traceId' => 'cap-money-php-fixture-1',
            'actor' => null,
            'actionIndex' => 0,
            'build' => 'php-money-fixture',
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
            'envelope' => determinism_envelope(
                \is_int($events[0]['at'] ?? null) ? $events[0]['at'] : null
            ),
            'events' => $events,
        ];
        // Spool before process end: the per-request process model means the
        // capsule must leave the process inside this request's lifecycle.
        register_shutdown_function(function () use ($payload): void {
            file_put_contents((string) getenv('CAPTURE_OUT'), canonical_json($payload));
        });
    }
}

// ---------------------------------------------------------------------------
// The application
// ---------------------------------------------------------------------------

/**
 * The database. Capture mode: real sqlite, seeded OUTSIDE the trace so only
 * the operation's own statement is part of the capsule. Replay: the pgsql
 * DSN could not even connect here (driver absent, host down); RecordingPdo's
 * constructor never dials, which is the point.
 */
function make_database(): RecordingPdo
{
    if (Instrument::replaying()) {
        return new RecordingPdo('pgsql:host=db.internal;dbname=quotes');
    }
    $pdo = new RecordingPdo('sqlite::memory:');
    $pdo->exec('CREATE TABLE issuers (id INTEGER PRIMARY KEY, symbol TEXT)');
    $pdo->exec("INSERT INTO issuers (id, symbol) VALUES (7, 'ACME')");
    return $pdo;
}

/** @return array{0: int, 1: array} status, output */
function quote(RecordingPdo $pdo, string $symbol): array
{
    try {
        $statement = $pdo->prepare('SELECT id, symbol FROM issuers WHERE symbol = ?');
        $statement->execute([$symbol]);
        if ($statement->fetch() === false) {
            return [404, ['error' => 'unknown symbol']];
        }
        $client = new RecordingClient(new FixtureClient());
        $response = $client->sendRequest(new FixtureRequest(
            'GET',
            'http://127.0.0.1:' . UPSTREAM_PORT . '/prices?tier=gold'
        ));
        $body = json_decode($response->getBody()->getContents(), true);
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

/** `php -S` router: one request, one trace, one lifecycle. */
function route(): void
{
    $path = parse_url($_SERVER['REQUEST_URI'] ?? '/', PHP_URL_PATH) ?: '/';
    // Only /quote runs the handler: a readiness probe on any other path
    // must not consume recorded exchanges.
    if ($path !== '/quote') {
        http_response_code(404);
        header('Content-Type: application/json');
        echo '{"error":"not found"}';
        return;
    }
    $database = make_database();
    $trace = null;
    $capture = null;
    if (getenv('MODE') === 'capture') {
        $capture = new FileCapture();
        $trace = BackendTrace::begin($capture->context(), 'GET /quote', [
            'input' => ['query' => ['symbol' => (string) ($_GET['symbol'] ?? 'ACME')]],
        ]);
        Instrument::setTrace($trace);
    }
    [$status, $output] = quote($database, (string) ($_GET['symbol'] ?? 'ACME'));
    if ($trace !== null) {
        $trace->finish($output, $status, $status < 500, true);
        Instrument::setTrace(null);
        $capture->record($trace);
    }
    http_response_code($status);
    header('Content-Type: application/json');
    echo json_encode($output);
}

if (\PHP_SAPI === 'cli-server') {
    route();
    return;
}

// ---------------------------------------------------------------------------
// MODE=capture CLI orchestration: upstream + `php -S` app + one request.
// ---------------------------------------------------------------------------

if (getenv('MODE') !== 'capture') {
    fwrite(STDERR, "usage: MODE=capture CAPTURE_OUT=... php app.php\n"
        . "or serve with: php -S 127.0.0.1:\$PORT app.php\n");
    exit(1);
}

$upstream = proc_open(
    [PHP_BINARY, '-r', '$s=stream_socket_server("tcp://127.0.0.1:' . UPSTREAM_PORT
        . '");while($c=@stream_socket_accept($s,10)){fread($c,4096);'
        . '$b=\'{"prices":null}\';fwrite($c,"HTTP/1.1 200 OK\r\n'
        . 'content-type: application/json\r\ncontent-length: ".strlen($b)."\r\n'
        . 'connection: close\r\n\r\n".$b);fclose($c);}'],
    [1 => ['file', '/dev/null', 'w'], 2 => ['file', '/dev/null', 'w']],
    $upstreamPipes
);
$app = proc_open(
    [PHP_BINARY, '-S', '127.0.0.1:' . CAPTURE_PORT, __FILE__],
    [1 => ['file', '/dev/null', 'w'], 2 => ['file', '/dev/null', 'w']],
    $appPipes,
    __DIR__,
    array_merge($_ENV, [
        'PATH' => (string) getenv('PATH'),
        'MODE' => 'capture',
        'CAPTURE_OUT' => (string) getenv('CAPTURE_OUT'),
    ])
);
$deadline = microtime(true) + 10;
$ready = false;
while (microtime(true) < $deadline && !$ready) {
    $probe = @fsockopen('127.0.0.1', CAPTURE_PORT, $code, $message, 0.25);
    if ($probe !== false) {
        fclose($probe);
        $ready = true;
        break;
    }
    usleep(50000);
}
if (!$ready) {
    fwrite(STDERR, "fixture app did not become ready\n");
    exit(1);
}
$context = stream_context_create(['http' => ['timeout' => 10, 'ignore_errors' => true]]);
@file_get_contents(
    'http://127.0.0.1:' . CAPTURE_PORT . '/quote?symbol=ACME',
    false,
    $context
);
// The capture spools from the app's shutdown function; wait for the file.
$out = (string) getenv('CAPTURE_OUT');
$deadline = microtime(true) + 10;
while (microtime(true) < $deadline && !(is_file($out) && filesize($out) > 0)) {
    usleep(50000);
    clearstatcache(true, $out);
}
proc_terminate($app);
proc_terminate($upstream);
proc_close($app);
proc_close($upstream);
if (!(is_file($out) && filesize($out) > 0)) {
    fwrite(STDERR, "capture was not written\n");
    exit(1);
}
fwrite(STDOUT, "capture fixture status 500\n");
