<?php

/*!
 * PSR-18 client decoration for reproit-backend-php: the capsule boundary for
 * apps whose outbound HTTP goes through a PSR-18 ClientInterface (Guzzle 7+,
 * Symfony HttpClient's Psr18Client, php-http adapters).
 *
 * `RecordingClient` wraps any PSR-18 client. Capture: the request line,
 * headers, and body plus the response status, headers, and body are recorded
 * onto the ambient trace as an `http` exchange, bounded and redacted at
 * source. The response body is TEED, not drained: PSR-7 streams are
 * pull-based, so a TeeStream records chunks AS THE APP CONSUMES them and the
 * exchange lands at EOF with the observed chunk boundaries (the SSE / LLM
 * streaming shape). A body the app abandons records nothing, exactly like
 * the Node reference's fetch wrapper. Replay (`REPROIT_REPLAY`): the
 * decorator serves the recorded exchange in process; the inner client is
 * never called, so no socket opens.
 *
 * NAMED GAP, stated rather than papered over: PHP has no process-wide HTTP
 * chokepoint, so traffic that bypasses the decorated client (curl_exec
 * called directly, file_get_contents with an http:// URL, an ORM's own
 * transport) is invisible to capture and unavailable at replay. Route
 * outbound calls through this client (or Instrument::http) or they are
 * outside the capsule.
 *
 * The PSR interfaces are vendored as minimal guarded declarations, the
 * psr15.php pattern: zero runtime dependencies, real psr/* installs win.
 * Method signatures follow the typed psr/http-message v2 surface.
 */

declare(strict_types=1);

namespace Psr\Http\Message {
    if (!\interface_exists(StreamInterface::class)) {
        interface StreamInterface
        {
        }
    }
    if (!\interface_exists(RequestInterface::class)) {
        interface RequestInterface
        {
        }
    }
    if (!\interface_exists(ResponseInterface::class)) {
        interface ResponseInterface
        {
        }
    }
}

namespace Psr\Http\Client {

    use Psr\Http\Message\RequestInterface;
    use Psr\Http\Message\ResponseInterface;

    if (!\interface_exists(ClientInterface::class)) {
        interface ClientInterface
        {
            public function sendRequest(RequestInterface $request): ResponseInterface;
        }
    }
}

namespace ReproitBackend\Psr18 {

    use Psr\Http\Client\ClientInterface;
    use Psr\Http\Message\RequestInterface;
    use Psr\Http\Message\ResponseInterface;
    use Psr\Http\Message\StreamInterface;
    use ReproitBackend\BodyCollector;
    use ReproitBackend\Instrument;
    use ReproitBackend\ReplaySession;

    use function ReproitBackend\http_exchange;
    use function ReproitBackend\serve_http;
    use function ReproitBackend\try_json;

    require_once __DIR__ . '/reproit.php';

    /**
     * A request body larger than this is not buffered for the record: the
     * exchange keeps the request without content rather than holding an
     * unbounded upload in memory (the Python port's MAX_TEE decision).
     */
    const MAX_REQUEST_RECORD_BYTES = 8 * 1024 * 1024;

    final class RecordingClient implements ClientInterface
    {
        public function __construct(private readonly ClientInterface $inner)
        {
        }

        public function sendRequest(RequestInterface $request): ResponseInterface
        {
            $session = Instrument::session();
            if ($session !== null) {
                return $this->serve($session, $request);
            }
            $meta = null;
            try {
                $meta = $this->requestMeta($request);
            } catch (\Throwable) {
                Instrument::count('failedCaptures');
            }
            $response = $this->inner->sendRequest($request);
            if ($meta === null || Instrument::currentTrace() === null) {
                return $response;
            }
            try {
                return $this->tee($meta, $response);
            } catch (\Throwable) {
                Instrument::count('failedCaptures');
                return $response;
            }
        }

        /** @return array{method: string, url: string, host: string,
         *               headers: array, body: ?string, contentType: string} */
        private function requestMeta(RequestInterface $request): array
        {
            $uri = $request->getUri();
            $headers = [];
            foreach ($request->getHeaders() as $name => $values) {
                $headers[(string) $name] = implode(', ', array_map('strval', $values));
            }
            $body = null;
            $stream = $request->getBody();
            if ($stream->isSeekable()) {
                $size = $stream->getSize();
                if ($size === null || $size <= MAX_REQUEST_RECORD_BYTES) {
                    $stream->rewind();
                    $body = $stream->getContents();
                    $stream->rewind();
                }
            }
            return [
                'method' => strtoupper($request->getMethod()),
                'url' => (string) $uri,
                'host' => $uri->getHost(),
                'headers' => $headers,
                'body' => $body,
                'contentType' => $request->getHeaderLine('content-type'),
            ];
        }

        /**
         * Wrap the response body in a TeeStream: the exchange records at the
         * moment the app observes EOF, with the chunk boundaries the app saw.
         */
        private function tee(array $meta, ResponseInterface $response): ResponseInterface
        {
            $contentType = $response->getHeaderLine('content-type');
            $headers = [];
            foreach ($response->getHeaders() as $name => $values) {
                $headers[(string) $name] = implode(', ', array_map('strval', $values));
            }
            $status = $response->getStatusCode();
            $record = function (BodyCollector $collector) use (
                $meta,
                $status,
                $headers,
                $contentType
            ): void {
                Instrument::record(
                    'call',
                    $meta['host'] !== '' ? $meta['host'] : 'http',
                    $meta['method'] . ' ' . $meta['url'],
                    http_exchange(
                        [
                            'method' => $meta['method'],
                            'url' => $meta['url'],
                            'headers' => $meta['headers'],
                            'body' => $meta['body'],
                            'contentType' => $meta['contentType'],
                        ],
                        [
                            'status' => $status,
                            'headers' => $headers,
                            'body' => $collector->result(),
                            'contentType' => $contentType,
                            'stream' => $collector->stream(
                                str_contains($contentType, 'text/event-stream')
                            ),
                        ]
                    )
                );
            };
            // TeeStream fires $record exactly once, at EOF. An abandoned
            // body never reaches EOF and records nothing.
            return $response->withBody(new TeeStream($response->getBody(), $record));
        }

        private function serve(
            ReplaySession $session,
            RequestInterface $request,
        ): ResponseInterface {
            $probe = [
                'method' => strtoupper($request->getMethod()),
                'url' => (string) $request->getUri(),
            ];
            $stream = $request->getBody();
            $body = $stream->isSeekable() ? (string) $stream : $stream->getContents();
            if ($body !== '') {
                $probe['body'] = try_json($body, $request->getHeaderLine('content-type'));
            }
            $served = serve_http($session, $probe);
            return new ReplayResponse(
                $served['status'],
                $served['headers'],
                new ReplayStream($served['chunks'] ?? [$served['body']])
            );
        }
    }

    /**
     * Pull-based tee: chunks flow to the app exactly as the inner stream
     * yields them and into a BodyCollector on the side. `$onEof` fires once,
     * when the app observes end of stream (read to EOF or getContents), so
     * the recorded chunk boundaries are the ones the app actually saw.
     */
    final class TeeStream implements StreamInterface
    {
        private BodyCollector $collector;
        private bool $recorded = false;
        /** @var callable(BodyCollector): void */
        private $onEof;

        public function __construct(private readonly StreamInterface $inner, callable $onEof)
        {
            $this->collector = new BodyCollector();
            $this->onEof = $onEof;
        }

        public function read(int $length): string
        {
            $chunk = $this->inner->read($length);
            try {
                if ($chunk !== '') {
                    $this->collector->push($chunk);
                }
                if ($this->inner->eof()) {
                    $this->recordOnce();
                }
            } catch (\Throwable) {
                Instrument::count('failedCaptures');
            }
            return $chunk;
        }

        public function getContents(): string
        {
            $contents = '';
            while (!$this->inner->eof()) {
                $chunk = $this->read(1 << 16);
                if ($chunk === '') {
                    break;
                }
                $contents .= $chunk;
            }
            return $contents;
        }

        public function __toString(): string
        {
            try {
                if ($this->inner->isSeekable()) {
                    $this->inner->rewind();
                }
                return $this->getContents();
            } catch (\Throwable) {
                return '';
            }
        }

        private function recordOnce(): void
        {
            if ($this->recorded) {
                return;
            }
            $this->recorded = true;
            ($this->onEof)($this->collector);
        }

        public function close(): void
        {
            $this->inner->close();
        }

        public function detach()
        {
            return $this->inner->detach();
        }

        public function getSize(): ?int
        {
            return $this->inner->getSize();
        }

        public function tell(): int
        {
            return $this->inner->tell();
        }

        public function eof(): bool
        {
            return $this->inner->eof();
        }

        public function isSeekable(): bool
        {
            return false;
        }

        public function seek(int $offset, int $whence = SEEK_SET): void
        {
            throw new \RuntimeException('reproit tee stream is not seekable');
        }

        public function rewind(): void
        {
            throw new \RuntimeException('reproit tee stream is not seekable');
        }

        public function isWritable(): bool
        {
            return false;
        }

        public function write(string $string): int
        {
            throw new \RuntimeException('reproit tee stream is read only');
        }

        public function isReadable(): bool
        {
            return true;
        }

        public function getMetadata(?string $key = null)
        {
            return $this->inner->getMetadata($key);
        }
    }

    /**
     * Replay-mode body: the recorded bytes, served one recorded chunk per
     * read so a consumer of an SSE exchange observes the captured stream
     * shape. Minimal on purpose; anything unsupported fails loudly.
     */
    final class ReplayStream implements StreamInterface
    {
        /** @var list<string> */
        private array $chunks;
        private int $at = 0;
        private string $pending = '';

        /** @param list<string> $chunks */
        public function __construct(array $chunks)
        {
            $this->chunks = array_values($chunks);
        }

        public function read(int $length): string
        {
            if ($this->pending === '' && $this->at < \count($this->chunks)) {
                $this->pending = $this->chunks[$this->at];
                $this->at += 1;
            }
            $out = substr($this->pending, 0, max(0, $length));
            $this->pending = substr($this->pending, \strlen($out));
            return $out;
        }

        public function getContents(): string
        {
            $rest = $this->pending . implode('', \array_slice($this->chunks, $this->at));
            $this->pending = '';
            $this->at = \count($this->chunks);
            return $rest;
        }

        public function __toString(): string
        {
            return implode('', $this->chunks);
        }

        public function eof(): bool
        {
            return $this->pending === '' && $this->at >= \count($this->chunks);
        }

        public function close(): void
        {
        }

        public function detach()
        {
            return null;
        }

        public function getSize(): ?int
        {
            return \strlen($this->__toString());
        }

        public function tell(): int
        {
            return \strlen($this->__toString()) - \strlen($this->pending)
                - \strlen(implode('', \array_slice($this->chunks, $this->at)));
        }

        public function isSeekable(): bool
        {
            return false;
        }

        public function seek(int $offset, int $whence = SEEK_SET): void
        {
            throw new \RuntimeException('reproit replay stream is not seekable');
        }

        public function rewind(): void
        {
            $this->at = 0;
            $this->pending = '';
        }

        public function isWritable(): bool
        {
            return false;
        }

        public function write(string $string): int
        {
            throw new \RuntimeException('reproit replay stream is read only');
        }

        public function isReadable(): bool
        {
            return true;
        }

        public function getMetadata(?string $key = null)
        {
            return $key === null ? [] : null;
        }
    }

    /**
     * Replay-mode PSR-7 response: status, lowercased headers, ReplayStream
     * body. Only the surface applications read from a response; `with*`
     * mutators are deliberately absent (a replayed response is evidence, not
     * a builder).
     */
    final class ReplayResponse implements ResponseInterface
    {
        /** @param array<string, string> $headers */
        public function __construct(
            private readonly int $status,
            private readonly array $headers,
            private readonly ReplayStream $body,
        ) {
        }

        public function getStatusCode(): int
        {
            return $this->status;
        }

        public function getReasonPhrase(): string
        {
            return $this->status === 599 ? 'Reproit Diverged' : 'OK';
        }

        public function getProtocolVersion(): string
        {
            return '1.1';
        }

        /** @return array<string, list<string>> */
        public function getHeaders(): array
        {
            return array_map(fn (string $value): array => [$value], $this->headers);
        }

        public function hasHeader(string $name): bool
        {
            return isset($this->headers[strtolower($name)]);
        }

        /** @return list<string> */
        public function getHeader(string $name): array
        {
            $value = $this->headers[strtolower($name)] ?? null;
            return $value === null ? [] : [$value];
        }

        public function getHeaderLine(string $name): string
        {
            return $this->headers[strtolower($name)] ?? '';
        }

        public function getBody(): ReplayStream
        {
            return $this->body;
        }
    }
}
