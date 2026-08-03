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

## Next

A captured failure replays with `reproit check <capture.json> --exec "<your boot command>"`, with
the database stopped and the network denied. See [ReproIt in CI](ci.md) for the gate, and
[what a repro is made of](repros.md) for what the capture has to contain to be replayable.
