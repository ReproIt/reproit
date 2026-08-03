# Add ReproIt to a backend

Install, mount one middleware, done. Everything below is the framework adapter your SDK already
ships, so you keep your own router, your own HTTP client, and your own database driver.

> Packages are not published yet. The install lines below are the intended names; until they are on
> the registries, add the SDK from this repository by path or submodule. Nothing else changes: the
> middleware is the same either way.

## Node

```sh
npm install reproit-backend-node
```

```js
const reproitExpress = require('reproit-backend-node/express');
const instrument = require('reproit-backend-node/instrument');

instrument.install();          // capture outbound calls
app.use(reproitExpress({ capture }));
```

Fastify is the same shape: `require('reproit-backend-node/fastify')`.

## Python

```sh
pip install reproit-backend-py
```

```python
from reproit_backend_py.asgi import ReproitMiddleware

app.add_middleware(ReproitMiddleware, capture=capture)
```

FastAPI, Starlette, or any ASGI app.

## Go

```sh
go get github.com/ReproIt/reproit/sdk/reproit-backend-go
```

```go
import reproit "github.com/ReproIt/reproit/sdk/reproit-backend-go"

handler = reproit.Middleware(reproit.MiddlewareOptions{Capture: capture})(handler)
```

## Rust

```sh
cargo add reproit-backend --features axum
```

```rust
use reproit_backend::axum::ReproitLayer;

let app = Router::new()
    .route("/quote", get(quote))
    .layer(ReproitLayer::new(MiddlewareConfig { capture, ..Default::default() }));
```

Actix is `reproit_backend::actix`.

## Java

```xml
<dependency>
  <groupId>com.reproit</groupId>
  <artifactId>reproit-backend-java</artifactId>
</dependency>
```

```java
// any servlet container: register the filter ahead of your handlers
context.addFilter("reproit", new ReproitFilter(capture))
       .addMappingForUrlPatterns(null, false, "/*");
```

## .NET

```sh
dotnet add package ReproitBackend
```

```csharp
app.UseReproit(new ReproitOptions { Capture = capture });   // before the handlers
```

## PHP

```sh
composer require reproit/reproit-backend-php
```

```php
// any PSR-15 stack (Slim, Mezzio, Laminas)
$app->add(new ReproitBackend\ReproitMiddleware($capture));
```

## Ruby

```sh
gem install reproit-backend-rb
```

```ruby
# Rails, Sinatra, or bare Rack
use ReproitBackendRb::Middleware, capture: capture
```

## What the middleware does

It begins a trace per request, records the outbound calls your code already makes, and finishes
the trace with the response. Requests that carry no trace context are untouched, so the adapter is
inert until something asks it to record.

An instrumentation defect never breaks a request: every adapter catches its own errors and lets
the response through. That is deliberate, and it is why mounting it in production is a small
decision rather than a large one.

## Reaching the trace

The middleware puts the trace where your framework already keeps per-request state, so a handler
can record an effect it considers significant:

| language | where |
| --- | --- |
| Node | `req.reproit` |
| Python | `request.state.reproit`, or `scope["state"]["reproit"]` |
| Go | `reproit.FromRequest(r)` or `reproit.FromContext(ctx)` |
| Rust | `Extension<Recorder>` |
| Java | request attribute `reproit` |
| .NET | `context.ReproitTrace()` |
| PHP | `$request->getAttribute('reproit')` |
| Ruby | `env["reproit.trace"]` |

## What it costs

Measured, not estimated. Four benchmarks run on every push, so these numbers cannot quietly rot:

| adapter | mounted, request not traced | mounted, request traced | baseline request |
| --- | --- | --- | --- |
| Node (express) | below the measurement floor | ~25 µs | ~60 µs |
| Go (net/http) | below the measurement floor | ~25 µs | ~55 µs |
| Python (ASGI) | ~1-7 µs, at the measurement floor | ~75 µs | ~180 µs |

| trace primitive alone | cost per call |
| --- | --- |
| Inactive | 0.08 µs |
| Active | 3.5 µs |

The untraced column is the one that matters in production, because a request only carries trace
context when something asked for it. For Node and Go it reads "below the measurement floor"
rather than a number because that is the honest result: driving a real HTTP server, the
difference between having the adapter mounted and not having it is smaller than the run-to-run
noise of the method itself (about 1-2 µs). The primitive benchmark puts it at 80 nanoseconds,
which is consistent with being invisible at HTTP scale. Python's untraced cost lands at the floor
rather than under it: across three runs it measured -21 µs, +1.4 µs and +6.5 µs, so it is single
digits at most and the sign is not resolvable.

The traced column is a real cost, per traced request, and Python's is about three times the other
two. All figures are single-threaded on an Apple M1 Ultra with no I/O in the handler, so treat
them as an order of magnitude rather than a promise about your hardware; Python's run-to-run
spread is the widest of the three (a noisy run put its traced cost at 42 µs and its baseline at
237 µs), which is the interpreter, not the adapter.

Three benchmarks drive the real middleware over a real socket and report the delta against an
unmounted baseline, by one method: alternating rounds, medians, and a second baseline per round
whose gap from the first is the method's own noise floor.
`validation/backend/adapter-benchmark.mjs` (express), `validation/backend/adapter-benchmark-go`
(net/http) and `validation/backend/adapter-benchmark.py` (ASGI).
`validation/backend/benchmark.mjs` measures the primitive underneath. All four fail the build if
a cost crosses its ceiling.

**Not yet measured, and worth knowing:** the per-dependency capture path (each outbound HTTP call
and database query recorded onto a trace), and the Rust, Java, .NET, PHP and Ruby adapters. The
adapters share a design but not an implementation, so no number here transfers to one that has
not been run.

## Next

A captured failure replays with `reproit check <capture.json> --exec "<your boot command>"`, with
the database stopped and the network denied. See [ReproIt in CI](ci.md) for the gate, and
[what a repro is made of](repros.md) for what the capture has to contain to be replayable.
