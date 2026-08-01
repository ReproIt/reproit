<?php

/*!
 * Bounded dependency-exchange records for reproit-backend-php: the request
 * the app sent and the response the dependency returned.
 *
 * PHP port of the shapes in sdk/reproit-backend-node/instrument.js. An
 * exchange is what hermetic replay serves, so responses are captured
 * verbatim up to a fixed inline budget; an over-budget body keeps only
 * provable identity (byte count + sha256) and is marked truncated, and
 * replay fails closed on it with a named reason instead of guessing.
 */

declare(strict_types=1);

namespace ReproitBackend;

/** Inline body budget per exchange side, identical to every other SDK. */
const MAX_EXCHANGE_BODY_BYTES = 8 * 1024;
/** Recorded headers are capped to keep events bounded. */
const MAX_EXCHANGE_HEADERS = 32;
/** Rows recorded per database result; beyond it the result is truncated. */
const MAX_DB_ROWS = 64;
/**
 * Stream chunk boundaries recorded per exchange (SSE / chunked responses,
 * the LLM streaming shape). Beyond it the boundaries are marked truncated
 * and replay fails closed rather than serve a wrong stream shape.
 */
const MAX_STREAM_CHUNKS = 128;

/**
 * Bound one exchange body. Declared JSON is decoded so structural redaction
 * sees fields rather than text. A BodyCollector that overflowed already
 * reduced the body to provable identity (byte count + sha256); that identity
 * array passes through untouched.
 */
function bounded_body(mixed $body, string $contentType): array
{
    if (\is_array($body) && ($body['truncated'] ?? false) === true) {
        return $body;
    }
    if (!\is_string($body) || $body === '') {
        return [];
    }
    if (\strlen($body) > MAX_EXCHANGE_BODY_BYTES) {
        return [
            'bodyBytes' => \strlen($body),
            'bodySha256' => hash('sha256', $body),
            'truncated' => true,
        ];
    }
    if (str_contains($contentType, 'application/json')) {
        $decoded = json_decode($body, true);
        if (json_last_error() === JSON_ERROR_NONE) {
            return ['body' => $decoded];
        }
    }
    return ['body' => $body];
}

/** @param array<string, mixed> $headers */
function bounded_headers(array $headers): array
{
    $lowered = [];
    foreach ($headers as $name => $value) {
        $lowered[strtolower((string) $name)] = \is_array($value)
            ? implode(', ', array_map('strval', $value))
            : (string) $value;
    }
    // Sort BEFORE the cap. Capping arrival order records a different subset
    // whenever the caller's header order shifts, so two runs of one request
    // disagree and the capsule stops matching.
    ksort($lowered, SORT_STRING);
    $bounded = \array_slice($lowered, 0, MAX_EXCHANGE_HEADERS, true);
    return $bounded === [] ? [] : ['headers' => $bounded];
}

/**
 * Collect a stream's chunks up to one byte past the inline budget; enough to
 * know the true size class without holding unbounded memory. The sha256 runs
 * over EVERY byte so truncated identity stays provable. Chunk boundaries are
 * recorded as observed byte lengths, bounded by MAX_STREAM_CHUNKS;
 * boundaries past the cap are counted, never guessed.
 *
 * PHP port of the Node reference's bodyCollector (instrument.js).
 */
final class BodyCollector
{
    private array $chunks = [];
    private array $boundaries = [];
    private int $bytes = 0;
    private int $droppedBoundaries = 0;
    private \HashContext $hash;

    public function __construct()
    {
        $this->hash = hash_init('sha256');
    }

    public function push(string $chunk): void
    {
        $this->bytes += \strlen($chunk);
        hash_update($this->hash, $chunk);
        if (\count($this->boundaries) < MAX_STREAM_CHUNKS) {
            $this->boundaries[] = \strlen($chunk);
        } else {
            $this->droppedBoundaries += 1;
        }
        if ($this->bytes <= MAX_EXCHANGE_BODY_BYTES) {
            $this->chunks[] = $chunk;
        }
    }

    /**
     * The collected body: null when empty, provable identity (an array
     * bounded_body passes through) when over budget, the raw bytes otherwise.
     */
    public function result(): string|array|null
    {
        if ($this->bytes === 0) {
            return null;
        }
        if ($this->bytes > MAX_EXCHANGE_BODY_BYTES) {
            return [
                'bodyBytes' => $this->bytes,
                'bodySha256' => hash_final(clone $this->hash),
                'truncated' => true,
            ];
        }
        return implode('', $this->chunks);
    }

    /**
     * Chunk boundaries as observed byte lengths. Recorded when the response
     * is a stream (SSE always; anything else only when it actually arrived
     * in more than one chunk, since a single-chunk body replays identically
     * without them).
     */
    public function stream(bool $isEventStream): ?array
    {
        if ($this->boundaries === []) {
            return null;
        }
        if (!$isEventStream && \count($this->boundaries) < 2 && $this->droppedBoundaries === 0) {
            return null;
        }
        if ($this->droppedBoundaries > 0) {
            return ['chunks' => $this->boundaries, 'truncated' => true];
        }
        return ['chunks' => $this->boundaries];
    }
}

/**
 * @param array{method: string, url: string, headers: array, body: mixed,
 *              contentType: string} $request
 * @param array{status: int, headers: array, body: mixed,
 *              contentType: string, stream?: ?array} $response
 */
function http_exchange(array $request, array $response): array
{
    $responseBody = bounded_body($response['body'], $response['contentType']);
    $encoded = array_merge(
        ['status' => $response['status']],
        bounded_headers($response['headers']),
        $responseBody
    );
    // Stream shape (SSE / chunked): observed chunk boundaries, so the whole
    // stream is ONE logical exchange and replay can re-serve it chunk for
    // chunk. A truncated inline body already fails closed, so boundaries are
    // only kept for bodies recorded verbatim.
    $stream = $response['stream'] ?? null;
    if (\is_array($stream) && ($responseBody['truncated'] ?? false) !== true) {
        $encoded['stream'] = $stream;
    }
    return [
        'protocol' => 'http',
        'request' => array_merge(
            ['method' => $request['method'], 'url' => $request['url']],
            bounded_headers($request['headers']),
            bounded_body($request['body'], $request['contentType'])
        ),
        'response' => $encoded,
    ];
}

function db_exchange(string $text, ?array $values, array $outcome): array
{
    $request = ['text' => $text];
    if ($values !== null && $values !== []) {
        $request['values'] = array_values($values);
    }
    return ['protocol' => 'db', 'request' => $request, 'response' => $outcome];
}

/** Normalize a driver result into the recorded response shape. */
function db_outcome(mixed $result): array
{
    if (!\is_array($result)) {
        return ['rowCount' => 0];
    }
    $rows = \is_array($result['rows'] ?? null) ? array_values($result['rows']) : [];
    $count = $result['rowCount'] ?? null;
    $outcome = [
        'command' => isset($result['command']) ? (string) $result['command'] : null,
        'rowCount' => \is_int($count) ? $count : \count($rows),
        'rows' => \array_slice($rows, 0, MAX_DB_ROWS),
    ];
    if (\count($rows) > MAX_DB_ROWS) {
        $outcome['truncated'] = true;
    }
    return $outcome;
}

function db_error(\Throwable $error): array
{
    $code = $error->getCode();
    return ['error' => [
        'message' => $error->getMessage(),
        'code' => $code === 0 ? null : (string) $code,
    ]];
}

/**
 * Effect kind for a statement: reads stay reads so state oracles keep their
 * meaning; everything else is a write.
 */
function statement_effect_kind(string $text): string
{
    $verb = strtoupper(substr(ltrim($text), 0, 8));
    return str_starts_with($verb, 'SELECT') || str_starts_with($verb, 'SHOW')
        ? 'read'
        : 'write';
}
