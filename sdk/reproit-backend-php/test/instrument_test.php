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
