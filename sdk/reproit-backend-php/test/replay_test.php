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
        [
            'kind' => 'effect', 'effect' => 'call', 'sequence' => 4,
            'exchange' => [
                'protocol' => 'http',
                'request' => ['method' => 'GET', 'url' => 'http://llm.internal/stream'],
                'response' => [
                    'status' => 200,
                    'headers' => ['content-type' => 'text/event-stream'],
                    'body' => "data: a\n\ndata: b\n\ndata: c\n\n",
                    'stream' => ['chunks' => [9, 9, 9]],
                ],
            ],
        ],
        [
            'kind' => 'effect', 'effect' => 'read', 'sequence' => 5,
            'exchange' => [
                'protocol' => 'pg',
                'request' => [
                    'text' => 'SELECT id, name FROM accounts WHERE id = $1',
                    'values' => [7],
                ],
                'response' => [
                    'command' => 'SELECT',
                    'rowCount' => 1,
                    'rows' => [['id' => 7, 'name' => 'acme']],
                ],
            ],
        ],
        ['kind' => 'return', 'status' => 500, 'success' => false, 'sequence' => 6],
    ],
];

$path = sys_get_temp_dir() . '/reproit-php-replay-' . getmypid() . '.json';
file_put_contents($path, json_encode($capture));
putenv('REPROIT_REPLAY=' . $path);

require_once __DIR__ . '/support.php';
require_once __DIR__ . '/../reproit.php';

/** PSR-18 inner client that must never be dialed during replay. */
final class NeverClient implements \Psr\Http\Client\ClientInterface
{
    public function sendRequest(
        \Psr\Http\Message\RequestInterface $request,
    ): \Psr\Http\Message\ResponseInterface {
        throw new \RuntimeException('inner PSR-18 client dialed during hermetic replay');
    }
}

/** Just enough PSR-7 request for the decorator: method, URI, body, headers. */
final class FakeRequest implements \Psr\Http\Message\RequestInterface
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

    public function getUri(): FakeUri
    {
        return new FakeUri($this->url);
    }

    public function getHeaders(): array
    {
        return array_map(fn (string $value): array => [$value], $this->headers);
    }

    public function getHeaderLine(string $name): string
    {
        return $this->headers[strtolower($name)] ?? '';
    }

    public function getBody(): FakeBody
    {
        return new FakeBody($this->body);
    }
}

final class FakeUri
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

final class FakeBody
{
    private int $at = 0;

    public function __construct(private readonly string $content)
    {
    }

    public function isSeekable(): bool
    {
        return true;
    }

    public function rewind(): void
    {
        $this->at = 0;
    }

    public function getSize(): int
    {
        return \strlen($this->content);
    }

    public function getContents(): string
    {
        $rest = substr($this->content, $this->at);
        $this->at = \strlen($this->content);
        return $rest;
    }

    public function __toString(): string
    {
        return $this->content;
    }
}

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

// The clock is NOT pinned, and that is a measured platform limit rather than
// an oversight: redeclaring an internal is fatal, and PHP's namespaced
// fallback shadows only calls made from inside the SDK's own namespace, so it
// can never reach the application's time(). This test pins BOTH halves of the
// documented contract so neither can silently regress.
check(
    abs((time() * 1000) - 1_753_747_200_000) > 60_000,
    'the wall clock is honestly NOT pinned (documented platform limit)'
);
check_same(
    1_753_747_200_000,
    Instrument::replayObservedAtMs(),
    'the capture instant is exposed for apps that must anchor time themselves'
);
check_same(
    'Europe/Berlin',
    date_default_timezone_get(),
    'the timezone IS pinned from the envelope'
);

// The seeded Mersenne stream is deterministic across replay runs: two fresh
// processes pinned to the same envelope draw the same mt_rand sequence.
// random_bytes/random_int stay CSPRNG and unpinnable; that gap is named in
// pin_envelope rather than papered over.
$draw = function () use ($path): string {
    $script = 'putenv("REPROIT_REPLAY=' . $path . '");'
        . 'require "' . __DIR__ . '/../reproit.php";'
        . 'ReproitBackend\\Instrument::session();'
        . 'echo mt_rand(), ":", mt_rand();';
    $process = proc_open([PHP_BINARY, '-r', $script], [1 => ['pipe', 'w']], $pipes);
    $out = stream_get_contents($pipes[1]);
    fclose($pipes[1]);
    proc_close($process);
    return (string) $out;
};
$first = $draw();
check($first !== '' && str_contains($first, ':'), 'seeded mt_rand draws');
check_same($first, $draw(), 'mt_rand sequence is identical across replay runs');

// The SDK clock seam: pinned to the capture instant in replay mode.
$clock = Instrument::clock();
check(
    abs($clock->nowMs() - 1_753_747_200_000) < 60_000,
    'Instrument::clock() is pinned to the capture instant during replay'
);

// A recorded SSE exchange replays as ONE logical exchange with the captured
// chunk boundaries, through the PSR-18 decorator with no inner client call.
$client = new \ReproitBackend\Psr18\RecordingClient(new NeverClient());
$response = $client->sendRequest(new FakeRequest('GET', 'http://llm.internal/stream'));
check_same(200, $response->getStatusCode(), 'replayed SSE status');
$body = $response->getBody();
$chunks = [];
while (!$body->eof()) {
    $chunk = $body->read(65536);
    if ($chunk !== '') {
        $chunks[] = $chunk;
    }
}
check_same(
    ["data: a\n\n", "data: b\n\n", "data: c\n\n"],
    $chunks,
    'the recorded chunk boundaries are re-served chunk for chunk'
);

// The PDO connect stub: the app boots with the database down, statements are
// served from the capture, and an unseen statement fails closed.
$pdo = new \ReproitBackend\RecordingPdo('pgsql:host=db.internal;dbname=accounts');
$statement = $pdo->prepare('SELECT id, name FROM accounts WHERE id = $1');
$statement->execute([7]);
check_same(
    [['id' => 7, 'name' => 'acme']],
    $statement->fetchAll(),
    'recorded pg rows served without a driver'
);
check_same(1, $statement->rowCount(), 'recorded rowCount served');
$pdoDiverged = false;
try {
    $pdo->query('SELECT never_recorded FROM nowhere');
} catch (DivergenceError) {
    $pdoDiverged = true;
}
check($pdoDiverged, 'an unseen statement through PDO fails closed');

// Prompt drift: a diverging chat-shaped body names the first differing
// message index in the marker's bodyDelta; the byte fallback names the
// offset. Fresh process so its session still holds the recorded exchange.
$driftLog = sys_get_temp_dir() . '/reproit-php-replay-drift-' . getmypid() . '.txt';
$drift = [
    'format' => 'reproit-backend-capture',
    'version' => 2,
    'operation' => 'POST /chat',
    'oracle' => 'backend-server-error',
    'events' => [[
        'kind' => 'effect', 'effect' => 'call', 'sequence' => 1,
        'exchange' => [
            'protocol' => 'http',
            'request' => [
                'method' => 'POST',
                'url' => 'http://llm.internal/v1/chat',
                'body' => ['messages' => [
                    ['role' => 'user', 'content' => 'hello'],
                    ['role' => 'assistant', 'content' => 'hi'],
                    ['role' => 'user', 'content' => 'weather?'],
                ]],
            ],
            'response' => ['status' => 200, 'body' => ['reply' => 'sunny']],
        ],
    ]],
];
$driftPath = sys_get_temp_dir() . '/reproit-php-replay-drift-' . getmypid() . '.json';
file_put_contents($driftPath, json_encode($drift));
$script = 'putenv("REPROIT_REPLAY=' . $driftPath . '");'
    . 'require "' . __DIR__ . '/../reproit.php";'
    . '$probe = ["method" => "POST", "url" => "http://llm.internal/v1/chat",'
    . ' "body" => ["messages" => [["role" => "user", "content" => "hello"],'
    . ' ["role" => "assistant", "content" => "hi"],'
    . ' ["role" => "user", "content" => "DIFFERENT"]]]];'
    . 'ReproitBackend\\serve_http(ReproitBackend\\Instrument::session(), $probe);';
$process = proc_open(
    [PHP_BINARY, '-r', $script],
    [1 => ['file', '/dev/null', 'w'], 2 => ['file', $driftLog, 'w']],
    $pipes
);
proc_close($process);
$marker = null;
foreach (explode("\n", (string) file_get_contents($driftLog)) as $line) {
    if (str_starts_with($line, DIVERGENCE_MARKER)) {
        $marker = $line;
        break;
    }
}
check($marker !== null, 'prompt drift emits the marker');
$report = json_decode(substr((string) $marker, \strlen(DIVERGENCE_MARKER)), true);
check_same('message', $report['bodyDelta']['kind'] ?? null, 'bodyDelta is message shaped');
check_same(
    2,
    $report['bodyDelta']['firstDifferingMessage'] ?? null,
    'bodyDelta names the first differing message index'
);
check_same(3, $report['bodyDelta']['recordedMessages'] ?? null, 'recorded message count');
check_same(3, $report['bodyDelta']['liveMessages'] ?? null, 'live message count');

// Byte fallback: a non-chat body names the first differing byte offset, and
// an ABSENT live body yields no delta at all (absence is not a difference
// the matcher can locate; it is already the divergence itself).
check_same(
    ['kind' => 'byte', 'offset' => 8],
    \ReproitBackend\body_delta('same-oldXbytes', 'same-oldYbytes'),
    'byte fallback names the first differing offset'
);
check_same(
    null,
    \ReproitBackend\body_delta('recorded', \ReproitBackend\Absent::value()),
    'an absent body yields no bodyDelta'
);
check(
    \ReproitBackend\body_delta(null, 'anything') === null,
    'a recorded null body is a wildcard, distinct from absent'
);

@unlink($driftPath);
@unlink($driftLog);
@unlink($path);
@unlink($errorLog);
report('replay_test');

