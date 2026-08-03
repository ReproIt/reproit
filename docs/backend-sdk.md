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

Measured, not estimated. Every supported backend SDK has a gated middleware and dependency-capture
benchmark, so these numbers cannot quietly rot:

| adapter | mounted, request not traced | mounted, request traced | baseline request |
| --- | --- | --- | --- |
| Node (express) | below the measurement floor | ~25-30 µs | ~90-115 µs |
| Go (net/http) | below the measurement floor | ~25-30 µs | ~75-95 µs |
| Python (ASGI) | below the floor to ~7 µs | ~75-85 µs | ~180-220 µs |
| Rust (Axum) | below the measurement floor | ~160-185 µs | ~195-245 µs |
| Java (Jetty servlet) | below the floor to ~7 µs | ~10-60 µs | ~140-150 µs |
| .NET (Kestrel) | below the measurement floor | ~50-115 µs | ~330 µs |
| PHP (`php -S`, vanilla adapter) | ~10-35 µs | ~20-115 µs | ~200-225 µs |
| Ruby (WEBrick/Rack) | below the floor to ~70 µs | ~130 µs | ~245-310 µs |

| trace primitive alone | cost per call |
| --- | --- |
| Inactive | 0.08 µs |
| Active | 3.5 µs |

| captured dependency exchange | cost per exchange |
| --- | --- |
| Node | 2.33 µs |
| Go | 2.69 µs |
| Python | 11.11 µs |
| Rust | 17.69 µs |
| Java | 0.83 µs |
| .NET | 1.02 µs |
| PHP | 3.90 µs |
| Ruby | 12.34 µs |

The untraced column is the one that matters in production, because a request only carries trace
context when something asked for it. For Node and Go it reads "below the measurement floor"
rather than a number because that is the honest result: driving a real HTTP server, the
difference between having the adapter mounted and not having it can be smaller than the run-to-run
noise of the method itself. The primitive benchmark puts Node's inactive path at about 80
nanoseconds, which is consistent with being invisible at HTTP scale. A negative delta is reported
as below the floor, never as a speedup.

The traced column is a real cost per traced request. Local figures are single-threaded on an Apple
M1 Ultra with no I/O in the handler, so treat them as an order of magnitude rather than a promise
about your hardware. The .NET row was measured natively on the workspace's x86_64 Windows VM. The
snapshot above is intentionally separate from the ceilings: CI ceilings are wider, bounded
regression limits sized not to flap under ordinary shared-runner contention.

The middleware benchmarks drive real framework middleware over a real socket and report the delta
against an unmounted baseline by one method: alternating rounds, medians, and a second baseline per
round whose gap from the first is the method's own noise floor. The dependency benchmarks use the
same interleaving and report the incremental cost of appending a bounded, representative outbound
HTTP exchange after subtracting trace construction. Node, Go, and Python live under
`validation/backend/adapter-benchmark*` and `validation/backend/dependency-benchmark*`; the other
five live with their SDK suites. `validation/backend/benchmark.mjs` measures the Node primitive
underneath. Every benchmark fails the build if its noise or cost crosses its explicit ceiling.

## Next

A captured failure replays with `reproit check <capture.json> --exec "<your boot command>"`, with
the database stopped and the network denied. See [ReproIt in CI](ci.md) for the gate, and
[what a repro is made of](repros.md) for what the capture has to contain to be replayable.
