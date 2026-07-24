<?php

/*!
 * Plain callable wrapper for vanilla PHP apps (front controllers, `php -S`
 * routers, classic per-script endpoints), no framework required.
 *
 * `handle_request($capture, $handler)` reads the canonical input from the
 * superglobals (decoded JSON body up to 64 KB, `$_GET`, lowercased headers),
 * begins the trace, invokes `$handler($trace)`, and emits the JSON response.
 * The handler returns `[$status, $output]` and records observed effects on
 * the passed trace (null when tracing is inert). Scan-time requests get the
 * `x-reproit-events` response header; with a Capture the finished trace is
 * handed to the sampler and uploaded at request end (see capture.php). Every
 * adapter path fails closed: instrumentation errors never break the response.
 */

declare(strict_types=1);

namespace ReproitBackend;

require_once __DIR__ . '/reproit.php';

const VANILLA_MAX_BODY_BYTES = 64 * 1024;

/** Lowercased request headers from `$_SERVER` (HTTP_* plus CONTENT_*). */
function server_headers(array $server): array
{
    $headers = [];
    foreach ($server as $name => $value) {
        if (str_starts_with((string) $name, 'HTTP_')) {
            $headers[strtolower(strtr(substr((string) $name, 5), '_', '-'))] = $value;
        }
    }
    foreach (['CONTENT_TYPE' => 'content-type', 'CONTENT_LENGTH' => 'content-length'] as $k => $v) {
        if (isset($server[$k]) && $server[$k] !== '') {
            $headers[$v] = $server[$k];
        }
    }
    return $headers;
}

/**
 * `$options` keys: operation (callable(array $server): string), tenant
 * (callable(array $server): ?string), effectsComplete (bool).
 * `$handler(?BackendTrace $trace): array{0: int, 1: mixed}`.
 */
function handle_request(?Capture $capture, callable $handler, array $options = []): void
{
    $trace = null;
    $scan = false;
    try {
        $headers = server_headers($_SERVER);
        $get = fn (string $name): ?string => isset($headers[$name])
            ? (string) $headers[$name]
            : null;
        $scanContext = trace_context_from_headers($get);
        $context = $scanContext ?? $capture?->context();
        if ($context !== null) {
            $scan = $scanContext !== null;
            $operation = isset($options['operation'])
                ? (string) ($options['operation'])($_SERVER)
                : ($_SERVER['REQUEST_METHOD'] ?? 'GET') . ' '
                    . (parse_url($_SERVER['REQUEST_URI'] ?? '/', PHP_URL_PATH) ?: '/');
            $body = null;
            $raw = file_get_contents('php://input');
            $contentType = strtolower((string) ($headers['content-type'] ?? ''));
            if (
                \is_string($raw)
                && $raw !== ''
                && \strlen($raw) <= VANILLA_MAX_BODY_BYTES
                && str_contains($contentType, 'application/json')
            ) {
                $decoded = json_decode($raw);
                $body = json_last_error() === JSON_ERROR_NONE ? $decoded : null;
            }
            $trace = BackendTrace::begin($context, $operation, [
                'tenant' => isset($options['tenant']) ? ($options['tenant'])($_SERVER) : null,
                'input' => http_input([
                    'body' => $body,
                    'query' => $_GET,
                    'headers' => $headers,
                ]),
            ]);
        }
    } catch (\Throwable $ignored) {
        // Fail closed: an instrumentation defect must not break the request.
        $trace = null;
    }

    try {
        [$status, $output] = $handler($trace);
        $status = (int) $status;
    } catch (\Throwable $error) {
        $status = 500;
        $output = ['error' => 'internal server error'];
    }

    try {
        if ($trace !== null && !$trace->finished()) {
            $effectsComplete = ($options['effectsComplete'] ?? false) === true;
            $trace->finish($output, $status, $status < 500, $effectsComplete);
            if ($scan) {
                header('x-reproit-events: ' . $trace->header());
            } elseif ($capture !== null) {
                $capture->record($trace);
            }
        }
    } catch (\Throwable $ignored) {
        // Oversized or over-long traces drop their header; the response ships.
    }

    http_response_code($status);
    header('Content-Type: application/json');
    $encoded = json_encode($output, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
    echo $encoded === false ? '{}' : $encoded;
}
