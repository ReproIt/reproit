<?php

// PSR-15 adapter tests: scan-time header round-trip, capture-mode recording,
// and fail-closed behavior, against minimal in-test PSR-7 fakes (the vendored
// interface declarations in psr15.php). Run: php test/psr15_test.php

declare(strict_types=1);

namespace ReproitBackend\Test;

use Psr\Http\Message\ResponseInterface;
use Psr\Http\Message\ServerRequestInterface;
use Psr\Http\Server\RequestHandlerInterface;
use ReproitBackend\Capture;
use ReproitBackend\ReproitMiddleware;

require __DIR__ . '/../psr15.php';
require __DIR__ . '/support.php';

final class FakeUri
{
    public function __construct(private string $path)
    {
    }

    public function getPath(): string
    {
        return $this->path;
    }
}

final class FakeRequest implements ServerRequestInterface
{
    public array $attributes = [];

    public function __construct(
        private string $method,
        private string $path,
        private array $headers = [],
        private string $body = '',
        private array $query = [],
    ) {
    }

    public function getHeaderLine(string $name): string
    {
        return (string) ($this->headers[strtolower($name)] ?? '');
    }

    public function getHeaders(): array
    {
        $headers = [];
        foreach ($this->headers as $name => $value) {
            $headers[$name] = [$value];
        }
        return $headers;
    }

    public function getMethod(): string
    {
        return $this->method;
    }

    public function getUri(): FakeUri
    {
        return new FakeUri($this->path);
    }

    public function getParsedBody(): mixed
    {
        return null;
    }

    public function getBody(): FakeBody
    {
        return new FakeBody($this->body);
    }

    public function getQueryParams(): array
    {
        return $this->query;
    }

    public function withAttribute(string $name, mixed $value): self
    {
        $clone = clone $this;
        $clone->attributes[$name] = $value;
        return $clone;
    }
}

final class FakeBody
{
    public function __construct(private string $content)
    {
    }

    public function __toString(): string
    {
        return $this->content;
    }
}

final class FakeResponse implements ResponseInterface
{
    public array $headers = [];

    public function __construct(private int $status, private string $body)
    {
    }

    public function getStatusCode(): int
    {
        return $this->status;
    }

    public function getBody(): FakeBody
    {
        return new FakeBody($this->body);
    }

    public function getHeaderLine(string $name): string
    {
        return (string) ($this->headers[strtolower($name)] ?? 'application/json');
    }

    public function withHeader(string $name, string $value): self
    {
        $clone = clone $this;
        $clone->headers[strtolower($name)] = $value;
        return $clone;
    }
}

final class FakeHandler implements RequestHandlerInterface
{
    public ?ServerRequestInterface $seen = null;

    /** @param callable(ServerRequestInterface): ResponseInterface $respond */
    public function __construct(private $respond)
    {
    }

    public function handle(ServerRequestInterface $request): ResponseInterface
    {
        $this->seen = $request;
        return ($this->respond)($request);
    }
}

// scan-time: trace round-trips into the x-reproit-events response header
$middleware = new ReproitMiddleware();
$handler = new FakeHandler(function (FakeRequest $request) {
    $request->attributes['reproit']->effect('write', ['resource' => 'orders', 'key' => '1']);
    return new FakeResponse(200, '{"ok":true}');
});
$request = new FakeRequest('POST', '/orders', [
    'x-reproit-trace' => 'trace-psr',
    'x-reproit-actor' => 'alice',
    'content-type' => 'application/json',
], '{"item":"widget","apiKey":"sk_live_leak"}');
$response = $middleware->process($request, $handler);
$header = $response->headers['x-reproit-events'] ?? '';
check($header !== '', 'scan-time response carries x-reproit-events');
$padded = strtr($header, '-_', '+/') . str_repeat('=', -\strlen($header) % 4 & 3);
$events = json_decode((string) base64_decode($padded), true);
check_same('trace-psr', $events[0]['traceId'], 'decoded trace id');
check_same('alice', $events[0]['actor'], 'decoded actor');
check_same(['start', 'effect', 'return'], array_map(
    fn (array $event) => $event['kind'],
    $events,
), 'start/effect/return sequence');
check_same(
    true,
    $events[0]['input']['body']['apiKey']['$reproit']['redacted'] ?? null,
    'secret-shaped body field redacted',
);
check_same('POST /orders', $events[0]['operation'], 'default operation is METHOD path');
check_same(200, $events[2]['status'], 'return status recorded');

// no trace header and no capture: middleware is inert
$middleware = new ReproitMiddleware();
$handler = new FakeHandler(fn () => new FakeResponse(200, '{"ok":true}'));
$response = $middleware->process(new FakeRequest('GET', '/ok'), $handler);
check(!isset($response->headers['x-reproit-events']), 'inert without x-reproit-trace');
check(!isset($handler->seen->attributes['reproit']), 'no trace attribute when inert');

// capture mode: a 500 response is recorded, a healthy one is not
$capture = Capture::create([
    'endpoint' => 'http://c/v1/events', 'apiKey' => 'sk', 'appId' => 'app',
]);
$middleware = new ReproitMiddleware($capture);
$boom = new FakeHandler(function (FakeRequest $request) {
    $request->attributes['reproit']->effect('write', ['resource' => 'orders', 'key' => '1']);
    return new FakeResponse(500, '{"error":"boom"}');
});
$response = $middleware->process(new FakeRequest('POST', '/boom'), $boom);
check(!isset($response->headers['x-reproit-events']), 'capture mode adds no scan header');
check_same(1, $capture->stats()['capturedOperations'], '500 response captured');
$middleware->process(new FakeRequest('GET', '/ok'), new FakeHandler(
    fn () => new FakeResponse(200, '{"ok":true}'),
));
check_same(1, $capture->stats()['capturedOperations'], 'healthy response not captured');

// a handler exception propagates and is captured as a failed operation
$middleware = new ReproitMiddleware($capture);
$thrown = false;
try {
    $middleware->process(new FakeRequest('GET', '/crash'), new FakeHandler(
        function (): FakeResponse {
            throw new \RuntimeException('boom');
        },
    ));
} catch (\RuntimeException $error) {
    $thrown = $error->getMessage() === 'boom';
}
check($thrown, 'handler exception propagates untouched');
check_same(2, $capture->stats()['capturedOperations'], 'exception captured as failure');
$queue = new \ReflectionProperty(Capture::class, 'queue');
$queue->setValue($capture, []); // keep the process-end shutdown drain a no-op

report('psr15_test');
