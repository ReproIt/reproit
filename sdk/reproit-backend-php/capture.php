<?php

/*!
 * Production capture mode: config-gated self-sampling upload of finished
 * operation traces to the Reproit Cloud ingest endpoint
 * (`/v1/capture-batches`).
 *
 * PHP port of sdk/reproit-backend-rs/src/capture.rs. Scan-time tracing stays
 * untouched: this module only adds a place to hand a finished BackendTrace
 * when no `x-reproit-trace` header exists. Operations that end in a server
 * error (HTTP 5xx) or report `success == false` are always captured; healthy
 * operations only under an optional per-mille baseline sample (default 0).
 *
 * PHP-model flush (documented deviation from the reference worker): PHP has
 * no long-lived background thread or timer per request, so `record` only
 * queues and the queue drains in ONE bounded synchronous pass at request end,
 * inside a registered shutdown function, after the response is released to
 * the client where the SAPI allows it (fastcgi_finish_request). The drain is
 * hard-capped by `shutdownTimeoutMs`; whatever cannot ship inside that budget
 * is dropped and counted. All reference bounds are kept: a fixed-depth queue
 * drops oldest on overflow, batches and retries are capped, and `record`
 * never blocks or throws.
 */

declare(strict_types=1);

namespace ReproitBackend;

require_once __DIR__ . '/trace.php';

// Payload format identifier of the replayable capture object attached to the
// finding context (`context.reproitCapture`).
const CAPTURE_FORMAT = 'reproit-backend-capture';
const CAPTURE_VERSION = 1;
// Version stamped when any event carries a captured dependency `exchange` or
// an envelope stamp. Older readers reject it with a named version error
// instead of silently evaluating a payload whose replay semantics they do
// not understand.
const CAPTURE_VERSION_EXCHANGES = 2;
// First-class registry oracle id for an operation that returned HTTP 5xx.
const SERVER_ERROR_ORACLE = 'backend-server-error';

// Bounds. Queue overflow drops the OLDEST pending operation; an oversized
// capture payload drops trailing effect events before it drops itself.
const MAX_QUEUE_OPERATIONS = 64;
const MAX_BATCH_OPERATIONS = 16;
const MAX_CAPTURE_JSON_BYTES = 48 * 1024;
const MIN_FLUSH_INTERVAL_MS = 100;
const MAX_RETRY_LIMIT = 5;
// Hard cap on the end-of-request drain: the response is never delayed longer.
const MAX_SHUTDOWN_TIMEOUT_MS = 10000;

/** The ingest protocol token charset (`validate_token` in reproit-protocol). */
function valid_token(mixed $value): bool
{
    return \is_string($value) && preg_match('/^[A-Za-z0-9._:-]{1,128}$/', $value) === 1;
}

/**
 * Where and when the capture happened, and a seed that makes REPLAY runs
 * deterministic. Honesty note: the seed does not reproduce the randomness
 * the app drew in production; it pins the replay's.
 */
function determinism_envelope(?int $observedAtMs = null): array
{
    $envelope = [
        'observedAtMs' => $observedAtMs ?? (int) (microtime(true) * 1000),
        'tz' => date_default_timezone_get(),
        'runtime' => 'php ' . PHP_VERSION,
        'os' => PHP_OS_FAMILY,
        'arch' => php_uname('m'),
        'replaySeed' => bin2hex(random_bytes(8)),
    ];
    $digest = getenv('REPROIT_IMAGE_DIGEST') ?: null;
    if (valid_token($digest)) {
        $envelope['imageDigest'] = (string) $digest;
    }
    return $envelope;
}

/** Payload version for a set of events: 2 when any event carries a captured
 * dependency exchange or an envelope stamp, 1 otherwise. */
function payload_version(array $events): int
{
    foreach ($events as $event) {
        if (!\is_array($event)) {
            continue;
        }
        if (\is_array($event['exchange'] ?? null)
            || \array_key_exists('at', $event)
            || \array_key_exists('monoNs', $event)
        ) {
            return CAPTURE_VERSION_EXCHANGES;
        }
    }
    return CAPTURE_VERSION;
}

/**
 * The replayable capture object (`reproit debug replay-capture` input).
 * Trailing effect events are dropped first when the payload exceeds the
 * context budget; a payload that stays oversized with only start/return
 * left is omitted entirely (null value).
 */
function capture_payload(array $operation, ?array $envelope = null): array
{
    $events = array_values($operation['events']);
    $droppedEffects = 0;
    while (true) {
        $value = [
            'format' => CAPTURE_FORMAT,
            'version' => payload_version($events),
            'operation' => $operation['operation'],
            'oracle' => SERVER_ERROR_ORACLE,
            'events' => $events,
        ];
        if ($envelope !== null) {
            $value['envelope'] = $envelope;
        }
        if (\strlen(canonical_json($value)) <= MAX_CAPTURE_JSON_BYTES) {
            return [$value, $droppedEffects];
        }
        $lastEffect = -1;
        for ($index = \count($events) - 1; $index >= 0; $index--) {
            if (\is_array($events[$index]) && ($events[$index]['kind'] ?? null) === 'effect') {
                $lastEffect = $index;
                break;
            }
        }
        if ($lastEffect < 0) {
            return [null, $droppedEffects];
        }
        array_splice($events, $lastEffect, 1);
        $droppedEffects += 1;
    }
}

final class Capture
{
    private string $endpoint;
    private string $apiKey;
    private string $appId;
    private ?string $build;
    private int $healthySamplePerMille;
    private int $flushIntervalMs;
    private int $requestTimeoutMs;
    private int $retryLimit;
    private int $shutdownTimeoutMs;
    private ?string $commit = null;
    private array $queue = [];
    private int $traceSeq = 1;
    private int $batchSeq = 1;
    private array $stats = [
        'capturedOperations' => 0,
        'droppedOperations' => 0,
        'sentBatches' => 0,
        'failedBatches' => 0,
    ];

    /**
     * `$config` keys: endpoint, apiKey, appId, build, healthySamplePerMille,
     * flushIntervalMs, requestTimeoutMs, retryLimit, shutdownTimeoutMs.
     * Returns null (capture disabled, host unaffected) when the config is
     * unusable: empty endpoint/key or identifiers the ingest protocol rejects.
     */
    public static function create(array $config): ?self
    {
        $endpoint = $config['endpoint'] ?? null;
        $apiKey = $config['apiKey'] ?? null;
        if (!\is_string($endpoint) || trim($endpoint) === '') {
            return null;
        }
        if (!\is_string($apiKey) || trim($apiKey) === '') {
            return null;
        }
        if (!valid_token($config['appId'] ?? null)) {
            return null;
        }
        $build = $config['build'] ?? null;
        if ($build !== null && !valid_token($build)) {
            return null;
        }
        $commit = $config['commit'] ?? null;
        if ($commit !== null && !valid_token($commit)) {
            return null;
        }
        return new self($config, $endpoint, $apiKey, $build, self::resolveCommit($commit));
    }

    /**
     * Code identity for the capture, in priority order: explicit config,
     * then the common CI and platform environment. Never shells out to git.
     */
    public static function resolveCommit(?string $commit): ?string
    {
        foreach ([$commit, getenv('REPROIT_COMMIT') ?: null, getenv('GITHUB_SHA') ?: null] as $c) {
            if (valid_token($c)) {
                return (string) $c;
            }
        }
        return null;
    }

    private function __construct(
        array $config,
        string $endpoint,
        string $apiKey,
        ?string $build,
        ?string $commit = null
    ) {
        $this->endpoint = $endpoint;
        $this->apiKey = $apiKey;
        $this->appId = $config['appId'];
        $this->build = $build;
        $this->commit = $commit;
        $this->healthySamplePerMille = max(0, (int) ($config['healthySamplePerMille'] ?? 0));
        $this->flushIntervalMs =
            max(MIN_FLUSH_INTERVAL_MS, (int) ($config['flushIntervalMs'] ?? 3000));
        $this->requestTimeoutMs = max(1, (int) ($config['requestTimeoutMs'] ?? 5000));
        $this->retryLimit = min(MAX_RETRY_LIMIT, max(0, (int) ($config['retryLimit'] ?? 2)));
        $this->shutdownTimeoutMs = min(
            MAX_SHUTDOWN_TIMEOUT_MS,
            max(0, (int) ($config['shutdownTimeoutMs'] ?? 2000)),
        );
        // The PHP-model equivalent of the reference background worker: one
        // bounded synchronous drain when the request-handling process ends.
        register_shutdown_function([$this, 'shutdown']);
    }

    /**
     * Synthesized trace context for capture-mode operations, replacing the
     * scan-time `x-reproit-trace` header requirement.
     */
    public function context(): array
    {
        return [
            'traceId' => 'cap-' . (int) (microtime(true) * 1000) . '-' . $this->traceSeq++,
            'actor' => null,
            'actionIndex' => 0,
            'build' => $this->build,
            'configContract' => null,
            // Capture-mode traces stamp per-event wall-clock and monotonic
            // offsets (the determinism envelope); scan-time traces never do.
            'captureEnvelope' => true,
        ];
    }

    /**
     * Hand a finished trace to the sampler. Unfinished traces are ignored.
     * Queues only, never sends, never blocks, and never fails visibly;
     * overflow drops the oldest queued operation.
     */
    public function record(BackendTrace $trace): void
    {
        try {
            $events = $trace->events();
            $returned = null;
            for ($index = \count($events) - 1; $index >= 0; $index--) {
                if (\is_array($events[$index]) && ($events[$index]['kind'] ?? null) === 'return') {
                    $returned = $events[$index];
                    break;
                }
            }
            if ($returned === null) {
                return;
            }
            $success = \is_bool($returned['success'] ?? null) ? $returned['success'] : true;
            $status = $returned['status'] ?? null;
            if (!\is_int($status) || $status < 0 || $status > 0xffff) {
                $status = null;
            }
            $error = !$success || ($status !== null && $status >= 500);
            if (!$error && !$this->sampleHealthy()) {
                return;
            }
            $operation = $events[0]['operation'] ?? null;
            if (!\is_string($operation)) {
                return;
            }
            $this->stats['capturedOperations'] += 1;
            $this->queue[] = ['operation' => $operation, 'status' => $status, 'events' => $events];
            if (\count($this->queue) > MAX_QUEUE_OPERATIONS) {
                array_shift($this->queue);
                $this->stats['droppedOperations'] += 1;
            }
        } catch (\Throwable $ignored) {
            // Capture must never surface errors into the host app.
        }
    }

    /**
     * Synchronously drain the queue within `$timeoutMs`. Returns true when
     * every queued operation was sent (or dropped as a failed batch), false
     * when the budget ran out first (the remainder stays queued for the
     * shutdown drain). Intended for tests, examples, and long-running CLIs.
     */
    public function flush(int $timeoutMs): bool
    {
        try {
            $this->drain(microtime(true) + $timeoutMs / 1000.0);
        } catch (\Throwable $ignored) {
            // Fail closed: drop, never crash the host.
        }
        return $this->queue === [];
    }

    /**
     * End-of-request drain (registered in the constructor). Releases the
     * response first where the SAPI supports it, then drains inside the
     * `shutdownTimeoutMs` budget and drops whatever remains.
     */
    public function shutdown(): void
    {
        try {
            if ($this->queue === []) {
                return;
            }
            if (\function_exists('fastcgi_finish_request')) {
                @fastcgi_finish_request();
            } elseif (\function_exists('litespeed_finish_request')) {
                @litespeed_finish_request();
            }
            $this->flush($this->shutdownTimeoutMs);
            $this->stats['droppedOperations'] += \count($this->queue);
            $this->queue = [];
        } catch (\Throwable $ignored) {
            // Capture must never surface errors into the host app.
        }
    }

    public function stats(): array
    {
        return $this->stats;
    }

    private function sampleHealthy(): bool
    {
        $perMille = $this->healthySamplePerMille;
        if ($perMille <= 0) {
            return false;
        }
        if ($perMille >= 1000) {
            return true;
        }
        return random_int(0, 999) < $perMille;
    }

    private function drain(float $deadline): void
    {
        while ($this->queue !== [] && microtime(true) < $deadline) {
            $operations = array_splice($this->queue, 0, 1);
            if ($this->send($this->buildBatch($operations), $deadline)) {
                $this->stats['sentBatches'] += 1;
            } else {
                $this->stats['failedBatches'] += 1;
                $this->stats['droppedOperations'] += \count($operations);
            }
        }
    }

    /**
     * Build one source-neutral capture-batch-v1 payload.
     */
    public function buildBatch(array $operations): array
    {
        if (\count($operations) !== 1) {
            throw new \InvalidArgumentException(
                'a causal capture batch must contain exactly one operation'
            );
        }
        $operation = $operations[0];
        $batchId = 'cb-php-' . (int) (microtime(true) * 1000) . '-' . $this->batchSeq++;
        $first = $operation['events'][0] ?? [];
        $traceId = \is_string($first['traceId'] ?? null) ? $first['traceId'] : null;
        $events = [];
        $parent = null;
        // Real monotonic offsets from the trace's envelope stamps; the
        // ordinal fallback only applies to traces recorded without capture
        // mode.
        $add = function (array $event, ?array $source = null) use (
            &$events,
            &$parent,
            $traceId
        ): void {
            $sequence = \count($events) + 1;
            $eventId = 'evt_backend-php_' . $sequence;
            $mono = $source['monoNs'] ?? null;
            $item = [
                'id' => $eventId,
                'sequence' => $sequence,
                'monotonicNs' => \is_int($mono) ? $mono : $sequence,
                'causalParentIds' => $parent === null ? [] : [$parent],
                'event' => $event,
            ];
            if ($traceId !== null) {
                $item['traceId'] = $traceId;
            }
            $events[] = $item;
            $parent = $eventId;
        };
        $add(['kind' => 'operation-start', 'name' => $operation['operation']], $first);
        $input = $first['input'] ?? null;
        $capturedInput = $input === null
            ? ['representation' => 'structural', 'shape' => ['type' => 'unknown']]
            : [
                'representation' => 'replayable',
                'value' => $input,
                'redaction' => 'redacted-at-source',
            ];
        $add([
            'kind' => 'trigger',
            'trigger' => 'http-request',
            'subject' => $operation['operation'],
            'value' => $capturedInput,
        ], $first);
        // Determinism envelope: where and when the capture happened, and a
        // seed that makes REPLAY runs deterministic. Honesty note: the seed
        // does not reproduce the app's original randomness; it pins the
        // replay's.
        $add([
            'kind' => 'checkpoint',
            'name' => 'determinism-envelope',
            'attributes' => $this->envelopeAttributes($first),
        ], $first);
        foreach ($operation['events'] as $source) {
            if (($source['kind'] ?? null) !== 'effect') {
                continue;
            }
            $add([
                'kind' => 'effect',
                'effect' => $source['effect'] ?? 'backend-effect',
                'subject' => $source['resource'] ?? $source['service']
                    ?? $operation['operation'],
                'value' => [
                    'representation' => 'replayable',
                    'value' => $source,
                    'redaction' => 'redacted-at-source',
                ],
            ], $source);
        }
        $returned = [];
        foreach (array_reverse($operation['events']) as $source) {
            if (($source['kind'] ?? null) === 'return') {
                $returned = $source;
                break;
            }
        }
        // Nest the raw return event exactly like the raw effect events, so
        // the batch can be projected back to a replayable backend capture.
        // The subject names the carrier: `backend_capture_from_batch` in
        // reproit-protocol keys the inversion on "operation-return".
        if ($returned !== []) {
            $add([
                'kind' => 'effect',
                'effect' => 'operation-return',
                'subject' => 'operation-return',
                'value' => [
                    'representation' => 'replayable',
                    'value' => $returned,
                    'redaction' => 'redacted-at-source',
                ],
            ], $returned);
        }
        $add([
            'kind' => 'operation-end',
            'name' => $operation['operation'],
            'outcome' => ($returned['success'] ?? false) === true ? 'succeeded' : 'failed',
        ], $returned);
        $status = $operation['status'];
        if ($status !== null && $status >= 500) {
            $signature = SERVER_ERROR_ORACLE . ':' . $operation['operation'];
            $message = 'backend operation ' . $operation['operation']
                . ' returned HTTP ' . $status;
            $add([
                'kind' => 'observation',
                'failure' => [
                    'observation' => 'exception',
                    'authority' => 'runtime-diagnosis',
                    'summary' => $message,
                    'signature' => $signature,
                    'observationPoint' => $operation['operation'],
                    'artifactIds' => [],
                ],
            ]);
        }
        $batch = [
            'version' => 1,
            'batchId' => $batchId,
            'projectId' => $this->appId,
            'sessionId' => $traceId ?? $batchId,
            'emitter' => [
                'id' => 'backend-php',
                'kind' => 'runtime-sdk',
                'component' => 'backend',
                'runtime' => 'php',
            ],
            'observedAt' => (string) (int) (microtime(true) * 1000),
            'policy' => [
                'consent' => 'application-telemetry',
                'retentionClass' => 'standard',
            ],
            'capabilities' => $this->capabilities($operation),
            'events' => $events,
            'artifacts' => [],
        ];
        $deployment = [];
        if ($this->build !== null) {
            $deployment['version'] = $this->build;
        }
        if ($this->commit !== null) {
            $deployment['commit'] = $this->commit;
        }
        if ($deployment !== []) {
            $batch['deployment'] = $deployment;
        }
        return $batch;
    }

    /**
     * `network: complete` is declared ONLY when the instrument layer
     * actually recorded exchanges, so a capsule never claims a capability
     * it lacks.
     */
    private function capabilities(array $operation): array
    {
        $capabilities = [
            ['capability' => 'http', 'completeness' => 'complete'],
            [
                'capability' => 'database',
                'completeness' => 'partial',
                'detail' => 'effect records do not prove complete database state capture',
            ],
        ];
        foreach ($operation['events'] as $event) {
            if (\is_array($event) && \is_array($event['exchange'] ?? null)) {
                $capabilities[] = [
                    'capability' => 'network',
                    'completeness' => 'complete',
                    'detail' => 'outbound dependency exchanges recorded with responses',
                ];
                break;
            }
        }
        return $capabilities;
    }

    private function envelopeAttributes(array $first): array
    {
        return determinism_envelope(\is_int($first['at'] ?? null) ? $first['at'] : null);
    }

    private function send(array $batch, float $deadline): bool
    {
        $body = canonical_json($batch);
        for ($attempt = 0; $attempt <= $this->retryLimit; $attempt++) {
            $remaining = $deadline - microtime(true);
            if ($remaining <= 0) {
                return false;
            }
            $timeout = min($this->requestTimeoutMs / 1000.0, $remaining);
            $status = $this->post($body, $timeout);
            if ($status !== null && $status >= 200 && $status < 300) {
                return true;
            }
            // A definitive client-side rejection cannot improve on retry.
            if ($status !== null && $status >= 400 && $status < 500) {
                return false;
            }
            if ($attempt < $this->retryLimit) {
                $backoff = (200 * $attempt + 200) / 1000.0;
                if ($deadline - microtime(true) <= $backoff) {
                    return false;
                }
                usleep((int) ($backoff * 1000000));
            }
        }
        return false;
    }

    /** One POST attempt; curl when available, stream context otherwise. */
    private function post(string $body, float $timeoutSeconds): ?int
    {
        if (\extension_loaded('curl')) {
            $handle = curl_init($this->endpoint);
            if ($handle === false) {
                return null;
            }
            $timeoutMs = max(1, (int) ($timeoutSeconds * 1000));
            curl_setopt_array($handle, [
                CURLOPT_POST => true,
                CURLOPT_POSTFIELDS => $body,
                CURLOPT_HTTPHEADER => [
                    'Authorization: Bearer ' . $this->apiKey,
                    'Content-Type: application/json',
                ],
                CURLOPT_RETURNTRANSFER => true,
                CURLOPT_TIMEOUT_MS => $timeoutMs,
                CURLOPT_CONNECTTIMEOUT_MS => $timeoutMs,
            ]);
            $sent = curl_exec($handle);
            $status = $sent === false ? 0 : curl_getinfo($handle, CURLINFO_RESPONSE_CODE);
            unset($handle); // CurlHandle closes on release (curl_close is a no-op since 8.0)
            return \is_int($status) && $status > 0 ? $status : null;
        }
        $context = stream_context_create(['http' => [
            'method' => 'POST',
            'header' => 'Authorization: Bearer ' . $this->apiKey . "\r\n"
                . "Content-Type: application/json\r\n",
            'content' => $body,
            'timeout' => max(0.001, $timeoutSeconds),
            'ignore_errors' => true,
        ]]);
        $sent = @file_get_contents($this->endpoint, false, $context);
        $lines = \function_exists('http_get_last_response_headers')
            ? http_get_last_response_headers()
            : ($http_response_header ?? null);
        if ($sent === false || !isset($lines[0])) {
            return null;
        }
        $matched = preg_match('#^HTTP/\S+\s+(\d{3})#', $lines[0], $matches);
        return $matched === 1 ? (int) $matches[1] : null;
    }
}
