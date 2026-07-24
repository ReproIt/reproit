<?php

/*!
 * Production capture mode: config-gated self-sampling upload of finished
 * operation traces to the Reproit Cloud ingest endpoint (`/v1/events`).
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
 * The replayable capture object (`reproit debug replay-capture` input).
 * Trailing effect events are dropped first when the payload exceeds the
 * context budget; a payload that stays oversized with only start/return
 * left is omitted entirely (null value).
 */
function capture_payload(array $operation): array
{
    $events = array_values($operation['events']);
    $droppedEffects = 0;
    while (true) {
        $value = [
            'format' => CAPTURE_FORMAT,
            'version' => CAPTURE_VERSION,
            'operation' => $operation['operation'],
            'oracle' => SERVER_ERROR_ORACLE,
            'events' => $events,
        ];
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
        return new self($config, $endpoint, $apiKey, $build);
    }

    private function __construct(array $config, string $endpoint, string $apiKey, ?string $build)
    {
        $this->endpoint = $endpoint;
        $this->apiKey = $apiKey;
        $this->appId = $config['appId'];
        $this->build = $build;
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
            $operations = array_splice($this->queue, 0, MAX_BATCH_OPERATIONS);
            if ($this->send($this->buildBatch($operations), $deadline)) {
                $this->stats['sentBatches'] += 1;
            } else {
                $this->stats['failedBatches'] += 1;
                $this->stats['droppedOperations'] += \count($operations);
            }
        }
    }

    /**
     * Build one event-batch-v1 payload: every captured event ships as a
     * `backend` frame, and each 5xx operation additionally ships a `finding`
     * frame tagged `backend-server-error` whose context carries the full
     * replayable capture object. Public for tests; not a host-facing API.
     */
    public function buildBatch(array $operations): array
    {
        $batchId = 'cap-' . (int) (microtime(true) * 1000) . '-' . $this->batchSeq++;
        $frames = [];
        $frame = function (array $event) use (&$frames, $batchId): void {
            $frames[] = [
                'runId' => $batchId,
                'sequence' => \count($frames) + 1,
                'scope' => ['domain' => 'shared'],
                'event' => $event,
            ];
        };
        foreach ($operations as $operation) {
            foreach ($operation['events'] as $event) {
                $frame(['kind' => 'backend', 'evidence' => $event]);
            }
            $status = $operation['status'];
            if ($status === null || $status < 500) {
                continue;
            }
            $signature = 'backend:' . $operation['operation'];
            $message = 'backend operation ' . $operation['operation']
                . ' returned HTTP ' . $status;
            $context = ['capture' => 'reproit-backend-php'];
            if ($this->build !== null) {
                $context['build'] = ['version' => $this->build];
            }
            [$payload, $droppedEffects] = capture_payload($operation);
            if ($payload === null) {
                $context['captureOmitted'] = true;
            } else {
                $context['reproitCapture'] = $payload;
                if ($droppedEffects > 0) {
                    $context['captureDroppedEffects'] = $droppedEffects;
                }
            }
            $frame([
                'kind' => 'finding',
                'signature' => $signature,
                'message' => $message,
                'identity' => [
                    'oracle' => SERVER_ERROR_ORACLE,
                    'invariant' => 'backend:server-error',
                    'kind' => 'server-error',
                    'message' => $message,
                    'frame' => '',
                    'trigger' => $signature,
                    'boundary' => $signature,
                ],
                'path' => [],
                'context' => $context,
            ]);
        }
        $batch = [
            'version' => 1,
            'batchId' => $batchId,
            'appId' => $this->appId,
            'frames' => $frames,
            'evidence' => [],
        ];
        if ($this->build !== null) {
            $batch['deployment'] = ['version' => $this->build];
        }
        return $batch;
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
