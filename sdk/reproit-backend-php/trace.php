<?php

/*!
 * reproit-backend-php, experimental backend trace adapter (v0.0.0)
 *
 * PHP port of sdk/reproit-backend-rs. Scan-time: services activate this
 * adapter only when a trusted request carries `x-reproit-trace`. The resulting
 * response header (`x-reproit-events`) contains bounded, trace-bound,
 * structurally redacted events. Production: the optional, config-gated capture
 * mode (capture.php) self-samples finished traces (always on 5xx / failure,
 * optional healthy baseline) and posts them to Cloud ingest. It is not a
 * public compatibility surface while backend contracts remain experimental.
 *
 * Wire parity with the Rust adapter: events serialize as compact JSON with
 * recursively sorted keys (serde_json's BTreeMap order), and the header is
 * unpadded base64url of that encoding. JSON objects are stdClass or non-empty
 * associative arrays; an empty PHP array always encodes as `[]`.
 */

declare(strict_types=1);

namespace ReproitBackend;

const MAX_EVENTS = 256;
const MAX_HEADER_BYTES = 60000;
const EFFECT_KINDS = ['read', 'write', 'delete', 'emit', 'call'];

const SECRET_PARTS = [
    'password',
    'passwd',
    'secret',
    'token',
    'authorization',
    'cookie',
    'email',
    'phone',
    'apikey',
    'publishablekey',
    'privatekey',
    'accesskey',
    'signingkey',
    'idempotencykey',
];

final class TraceError extends \RuntimeException
{
    /** `getCode()`: InvalidOperation | AlreadyFinished | TooManyEvents | HeaderTooLarge */
    public function __construct(string $code)
    {
        parent::__construct('reproit trace rejected input: ' . $code);
        $this->code = $code;
    }
}

/** Code point count; falls back to byte count on invalid UTF-8 (pcre only, no ext-mbstring). */
function codepoint_length(string $value): int
{
    $count = @preg_match_all('/./su', $value);
    return $count === false ? \strlen($value) : $count;
}

/** First `$maximum` code points; falls back to a byte slice on invalid UTF-8. */
function codepoint_slice(string $value, int $maximum): string
{
    if (@preg_match('/^(.{0,' . $maximum . '})/su', $value, $matches) === 1) {
        return $matches[1];
    }
    return substr($value, 0, $maximum);
}

/** Trimmed, non-empty, at most `$maximum` code points; null otherwise. */
function bounded(mixed $value, int $maximum): ?string
{
    if (!\is_string($value)) {
        return null;
    }
    $trimmed = trim($value);
    if ($trimmed === '' || codepoint_length($trimmed) > $maximum) {
        return null;
    }
    return $trimmed;
}

/**
 * `$get(name)` returns the request header value (or null). Returns null when
 * no valid `x-reproit-trace` is present: the adapter stays inert.
 */
function trace_context_from_headers(callable $get): ?array
{
    $raw = $get('x-reproit-trace');
    $traceId = $raw === null ? null : bounded((string) $raw, 128);
    if ($traceId === null) {
        return null;
    }
    $header = function (string $name, int $maximum) use ($get): ?string {
        $value = $get($name);
        return $value === null ? null : bounded((string) $value, $maximum);
    };
    $action = $get('x-reproit-action');
    $parsed = $action === null ? false : filter_var(trim((string) $action), FILTER_VALIDATE_INT);
    $actionIndex = \is_int($parsed) && $parsed >= 0 && $parsed <= 0xffffffff ? $parsed : 0;
    return [
        'traceId' => $traceId,
        'actor' => $header('x-reproit-actor', 32),
        'actionIndex' => $actionIndex,
        'build' => $header('x-reproit-build', 128),
        'configContract' => $header('x-reproit-config-contract', 128),
    ];
}

function valid_path(mixed $path): bool
{
    if (!\is_string($path) || $path === '') {
        return false;
    }
    foreach (explode('.', $path) as $segment) {
        $name = str_ends_with($segment, '[]') ? substr($segment, 0, -2) : $segment;
        if (preg_match('/^[A-Za-z_][A-Za-z0-9_]*$/', $name) !== 1) {
            return false;
        }
    }
    return true;
}

/**
 * GraphQL selection mapping (parser-produced only). Returns null on an
 * invalid path, matching the Rust constructor.
 */
function selection(string $schemaPath, string $responsePath, ?string $typeCondition = null): ?array
{
    if (!valid_path($schemaPath) || !valid_path($responsePath)) {
        return null;
    }
    $value = ['schemaPath' => $schemaPath, 'responsePath' => $responsePath];
    if ($typeCondition !== null) {
        $invalid = !valid_path($typeCondition)
            || str_contains($typeCondition, '.')
            || str_contains($typeCondition, '[]');
        if ($invalid) {
            return null;
        }
        $value['typeCondition'] = $typeCondition;
    }
    return $value;
}

/**
 * Canonical decoded OpenAPI input. Framework adapters must provide decoded
 * values (including arrays for repeated query/header parameters), never raw
 * query strings whose serialization style is ambiguous. Keys of `$parts`:
 * body, path, query, headers. Returns stdClass so an empty input is `{}`.
 */
function http_input(array $parts): \stdClass
{
    $value = new \stdClass();
    if (\array_key_exists('body', $parts) && $parts['body'] !== null) {
        $value->body = $parts['body'];
    }
    foreach (['path', 'query', 'headers'] as $name) {
        $fields = $parts[$name] ?? null;
        if (!\is_array($fields)) {
            continue;
        }
        $entries = [];
        foreach ($fields as $key => $field) {
            $entries[$name === 'headers' ? strtolower((string) $key) : (string) $key] = $field;
        }
        if ($entries !== []) {
            $value->{$name} = $entries;
        }
    }
    return $value;
}

/**
 * Compact JSON with recursively sorted object keys: byte-identical to the
 * Rust adapter's serde_json (BTreeMap) encoding of the same events.
 */
function canonical_json(mixed $value): string
{
    if ($value instanceof \stdClass) {
        $value = get_object_vars($value);
        if ($value === []) {
            return '{}';
        }
    } elseif (!\is_array($value)) {
        $encoded = json_encode($value, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
        return $encoded === false ? 'null' : $encoded;
    } elseif (array_is_list($value)) {
        return '[' . implode(',', array_map(__FUNCTION__, $value)) . ']';
    }
    ksort($value, SORT_STRING);
    $body = [];
    foreach ($value as $key => $field) {
        $encodedKey = json_encode((string) $key, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
        $body[] = $encodedKey . ':' . canonical_json($field);
    }
    return '{' . implode(',', $body) . '}';
}

/** Hashed identity for idempotency keys: never ship the raw key. */
function identity(string $value): string
{
    return 'sha256:' . substr(hash('sha256', $value), 0, 24);
}

function secret_field(string $name): bool
{
    $folded = strtolower(preg_replace('/[^A-Za-z0-9]/', '', $name) ?? $name);
    foreach (SECRET_PARTS as $part) {
        if (str_contains($folded, $part)) {
            return true;
        }
    }
    return false;
}

/**
 * Recursive structural redaction: secret-named fields are replaced with a
 * `$reproit` metadata stub (type + length), everything else recurses.
 */
function redact(mixed $value): mixed
{
    if ($value instanceof \stdClass) {
        $redacted = new \stdClass();
        foreach (get_object_vars($value) as $key => $field) {
            $redacted->{$key} = secret_field((string) $key) ? metadata($field) : redact($field);
        }
        return $redacted;
    }
    if (\is_array($value)) {
        if (array_is_list($value)) {
            return array_map(__FUNCTION__, $value);
        }
        $redacted = [];
        foreach ($value as $key => $field) {
            $redacted[$key] = secret_field((string) $key) ? metadata($field) : redact($field);
        }
        return $redacted;
    }
    return $value;
}

function metadata(mixed $value): array
{
    $kind = 'null';
    $length = null;
    if (\is_bool($value)) {
        $kind = 'boolean';
    } elseif (\is_int($value)) {
        $kind = 'integer';
    } elseif (\is_float($value)) {
        $kind = 'number';
    } elseif (\is_string($value)) {
        $kind = 'string';
        $length = codepoint_length($value);
    } elseif (\is_array($value) && array_is_list($value)) {
        $kind = 'array';
        $length = \count($value);
    } elseif (\is_array($value) || $value instanceof \stdClass) {
        $kind = 'object';
    }
    return ['$reproit' => ['redacted' => true, 'type' => $kind, 'length' => $length]];
}

/** Process-wide event sequence, mirroring the reference adapters. */
function next_sequence(): int
{
    static $counter = 1;
    return $counter++;
}

final class BackendTrace
{
    private array $common;
    private array $events = [];
    private bool $finished = false;

    private function __construct(array $common)
    {
        $this->common = $common;
    }

    /** `$opts` keys: spanId, tenant, idempotencyKey, input, selections. */
    public static function begin(array $context, string $operation, array $opts = []): self
    {
        $name = bounded($operation, 256);
        if ($name === null) {
            throw new TraceError('InvalidOperation');
        }
        $spanId = bounded((string) ($opts['spanId'] ?? $context['traceId'] . ':' . $name), 128);
        if ($spanId === null) {
            throw new TraceError('InvalidOperation');
        }
        $common = [
            'traceId' => $context['traceId'],
            'spanId' => $spanId,
            'actionIndex' => $context['actionIndex'],
            'operation' => $name,
        ];
        foreach (['actor', 'build', 'configContract'] as $field) {
            if (!empty($context[$field])) {
                $common[$field] = $context[$field];
            }
        }
        $tenant = isset($opts['tenant']) ? bounded((string) $opts['tenant'], 128) : null;
        if ($tenant !== null) {
            $common['tenant'] = $tenant;
        }
        if (isset($opts['idempotencyKey'])) {
            $common['idempotencyKey'] = identity((string) $opts['idempotencyKey']);
        }
        if (\is_array($opts['selections'] ?? null) && $opts['selections'] !== []) {
            $common['selections'] = \array_slice(array_values($opts['selections']), 0, MAX_EVENTS);
        }
        $trace = new self($common);
        $trace->push('start', ['input' => redact($opts['input'] ?? null)]);
        return $trace;
    }

    /** `$opts` keys: resource, key, tenant, event, detail. */
    public function effect(string $kind, array $opts = []): void
    {
        if ($this->finished) {
            throw new TraceError('AlreadyFinished');
        }
        if (!\in_array($kind, EFFECT_KINDS, true)) {
            throw new TraceError('InvalidOperation');
        }
        $fields = ['effect' => $kind];
        $names = [
            'resource' => 'resource',
            'key' => 'key',
            'tenant' => 'effectTenant',
            'event' => 'event',
        ];
        foreach ($names as $option => $field) {
            if (isset($opts[$option])) {
                $fields[$field] = codepoint_slice((string) $opts[$option], 256);
            }
        }
        if (isset($opts['detail'])) {
            $detail = redact($opts['detail']);
            if ($detail instanceof \stdClass) {
                $detail = get_object_vars($detail);
            }
            if (\is_array($detail) && !array_is_list($detail)) {
                foreach (['before', 'after', 'payload'] as $key) {
                    if (\array_key_exists($key, $detail)) {
                        $fields[$key] = $detail[$key];
                    }
                }
            }
        }
        $this->push('effect', $fields);
    }

    public function finish(mixed $output, mixed $status, bool $success, bool $effectsComplete): void
    {
        if ($this->finished) {
            throw new TraceError('AlreadyFinished');
        }
        $this->push('return', [
            'output' => redact($output),
            'status' => $status,
            'success' => $success,
            'effectsComplete' => $effectsComplete,
        ]);
        $this->finished = true;
    }

    public function header(): string
    {
        if (!$this->finished) {
            throw new TraceError('AlreadyFinished');
        }
        $encoded = rtrim(strtr(base64_encode(canonical_json($this->events)), '+/', '-_'), '=');
        if (\strlen($encoded) > MAX_HEADER_BYTES) {
            throw new TraceError('HeaderTooLarge');
        }
        return $encoded;
    }

    public function events(): array
    {
        return $this->events;
    }

    public function finished(): bool
    {
        return $this->finished;
    }

    private function push(string $kind, array $fields): void
    {
        if (\count($this->events) >= MAX_EVENTS) {
            throw new TraceError('TooManyEvents');
        }
        $event = $this->common;
        $event['sequence'] = next_sequence();
        $event['kind'] = $kind;
        foreach ($fields as $name => $value) {
            $event[$name] = $value;
        }
        $this->events[] = $event;
    }
}
