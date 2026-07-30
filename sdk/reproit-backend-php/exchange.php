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
 * Bound one exchange body. Declared JSON is decoded so structural redaction
 * sees fields rather than text.
 */
function bounded_body(?string $body, string $contentType): array
{
    if ($body === null || $body === '') {
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
    $bounded = [];
    foreach ($headers as $name => $value) {
        if (\count($bounded) >= MAX_EXCHANGE_HEADERS) {
            break;
        }
        $bounded[strtolower((string) $name)] = \is_array($value)
            ? implode(', ', array_map('strval', $value))
            : (string) $value;
    }
    return $bounded === [] ? [] : ['headers' => $bounded];
}

/**
 * @param array{method: string, url: string, headers: array, body: ?string,
 *              contentType: string} $request
 * @param array{status: int, headers: array, body: ?string,
 *              contentType: string} $response
 */
function http_exchange(array $request, array $response): array
{
    return [
        'protocol' => 'http',
        'request' => array_merge(
            ['method' => $request['method'], 'url' => $request['url']],
            bounded_headers($request['headers']),
            bounded_body($request['body'], $request['contentType'])
        ),
        'response' => array_merge(
            ['status' => $response['status']],
            bounded_headers($response['headers']),
            bounded_body($response['body'], $response['contentType'])
        ),
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
