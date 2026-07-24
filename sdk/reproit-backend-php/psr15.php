<?php

/*!
 * PSR-15 middleware for reproit-backend-php.
 *
 * Scan-time: inert unless the request carries `x-reproit-trace`; the finished
 * trace is returned as the `x-reproit-events` response header. Production:
 * pass a Capture and every request is traced and handed to the sampler
 * instead. Handlers record observed effects via the `reproit` request
 * attribute. Every adapter path fails closed: instrumentation errors never
 * reach the host app.
 *
 * The PSR interfaces below are vendored as minimal declarations, each guarded
 * by interface_exists, so the package keeps zero runtime dependencies while
 * remaining drop-in compatible with real psr/http-message and
 * psr/http-server-middleware installs (theirs win when present).
 *
 * Route parameters are matched by the framework's router, usually after
 * middleware runs, so they are not part of the canonical input here. JSON
 * bodies are decoded up to a fixed cap; larger or non-JSON bodies are traced
 * without content.
 */

declare(strict_types=1);

namespace Psr\Http\Message {
    if (!\interface_exists(ServerRequestInterface::class)) {
        interface ServerRequestInterface
        {
        }
    }
    if (!\interface_exists(ResponseInterface::class)) {
        interface ResponseInterface
        {
        }
    }
}

namespace Psr\Http\Server {

    use Psr\Http\Message\ResponseInterface;
    use Psr\Http\Message\ServerRequestInterface;

    if (!\interface_exists(RequestHandlerInterface::class)) {
        interface RequestHandlerInterface
        {
            public function handle(ServerRequestInterface $request): ResponseInterface;
        }
    }
    if (!\interface_exists(MiddlewareInterface::class)) {
        interface MiddlewareInterface
        {
            public function process(
                ServerRequestInterface $request,
                RequestHandlerInterface $handler,
            ): ResponseInterface;
        }
    }
}

namespace ReproitBackend {

    use Psr\Http\Message\ResponseInterface;
    use Psr\Http\Message\ServerRequestInterface;
    use Psr\Http\Server\MiddlewareInterface;
    use Psr\Http\Server\RequestHandlerInterface;

    require_once __DIR__ . '/reproit.php';

    const MAX_BODY_BYTES = 64 * 1024;

    /** Decoded JSON value (objects as stdClass), or null for anything else. */
    function decode_json_body(string $body, string $contentType): mixed
    {
        if ($body === '' || \strlen($body) > MAX_BODY_BYTES) {
            return null;
        }
        if (!str_contains(strtolower($contentType), 'application/json')) {
            return null;
        }
        $decoded = json_decode($body);
        return json_last_error() === JSON_ERROR_NONE ? $decoded : null;
    }

    final class ReproitMiddleware implements MiddlewareInterface
    {
        private ?Capture $capture;
        /** @var callable|null fn(ServerRequestInterface): string */
        private $operation;
        /** @var callable|null fn(ServerRequestInterface): ?string */
        private $tenant;
        private bool $effectsComplete;

        public function __construct(
            ?Capture $capture = null,
            ?callable $operation = null,
            ?callable $tenant = null,
            bool $effectsComplete = false,
        ) {
            $this->capture = $capture;
            $this->operation = $operation;
            $this->tenant = $tenant;
            $this->effectsComplete = $effectsComplete;
        }

        public function process(
            ServerRequestInterface $request,
            RequestHandlerInterface $handler,
        ): ResponseInterface {
            $trace = null;
            $scan = false;
            try {
                [$trace, $scan, $request] = $this->beginTrace($request);
            } catch (\Throwable $ignored) {
                // Fail closed: an instrumentation defect must not break the request.
                $trace = null;
            }
            if ($trace === null) {
                return $handler->handle($request);
            }
            try {
                $response = $handler->handle($request);
            } catch (\Throwable $error) {
                $this->finishOnError($trace, $scan);
                throw $error;
            }
            try {
                return $this->finishTrace($trace, $scan, $response);
            } catch (\Throwable $ignored) {
                // Oversized or over-long traces drop their header; the response ships.
                return $response;
            }
        }

        private function beginTrace(ServerRequestInterface $request): array
        {
            $get = function (string $name) use ($request): ?string {
                $line = $request->getHeaderLine($name);
                return $line === '' ? null : $line;
            };
            $scanContext = trace_context_from_headers($get);
            $context = $scanContext;
            if ($context === null && $this->capture !== null) {
                $context = $this->capture->context();
            }
            if ($context === null) {
                return [null, false, $request];
            }
            $operation = $this->operation !== null
                ? (string) ($this->operation)($request)
                : $request->getMethod() . ' ' . $request->getUri()->getPath();
            $body = $request->getParsedBody();
            if ($body === null) {
                $body = decode_json_body(
                    (string) $request->getBody(),
                    $request->getHeaderLine('content-type'),
                );
            }
            $headers = [];
            foreach ($request->getHeaders() as $name => $values) {
                $headers[strtolower((string) $name)] =
                    \count($values) === 1 ? $values[0] : array_values($values);
            }
            $trace = BackendTrace::begin($context, $operation, [
                'tenant' => $this->tenant !== null ? ($this->tenant)($request) : null,
                'input' => http_input([
                    'body' => $body,
                    'query' => $request->getQueryParams(),
                    'headers' => $headers,
                ]),
            ]);
            return [$trace, $scanContext !== null, $request->withAttribute('reproit', $trace)];
        }

        private function finishTrace(
            BackendTrace $trace,
            bool $scan,
            ResponseInterface $response,
        ): ResponseInterface {
            if ($trace->finished()) {
                return $response;
            }
            $status = $response->getStatusCode();
            $output = decode_json_body(
                (string) $response->getBody(),
                $response->getHeaderLine('content-type'),
            );
            $trace->finish($output, $status, $status < 500, $this->effectsComplete);
            if ($scan) {
                return $response->withHeader('x-reproit-events', $trace->header());
            }
            $this->capture->record($trace);
            return $response;
        }

        private function finishOnError(BackendTrace $trace, bool $scan): void
        {
            try {
                if (!$trace->finished()) {
                    $trace->finish(null, 500, false, $this->effectsComplete);
                    if (!$scan && $this->capture !== null) {
                        $this->capture->record($trace);
                    }
                }
            } catch (\Throwable $ignored) {
                // Fail closed: the host's exception handling proceeds untouched.
            }
        }
    }
}
