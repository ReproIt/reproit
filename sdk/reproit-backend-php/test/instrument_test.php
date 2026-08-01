<?php

/*!
 * Outbound-exchange capture and hermetic replay tests for the PHP SDK.
 *
 * Run: php test/instrument_test.php
 */

declare(strict_types=1);

namespace ReproitBackend\Test;

use ReproitBackend\BackendTrace;
use ReproitBackend\Capture;
use ReproitBackend\Instrument;

use function ReproitBackend\bounded_body;
use function ReproitBackend\bounded_headers;
use function ReproitBackend\http_exchange;

use const ReproitBackend\MAX_EXCHANGE_BODY_BYTES;
use const ReproitBackend\MAX_EXCHANGE_HEADERS;

require_once __DIR__ . '/support.php';
require_once __DIR__ . '/../reproit.php';

/** Streams preset chunks one per read: the shape an SSE upstream delivers. */
final class ChunkStream implements \Psr\Http\Message\StreamInterface
{
    private int $at = 0;

    /** @param list<string> $chunks */
    public function __construct(private readonly array $chunks)
    {
    }

    public function read(int $length): string
    {
        if ($this->at >= \count($this->chunks)) {
            return '';
        }
        $chunk = $this->chunks[$this->at];
        $this->at += 1;
        return $chunk;
    }

    public function eof(): bool
    {
        return $this->at >= \count($this->chunks);
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
        $rest = implode('', \array_slice($this->chunks, $this->at));
        $this->at = \count($this->chunks);
        return $rest;
    }

    public function close(): void
    {
    }

    public function detach()
    {
        return null;
    }

    public function getSize(): ?int
    {
        return \strlen(implode('', $this->chunks));
    }

    public function tell(): int
    {
        return 0;
    }

    public function getMetadata(?string $key = null)
    {
        return null;
    }
}

/** Just enough PSR-7 response for the decorator's capture tee. */
final class StubResponse implements \Psr\Http\Message\ResponseInterface
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

/** Inner PSR-18 client answering with a preset response. */
final class StubClient implements \Psr\Http\Client\ClientInterface
{
    public function __construct(private readonly StubResponse $response)
    {
    }

    public function sendRequest(
        \Psr\Http\Message\RequestInterface $request,
    ): \Psr\Http\Message\ResponseInterface {
        return $this->response;
    }
}

/** Just enough PSR-7 request for the decorator: method, URI, body, headers. */
final class StubRequest implements \Psr\Http\Message\RequestInterface
{
    public function __construct(
        private readonly string $method,
        private readonly string $url,
        private readonly string $body = '',
        private readonly array $headers = [],
    ) {
    }

    public function getMethod(): string
    {
        return $this->method;
    }

    public function getUri(): StubUri
    {
        return new StubUri($this->url);
    }

    public function getHeaders(): array
    {
        return array_map(fn (string $value): array => [$value], $this->headers);
    }

    public function getHeaderLine(string $name): string
    {
        return $this->headers[strtolower($name)] ?? '';
    }

    public function getBody(): ChunkStream
    {
        return new ChunkStream($this->body === '' ? [] : [$this->body]);
    }
}

final class StubUri
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

function trace_with_envelope(): BackendTrace
{
    return BackendTrace::begin(
        [
            'traceId' => 'cap-x-1',
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
function recorded_exchanges(BackendTrace $trace): array
{
    $found = [];
    foreach ($trace->events() as $event) {
        if (\is_array($event['exchange'] ?? null)) {
            $found[] = $event['exchange'];
        }
    }
    return $found;
}

// bounds: an oversized body keeps provable identity only
$big = str_repeat('x', MAX_EXCHANGE_BODY_BYTES + 1);
$bounded = bounded_body($big, 'text/plain');
check_same(true, $bounded['truncated'], 'oversized body is marked truncated');
check_same(\strlen($big), $bounded['bodyBytes'], 'oversized body keeps its byte count');
check(
    preg_match('/^[0-9a-f]{64}$/', $bounded['bodySha256']) === 1,
    'oversized body keeps a sha256'
);
check(!isset($bounded['body']), 'oversized body drops its content');

// bounds: headers are capped
$many = [];
for ($index = 0; $index < MAX_EXCHANGE_HEADERS + 10; $index++) {
    $many['x-header-' . $index] = (string) $index;
}
check_same(
    MAX_EXCHANGE_HEADERS,
    \count(bounded_headers($many)['headers']),
    'headers are capped at the shared bound'
);

// redaction applies INSIDE captured exchange bodies
$trace = trace_with_envelope();
Instrument::setTrace($trace);
Instrument::record('call', 'pricing', 'GET /prices', http_exchange(
    ['method' => 'GET', 'url' => 'http://pricing/prices', 'headers' => [], 'body' => null,
     'contentType' => ''],
    [
        'status' => 200,
        'headers' => ['content-type' => 'application/json'],
        'body' => json_encode(['prices' => [1, 2], 'apiKey' => 'sk-live-secret']),
        'contentType' => 'application/json',
    ]
));
$exchange = recorded_exchanges($trace)[0] ?? null;
check(\is_array($exchange), 'exchange recorded on the ambient trace');
check_same(200, $exchange['response']['status'], 'response status recorded');
check_same([1, 2], $exchange['response']['body']['prices'], 'response body recorded');
check_same(
    true,
    $exchange['response']['body']['apiKey']['$reproit']['redacted'],
    'secrets inside exchange bodies are redacted at source'
);

// the database boundary records rows and errors
$trace = trace_with_envelope();
Instrument::setTrace($trace);
Instrument::db('SELECT id FROM issuers WHERE symbol = $1', ['ACME'], fn () => [
    'command' => 'SELECT', 'rowCount' => 1, 'rows' => [['id' => 7]],
]);
try {
    Instrument::db('SELECT boom', null, function (): array {
        throw new \RuntimeException('relation missing');
    });
} catch (\RuntimeException) {
    // recorded below
}
$exchanges = recorded_exchanges($trace);
check_same(2, \count($exchanges), 'both database statements recorded');
check_same('db', $exchanges[0]['protocol'], 'database protocol tagged');
check_same(['ACME'], $exchanges[0]['request']['values'], 'bound values recorded');
check_same([['id' => 7]], $exchanges[0]['response']['rows'], 'rows recorded');
check_same(
    'relation missing',
    $exchanges[1]['response']['error']['message'],
    'a driver error is recorded, not swallowed'
);
Instrument::setTrace(null);

// PSR-18 decoration: the response body is TEED, not drained. Chunk
// boundaries are recorded exactly as the app consumed them (the SSE / LLM
// streaming shape), the request line and body ride the exchange, and the
// record lands at the moment the app observes EOF.
$trace = trace_with_envelope();
Instrument::setTrace($trace);
$sse = new StubResponse(
    200,
    ['content-type' => 'text/event-stream'],
    new ChunkStream(["data: a\n\n", "data: b\n\n", "data: c\n\n"])
);
$client = new \ReproitBackend\Psr18\RecordingClient(new StubClient($sse));
$request = new StubRequest(
    'POST',
    'http://llm.internal/v1/stream',
    '{"model":"m","apiKey":"sk-live-leak"}',
    ['content-type' => 'application/json']
);
$teed = $client->sendRequest($request);
check_same([], recorded_exchanges($trace), 'nothing recorded before the app reads');
$seen = [];
$body = $teed->getBody();
while (!$body->eof()) {
    $chunk = $body->read(65536);
    if ($chunk !== '') {
        $seen[] = $chunk;
    }
}
check_same(
    ["data: a\n\n", "data: b\n\n", "data: c\n\n"],
    $seen,
    'the app reads the live stream through the tee untouched'
);
$exchange = recorded_exchanges($trace)[0] ?? null;
check(\is_array($exchange), 'the exchange records at EOF');
check_same('http', $exchange['protocol'] ?? null, 'psr18 exchange protocol');
check_same('POST', $exchange['request']['method'], 'request method recorded');
check_same('http://llm.internal/v1/stream', $exchange['request']['url'], 'url recorded');
check_same(
    true,
    $exchange['request']['body']['apiKey']['$reproit']['redacted'] ?? null,
    'request body secrets redacted at source'
);
check_same("data: a\n\ndata: b\n\ndata: c\n\n", $exchange['response']['body'], 'body verbatim');
check_same([9, 9, 9], $exchange['response']['stream']['chunks'], 'observed chunk boundaries');

// An abandoned body records nothing, exactly like a response nobody reads.
$before = \count(recorded_exchanges($trace));
$client->sendRequest(new StubRequest('GET', 'http://llm.internal/ignored'));
check_same(
    $before,
    \count(recorded_exchanges($trace)),
    'an abandoned response body records no exchange'
);
Instrument::setTrace(null);

// PDO wrap: statements record the Node pg wire shape and the app observes
// exactly the recorded rows. Guarded on the sqlite driver so the suite stays
// dependency-free; the skip is loud, never silent.
if (\in_array('sqlite', \PDO::getAvailableDrivers(), true)) {
    $trace = trace_with_envelope();
    $pdo = new \ReproitBackend\RecordingPdo('sqlite::memory:');
    $pdo->exec('CREATE TABLE issuers (id INTEGER PRIMARY KEY, symbol TEXT)');
    $pdo->exec("INSERT INTO issuers (id, symbol) VALUES (7, 'ACME')");
    Instrument::setTrace($trace);
    $statement = $pdo->prepare('SELECT id, symbol FROM issuers WHERE symbol = ?');
    $statement->execute(['ACME']);
    $rows = $statement->fetchAll();
    $failed = false;
    try {
        $pdo->query('SELECT boom FROM nowhere');
    } catch (\PDOException) {
        $failed = true;
    }
    Instrument::setTrace(null);
    check_same([['id' => 7, 'symbol' => 'ACME']], $rows, 'pdo app rows are the driver rows');
    $exchanges = recorded_exchanges($trace);
    check_same(2, \count($exchanges), 'both pdo statements recorded');
    check_same('pg', $exchanges[0]['protocol'], 'pdo exchanges use the pg wire shape');
    check_same(
        'SELECT id, symbol FROM issuers WHERE symbol = ?',
        $exchanges[0]['request']['text'],
        'statement text recorded'
    );
    check_same(['ACME'], $exchanges[0]['request']['values'], 'bound values recorded');
    check_same('SELECT', $exchanges[0]['response']['command'], 'command tag recorded');
    check_same(
        [['id' => 7, 'symbol' => 'ACME']],
        $exchanges[0]['response']['rows'],
        'recorded rows equal the rows the app saw'
    );
    check($failed, 'a driver error still raises to the app');
    check(
        \is_string($exchanges[1]['response']['error']['message'] ?? null),
        'the driver error is recorded, not swallowed'
    );
} else {
    fwrite(STDERR, "SKIP: pdo_sqlite driver unavailable; PDO capture cases not run\n");
}

// capture mode stamps the envelope; scan-time traces stay byte-stable
$stamped = trace_with_envelope();
$allStamped = true;
foreach ($stamped->events() as $event) {
    if (!\is_int($event['at'] ?? null) || !\is_int($event['monoNs'] ?? null)) {
        $allStamped = false;
    }
}
check($allStamped, 'capture-mode events carry wall-clock and monotonic stamps');
$scan = BackendTrace::begin(
    ['traceId' => 'trace-a', 'actor' => null, 'actionIndex' => 0, 'build' => null,
     'configContract' => null],
    'op'
);
$scanClean = true;
foreach ($scan->events() as $event) {
    if (\array_key_exists('at', $event) || \array_key_exists('monoNs', $event)) {
        $scanClean = false;
    }
}
check($scanClean, 'scan-time events carry no envelope stamps');

// the batch declares `network` only when exchanges exist
$capture = Capture::create([
    'endpoint' => 'http://c/v1/capture-batches', 'apiKey' => 'sk', 'appId' => 'app-demo',
]);
$withExchange = BackendTrace::begin($capture->context(), 'GET /quote', ['input' => null]);
$withExchange->effect('call', [
    'resource' => 'pricing',
    'key' => 'GET /prices',
    'exchange' => [
        'protocol' => 'http',
        'request' => ['method' => 'GET', 'url' => 'http://pricing/prices'],
        'response' => ['status' => 200, 'body' => ['prices' => null]],
    ],
]);
$withExchange->finish(['error' => 'boom'], 500, false, true);
$batch = $capture->buildBatch([
    ['operation' => 'GET /quote', 'status' => 500, 'events' => $withExchange->events()],
]);
$names = array_map(fn (array $entry) => $entry['capability'], $batch['capabilities']);
check(\in_array('network', $names, true), 'network capability declared with exchanges');

$plain = BackendTrace::begin($capture->context(), 'GET /quote', ['input' => null]);
$plain->effect('read', ['resource' => 'inventory', 'key' => 'widget']);
$plain->finish(null, 500, false, true);
$bare = $capture->buildBatch([
    ['operation' => 'GET /quote', 'status' => 500, 'events' => $plain->events()],
]);
$bareNames = array_map(fn (array $entry) => $entry['capability'], $bare['capabilities']);
check(
    !\in_array('network', $bareNames, true),
    'no network capability without recorded exchanges'
);

report('instrument_test');
