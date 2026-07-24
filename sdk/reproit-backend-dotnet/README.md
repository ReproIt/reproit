# ReproIt backend adapter for .NET

This package is an internal validation surface, not a published compatibility API. It is inactive
unless a trusted request contains `x-reproit-trace`. It is a port of the Rust reference adapter
(`sdk/reproit-backend-rs`) with the same bounds, redaction, and wire format, targeting net8.0
with zero NuGet dependencies (the ASP.NET Core adapter sits behind a shared framework
reference).

Framework integrations pass their header lookup into `Reproit.TraceContextFromHeaders`, start an
operation with `BackendTrace.Begin`, record only effects actually observed by the adapter, then
call `Finish` and return `Header()` as `x-reproit-events`. Set `EffectsComplete` only when the
adapter observed every persistent effect in the operation. Tenant and resource identifiers must be
non-secret structural identifiers.

The adapter enforces bounded identifiers, 256 events, a 60 KB encoded header, typed effects, one
return, no effects after return, hashed idempotency identity, and recursive structural redaction.
GraphQL callers may attach parser-produced `Reproit.Selection` mappings; never infer selections
from response content.

## ASP.NET Core middleware

`UseReproit` mounts before the handlers: it builds the canonical decoded input (JSON body up to
64 KB, decoded query values, lowercased headers), begins the trace, and holds the response so the
trace finishes and `x-reproit-events` attaches before headers flush on scan-time requests.
Handlers record observed effects through `httpContext.ReproitTrace()`:

```csharp
using ReproitBackend;

var capture = Capture.Create(new CaptureConfig
{
    Endpoint = "https://cloud.example.com/v1/events", // ingest endpoint
    ApiKey = "sk_live_...",                           // project API key (Authorization: Bearer)
    AppId = "app-id",                                 // Cloud project app id
    Build = "1.4.2",                                  // optional deployment identity
});
var app = builder.Build();
app.UseReproit(new ReproitOptions { Capture = capture }); // Capture = null: scan-time only

app.MapPost("/orders", (HttpContext context) =>
{
    context.ReproitTrace()?.Effect("write", new EffectOptions { Resource = "orders", Key = "1" });
    // ...
});
```

Every adapter path fails closed: an instrumentation defect never breaks the request.

## Production capture mode (off by default)

Capture mode uploads finished traces to Cloud ingest without requiring `x-reproit-trace`. It is
config-gated: nothing leaves the process unless the host constructs a `Capture`.
`Capture.Create(config)` returns `null` (capture disabled, host unaffected) when the config is
unusable. `capture.Record(trace)` never blocks, never throws, and never surfaces errors.

Sampling: operations whose return reports `success == false` or HTTP 5xx are always captured;
healthy operations are captured only under `HealthySamplePerMille` (default 0, backend frames
only, no finding). A 5xx capture is posted as an event-batch-v1 batch: every trace event as a
`backend` frame plus one `finding` frame tagged with the first-class `backend-server-error`
oracle id, whose `context.reproitCapture` object carries the full redacted start/effects/return
sequence for deterministic local replay:

```sh
# fetch the finding from /v1/errors/:app, save context.reproitCapture as capture.json, then:
reproit debug replay-capture capture.json
```

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), 16 operations per batch,
48 KB capture payload (trailing effect events dropped first, `captureDroppedEffects` counts
them), bounded flush interval (floor 100 ms), per-request timeout, and at most `RetryLimit`
(cap 5) retries; 4xx responses are never retried. Redaction runs in `Begin`/`Effect`/`Finish`,
before anything is queued. Uploads run on one background thread over a shared HttpClient, off
the request path. `sdk/test/oracle_contract_test.js` pins the `backend-server-error` tagging
contract.

## Tests

```sh
cd sdk/reproit-backend-dotnet
dotnet test ReproitBackend.Tests   # unit + batch validation + Kestrel e2e against a stub ingest
```

The suite includes a golden-bytes canonical JSON parity test against the Node SDK's
`canonicalJson` (requires `node` on PATH; set `REPROIT_CLI_ROOT` when running out of tree).
