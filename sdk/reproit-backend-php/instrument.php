<?php

/*!
 * Outbound-exchange capture and hermetic replay for reproit-backend-php.
 *
 * PHP cannot monkeypatch a client the way Node can, so the boundary is
 * explicit and OPT-IN, exactly like the Rust SDK: route outbound HTTP
 * through `Instrument::http()` (or wrap a PSR-18 client with
 * `RecordingClient`) and database statements through `Instrument::db()`, and
 * every dependency exchange (request AND response) is recorded onto the
 * ambient request trace, bounded and redacted at source.
 *
 * What IS automatic once the boundary is used: the ambient trace comes from
 * the request-scoped singleton the framework adapters set, so handlers do
 * not thread a trace through their own code, and with `REPROIT_REPLAY` set
 * the SAME entry points serve the recorded exchanges with no socket and no
 * driver. Anything bypassing the boundary (a raw curl_exec, an ORM with its
 * own connection) is invisible to capture and unavailable at replay; that is
 * stated rather than papered over.
 *
 * Every capture path fails closed the other way: an instrumentation defect
 * must never break the host application's request.
 */

declare(strict_types=1);

namespace ReproitBackend;

require_once __DIR__ . '/exchange.php';
require_once __DIR__ . '/replay.php';
require_once __DIR__ . '/trace.php';

/** Raised when a replayed call has no matching recorded exchange. */
final class DivergenceError extends \RuntimeException
{
}

/** Raised in replay mode when the capture recorded a driver error. */
final class RecordedError extends \RuntimeException
{
}

/** The served side of one HTTP exchange, whether live or replayed. */
final class HttpResponse
{
    public function __construct(
        public readonly int $status,
        /** @var array<string, string> */
        public readonly array $headers,
        public readonly string $body,
    ) {
    }

    public function json(): mixed
    {
        return json_decode($this->body, true);
    }
}

final class Instrument
{
    private static ?BackendTrace $trace = null;
    private static ?ReplaySession $session = null;
    private static bool $sessionLoaded = false;
    private static array $stats = [
        'capturedExchanges' => 0,
        'truncatedBodies' => 0,
        'failedCaptures' => 0,
    ];

    /**
     * The request-scoped ambient trace. PHP serves one request per process
     * in the classic model, so a static singleton IS the request scope; the
     * framework adapters set and clear it around the handler.
     */
    public static function setTrace(?BackendTrace $trace): void
    {
        self::$trace = $trace;
    }

    public static function currentTrace(): ?BackendTrace
    {
        if (self::$trace === null || self::$trace->finished()) {
            return null;
        }
        return self::$trace;
    }

    /**
     * Load the replay session once and pin the process envelope. Calling
     * this from the entry point pins the timezone before any date-sensitive
     * code runs.
     */
    public static function session(): ?ReplaySession
    {
        if (!self::$sessionLoaded) {
            self::$sessionLoaded = true;
            $path = (string) (getenv('REPROIT_REPLAY') ?: '');
            if (trim($path) !== '') {
                self::$session = ReplaySession::load($path);
                pin_envelope(self::$session->envelope());
            }
        }
        return self::$session;
    }

    public static function replaying(): bool
    {
        return self::session() !== null;
    }

    /** The capture's seeded stream, or null outside replay mode. */
    public static function replayRng(): ?ReplayRng
    {
        $session = self::session();
        return $session === null ? null : rng_for($session->envelope());
    }

    /**
     * The capture's wall-clock instant in epoch milliseconds, or null outside
     * replay mode. PHP cannot pin the process clock without an extension (see
     * pin_envelope in replay.php for the measured evidence), so an app that
     * needs an anchored clock in replay reads this and uses it as its time
     * source instead of calling time() directly.
     */
    public static function replayObservedAtMs(): ?int
    {
        $session = self::session();
        return $session === null ? null : observed_at_ms($session->envelope());
    }

    /**
     * The SDK clock seam: a PinnedClock offset to the capture instant in
     * replay mode, the system clock otherwise. PHP cannot intercept time()
     * process wide without an extension (pin_envelope documents the measured
     * evidence), so an app that wants replay-anchored time reads THIS clock
     * instead of the ambient one; no mode branch needed in app code.
     */
    public static function clock(): Clock
    {
        $observed = self::replayObservedAtMs();
        return $observed === null ? new SystemClock() : new PinnedClock($observed);
    }

    /** @return array<string, int> */
    public static function stats(): array
    {
        return self::$stats;
    }

    public static function count(string $key): void
    {
        self::$stats[$key] = (self::$stats[$key] ?? 0) + 1;
    }

    /** Record one exchange on the ambient trace; never raises into the host. */
    public static function record(string $kind, string $resource, string $key, array $exchange): void
    {
        try {
            $trace = self::currentTrace();
            if ($trace === null) {
                return;
            }
            $trace->effect($kind, [
                'resource' => $resource,
                'key' => $key,
                'exchange' => $exchange,
            ]);
            self::count('capturedExchanges');
        } catch (\Throwable) {
            self::count('failedCaptures');
        }
    }

    /**
     * Outbound HTTP boundary. Sends the request with stdlib streams and
     * records the exchange, or serves the recorded one in replay mode
     * without opening a socket.
     *
     * @param array<string, string> $headers
     */
    public static function http(
        string $method,
        string $url,
        ?string $body = null,
        array $headers = [],
        float $timeoutSeconds = 5.0
    ): HttpResponse {
        $method = strtoupper($method);
        $contentType = '';
        foreach ($headers as $name => $value) {
            if (strtolower((string) $name) === 'content-type') {
                $contentType = (string) $value;
            }
        }
        $session = self::session();
        if ($session !== null) {
            $probe = ['method' => $method, 'url' => $url];
            if ($body !== null && $body !== '') {
                $probe['body'] = try_json($body, $contentType);
            }
            $served = serve_http($session, $probe);
            return new HttpResponse($served['status'], $served['headers'], $served['body']);
        }
        $headerLines = '';
        foreach ($headers as $name => $value) {
            $headerLines .= $name . ': ' . $value . "\r\n";
        }
        $context = stream_context_create(['http' => [
            'method' => $method,
            'header' => $headerLines,
            'content' => $body ?? '',
            'timeout' => $timeoutSeconds,
            'ignore_errors' => true,
        ]]);
        $raw = @file_get_contents($url, false, $context);
        $responseHeaders = [];
        $status = 0;
        foreach ($http_response_header ?? [] as $line) {
            if (preg_match('#^HTTP/\S+\s+(\d{3})#', $line, $matches) === 1) {
                $status = (int) $matches[1];
                continue;
            }
            $split = explode(':', $line, 2);
            if (\count($split) === 2) {
                $responseHeaders[strtolower(trim($split[0]))] = trim($split[1]);
            }
        }
        $responseBody = $raw === false ? '' : $raw;
        try {
            self::record('call', parse_url($url, PHP_URL_HOST) ?: 'http', $method . ' ' . $url, http_exchange(
                [
                    'method' => $method,
                    'url' => $url,
                    'headers' => $headers,
                    'body' => $body,
                    'contentType' => $contentType,
                ],
                [
                    'status' => $status,
                    'headers' => $responseHeaders,
                    'body' => $responseBody,
                    'contentType' => $responseHeaders['content-type'] ?? '',
                ]
            ));
        } catch (\Throwable) {
            self::count('failedCaptures');
        }
        return new HttpResponse($status, $responseHeaders, $responseBody);
    }

    /**
     * Generic database boundary: run `$run` and record the statement with
     * its result, or serve the recorded result in replay mode without
     * touching a driver. `$run` is never called while replaying, which is
     * what makes a replay run valid with the database stopped.
     *
     * `$run` returns ['rows' => [...], 'command' => .., 'rowCount' => ..].
     */
    public static function db(string $text, ?array $values, callable $run): array
    {
        $session = self::session();
        if ($session !== null) {
            $probe = ['text' => $text];
            if ($values !== null && $values !== []) {
                $probe['values'] = array_values($values);
            }
            $recorded = $session->match('db', $probe);
            if ($recorded === null) {
                throw new DivergenceError('reproit: database call diverged from the capture');
            }
            $outcome = $recorded['response'] ?? [];
            if (isset($outcome['error'])) {
                throw new RecordedError((string) ($outcome['error']['message'] ?? 'recorded error'));
            }
            return [
                'command' => $outcome['command'] ?? null,
                'rowCount' => $outcome['rowCount'] ?? 0,
                'rows' => \is_array($outcome['rows'] ?? null) ? $outcome['rows'] : [],
            ];
        }
        try {
            $result = $run();
        } catch (\Throwable $error) {
            self::record(
                statement_effect_kind($text),
                'db',
                substr($text, 0, 256),
                db_exchange($text, $values, db_error($error))
            );
            throw $error;
        }
        self::record(
            statement_effect_kind($text),
            'db',
            substr($text, 0, 256),
            db_exchange($text, $values, db_outcome($result))
        );
        return \is_array($result) ? $result : ['rows' => [], 'rowCount' => 0];
    }

    /** Test seam: forget the loaded session so a new REPROIT_REPLAY applies. */
    public static function resetForTests(): void
    {
        self::$session = null;
        self::$sessionLoaded = false;
        self::$trace = null;
    }
}
