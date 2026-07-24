# ReproIt backend adapter for Java

This package is an internal validation surface, not a published compatibility API. It is inactive
unless a trusted request contains `x-reproit-trace`. It is a port of the Rust reference adapter
(`sdk/reproit-backend-rs`) with the same bounds, redaction, and wire format, in plain Java 17 with
zero runtime dependencies (the servlet API is provided by the host container).

Framework integrations pass their header lookup into `BackendTrace.traceContextFromHeaders`, start
an operation with `BackendTrace.begin`, record only effects actually observed by the adapter, then
call `finish` and return `header()` as `x-reproit-events`. Set `effectsComplete` only when the
adapter observed every persistent effect in the operation. Tenant and resource identifiers must be
non-secret structural identifiers.

The adapter enforces bounded identifiers, 256 events, a 60 KB encoded header, typed effects, one
return, no effects after return, hashed idempotency identity, and recursive structural redaction.
GraphQL callers may attach parser-produced `selection` mappings; never infer selections from
response content.

## Servlet filter (any jakarta.servlet container, including Spring Boot)

`ReproitFilter` buffers the request body (JSON up to 64 KB, decoded query values, lowercased
headers), begins the trace, holds the response body (bounded) so the trace finishes before
anything is committed, and attaches `x-reproit-events` on scan-time requests. Handlers record
observed effects through the `reproit` request attribute:

```java
import dev.reproit.backend.BackendTrace;
import dev.reproit.backend.Capture;
import dev.reproit.backend.ReproitFilter;

Capture capture = Capture.create(new Capture.Config()
    .endpoint("https://cloud.example.com/v1/events") // ingest endpoint
    .apiKey("sk_live_...")                           // project API key (Authorization: Bearer)
    .appId("app-id")                                 // Cloud project app id
    .build("1.4.2"));                                // optional deployment identity

// Plain servlet container: register the filter on /*. Pass no capture
// (new ReproitFilter()) for scan-time only.
// Spring Boot (no Spring dependency needed here): register it as a bean.
@Bean
public FilterRegistrationBean<ReproitFilter> reproitFilter() {
    FilterRegistrationBean<ReproitFilter> bean =
        new FilterRegistrationBean<>(new ReproitFilter(capture));
    bean.addUrlPatterns("/*");
    bean.setOrder(Ordered.HIGHEST_PRECEDENCE);
    return bean;
}

// In a handler (servlet, Spring controller with HttpServletRequest, ...):
BackendTrace trace = (BackendTrace) request.getAttribute(ReproitFilter.REQUEST_ATTRIBUTE);
if (trace != null) {
    trace.effect("write", new BackendTrace.Effect().resource("orders").key("1"));
}
```

Every adapter path fails closed: an instrumentation defect never breaks the request.

## Production capture mode (off by default)

Capture mode uploads finished traces to Cloud ingest without requiring `x-reproit-trace`. It is
config-gated: nothing leaves the process unless the host constructs a `Capture`.
`Capture.create(config)` returns `null` (capture disabled, host unaffected) when the config is
unusable. `capture.record(trace)` never blocks, never throws, and never surfaces errors.

Sampling: operations whose return reports `success == false` or HTTP 5xx are always captured;
healthy operations are captured only under `healthySamplePerMille` (default 0, backend frames
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
them), bounded flush interval (floor 100 ms), per-request timeout, and at most `retryLimit`
(cap 5) retries; 4xx responses are never retried. Redaction runs in `begin`/`effect`/`finish`,
before anything is queued. Uploads use `java.net.http.HttpClient` on one daemon thread.
`sdk/test/oracle_contract_test.js` pins the `backend-server-error` tagging contract.

## Tests

```sh
cd sdk/reproit-backend-java
mvn test   # unit + batch validation + Jetty servlet e2e against a stub ingest
```

Unit tests pin bounds, redaction, tagging, and batch shape (through a Java port of
`sdk/test/event_batch_v1.js`), plus golden-byte canonical JSON parity with the Node SDK. The e2e
test runs `ReproitFilter` in a real Jetty container with a planted 500 and asserts the tagged
finding batch at a local stub ingest, and the scan-time `x-reproit-events` round-trip.
