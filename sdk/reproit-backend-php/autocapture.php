<?php

/*!
 * Automatic outbound-stream capture for reproit-backend-php.
 *
 * PHP has no process-wide HTTP chokepoint, so the explicit boundary
 * (`Instrument::http`, `RecordingClient`, `RecordingPdo`) is opt-in. ONE
 * outbound path is an exception: PHP lets userland replace a stream wrapper.
 * This file registers a wrapper over `http://` and `https://`, so every
 * stream-based outbound request (file_get_contents, fopen, SimpleXML, DOM
 * load on an http(s) URL) is captured AUTOMATICALLY with no per-call change,
 * through the SAME recording path as `Instrument::http` (the wrapper delegates
 * to it, so the recorded exchange shape is identical).
 *
 * Scope, stated precisely rather than papered over:
 *
 *  - AUTOMATIC once installed: http:// and https:// STREAM traffic
 *    (file_get_contents / fopen / SimpleXML / DOMDocument::load and anything
 *    else that opens the http(s) stream wrapper).
 *  - STILL OPT-IN: `curl_exec` and PDO. curl and the PDO drivers are C-level
 *    functions; PHP cannot redefine or intercept a C function at runtime
 *    without the uopz or runkit extension, and neither is present (nor an
 *    acceptable production dependency for an SDK). curl-direct traffic stays
 *    invisible unless routed through `Instrument::http`, and database
 *    statements stay invisible unless run through `RecordingPdo`.
 *
 * Installation is EXPLICIT: requiring reproit.php loads this class but does
 * not touch the wrapper table, so the existing suites are unaffected. The
 * `auto_prepend_file` bootstrap (bootstrap.php) or a direct `install()` call
 * installs it.
 *
 * Every capture path fails closed toward the host: an instrumentation defect
 * must never break the host application's request. In replay mode the wrapper
 * serves the recorded exchange with no socket and fails closed on divergence,
 * exactly like `Instrument::http`.
 */

declare(strict_types=1);

namespace ReproitBackend;

require_once __DIR__ . '/instrument.php';
require_once __DIR__ . '/replay.php';

/**
 * A capturing stream wrapper for http:// and https://. PHP instantiates one
 * per opened stream and sets `$context` from the caller's context, so the
 * request method, headers, and body are read from there and the whole
 * exchange is delegated to `Instrument::http`.
 */
final class StreamCapture
{
    /** Set by PHP from the caller's stream context; null when none is passed. */
    public $context;

    /** The schemes the wrapper owns. */
    private const SCHEMES = ['http', 'https'];

    private static bool $installed = false;

    private string $buffer = '';
    private int $offset = 0;

    /**
     * Register the wrapper over http:// and https://. Idempotent: a second
     * call is a no-op, so the auto_prepend bootstrap and an explicit test
     * install cannot double-register. Fails closed toward the host: a
     * registration error leaves the builtin wrapper in place rather than
     * breaking outbound HTTP.
     */
    public static function install(): void
    {
        if (self::$installed) {
            return;
        }
        try {
            self::register();
            self::$installed = true;
        } catch (\Throwable) {
            // Leave the builtin wrappers as they were; capture is best-effort
            // and must never take the host's outbound HTTP down with it.
            self::restore();
        }
    }

    /** Restore the builtin http/https wrappers. Idempotent. */
    public static function uninstall(): void
    {
        if (!self::$installed) {
            return;
        }
        self::restore();
        self::$installed = false;
    }

    public static function installed(): bool
    {
        return self::$installed;
    }

    private static function register(): void
    {
        foreach (self::SCHEMES as $scheme) {
            stream_wrapper_unregister($scheme);
            stream_wrapper_register($scheme, self::class);
        }
    }

    private static function restore(): void
    {
        foreach (self::SCHEMES as $scheme) {
            stream_wrapper_restore($scheme);
        }
    }

    /**
     * Open a stream. In capture mode the real request rides the builtin
     * wrapper (temporarily restored so the fetch does not recurse into this
     * class) through `Instrument::http`, which records the exchange. In
     * replay mode `Instrument::http` serves the recorded exchange with no
     * socket, and a divergence (server 599 sentinel) fails closed with a
     * thrown `DivergenceError`, the stream-wrapper form of the 599 the
     * explicit boundary returns.
     */
    public function stream_open(string $path, string $mode, int $options, ?string &$opened): bool
    {
        $this->buffer = '';
        $this->offset = 0;
        [$method, $headers, $body] = $this->request_parts();
        $session = Instrument::session();
        if ($session !== null) {
            $response = Instrument::http($method, $path, $body, $headers);
            if ($response->status === 599) {
                throw new DivergenceError('reproit: outbound stream diverged from the capture');
            }
            $this->buffer = $response->body;
            return true;
        }
        // Capture: the fetch must reach a real socket, so restore the builtin
        // wrapper around it, then re-register this class. A capture defect is
        // isolated: the host still gets its bytes through a plain builtin
        // fetch and the miss is counted, never raised into the host.
        self::restore();
        try {
            $response = Instrument::http($method, $path, $body, $headers);
            $this->buffer = $response->body;
        } catch (\Throwable) {
            Instrument::count('failedCaptures');
            $context = \is_resource($this->context) ? $this->context : null;
            $raw = @file_get_contents($path, false, $context);
            $this->buffer = $raw === false ? '' : $raw;
        } finally {
            self::register();
        }
        return true;
    }

    public function stream_read(int $count): string
    {
        $chunk = substr($this->buffer, $this->offset, max($count, 0));
        $this->offset += \strlen($chunk);
        return $chunk;
    }

    public function stream_eof(): bool
    {
        return $this->offset >= \strlen($this->buffer);
    }

    public function stream_tell(): int
    {
        return $this->offset;
    }

    /** The size is what file_get_contents preallocates against; the rest is zeroed. */
    public function stream_stat(): array
    {
        return $this->stat_shape(\strlen($this->buffer));
    }

    /** Swallow blocking/timeout options so a stdlib fetch raises no warning. */
    public function stream_set_option(int $option, int $arg1, int $arg2): bool
    {
        return true;
    }

    public function stream_close(): void
    {
        $this->buffer = '';
        $this->offset = 0;
    }

    /**
     * No stat is served for a URL: an http(s) resource has no filesystem
     * identity, and reporting one would let file_exists lie. false is the
     * honest answer and matches the builtin wrapper.
     */
    public function url_stat(string $path, int $flags): array|false
    {
        return false;
    }

    /**
     * Read the request method, headers, and body from the caller's stream
     * context. Absent context means a plain GET with no headers or body, the
     * file_get_contents default.
     *
     * @return array{0: string, 1: array<string, string>, 2: ?string}
     */
    private function request_parts(): array
    {
        $method = 'GET';
        $headers = [];
        $body = null;
        if (\is_resource($this->context)) {
            $options = stream_context_get_options($this->context);
            $http = $options['http'] ?? $options['https'] ?? [];
            if (isset($http['method']) && $http['method'] !== '') {
                $method = strtoupper((string) $http['method']);
            }
            $headers = self::parse_headers($http['header'] ?? []);
            if (isset($http['content']) && $http['content'] !== '') {
                $body = (string) $http['content'];
            }
        }
        return [$method, $headers, $body];
    }

    /**
     * Normalize context headers (a CRLF string or a list of "Name: value"
     * lines) into the name => value map `Instrument::http` expects.
     *
     * @param string|array<int, string> $header
     * @return array<string, string>
     */
    private static function parse_headers(string|array $header): array
    {
        $lines = \is_array($header) ? $header : (preg_split('/\r\n|\n|\r/', $header) ?: []);
        $headers = [];
        foreach ($lines as $line) {
            $split = strpos((string) $line, ':');
            if ($split === false) {
                continue;
            }
            $name = trim(substr((string) $line, 0, $split));
            if ($name === '') {
                continue;
            }
            $headers[$name] = trim(substr((string) $line, $split + 1));
        }
        return $headers;
    }

    /** A full stat array with only the size set; PHP reads `size` (index 7). */
    private function stat_shape(int $size): array
    {
        $keyed = [
            'dev' => 0, 'ino' => 0, 'mode' => 0, 'nlink' => 0, 'uid' => 0,
            'gid' => 0, 'rdev' => 0, 'size' => $size, 'atime' => 0, 'mtime' => 0,
            'ctime' => 0, 'blksize' => -1, 'blocks' => -1,
        ];
        return array_merge(array_values($keyed), $keyed);
    }
}

/**
 * Install automatic http(s) stream capture. Idempotent and fail-closed. Call
 * it from the auto_prepend bootstrap (bootstrap.php) or directly.
 */
function install(): void
{
    StreamCapture::install();
}

/** Restore the builtin http(s) wrappers. Idempotent. */
function uninstall(): void
{
    StreamCapture::uninstall();
}
