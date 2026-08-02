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
reproit internal debug replay-capture capture.json
```

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), 16 operations per batch,
48 KB capture payload (trailing effect events dropped first, `captureDroppedEffects` counts
them), bounded flush interval (floor 100 ms), per-request timeout, and at most `retryLimit`
(cap 5) retries; 4xx responses are never retried. Redaction runs in `begin`/`effect`/`finish`,
before anything is queued. Uploads use `java.net.http.HttpClient` on one daemon thread.
`sdk/test/oracle_contract_test.js` pins the `backend-server-error` tagging contract.

## Agent oracles (LLM/agent operations)

An LLM/agent operation can mark an authored failure assertion on its own trace, the same
vocabulary as the Node and Python SDKs:

```java
trace.oracle(Capture.AGENT_GUARDRAIL_ORACLE, Map.of("tool", "delete_order"));
```

The three registry ids (lowest confidence tier) are `agent-response-content`,
`agent-guardrail-violation`, and `agent-loop-bound-exceeded` (`Capture.AGENT_ORACLES`).
Unknown ids are rejected with `TraceError` so a typo cannot mint an oracle category. The
marker rides as an `emit` effect (resource `reproit-oracle`), so the wire vocabulary is
unchanged. A marked operation is ALWAYS captured, like a 5xx, even when it returns 200; its
failure observation is a `contract-violation` whose signature carries the marked id instead
of the `backend-server-error` default, and the replayable capture payload's `oracle` field
is the marked id.

## CI capture mode (the flaky-CI wedge)

Same capsule, captured in CI instead of production: the trigger identity is the TEST
(`operation` = `test:<suite>#<test>`), the oracle is the existing
`backend-authored-invariant` id (a test IS an authored invariant), and the wire is
untouched. Two integration surfaces share one core (`Ci`):

- JUnit 5: `@ExtendWith(ReproitCi.class)` on the test class. Suite = the class's simple
  name, test = the method name. JUnit is a `provided` dependency: the extension only loads
  in suites that already ship JUnit 5, and the SDK jar stays zero-dependency for apps.
- The dependency-free micro-runner, for fixtures and hermetic gates (compiles with plain
  javac, no jars): `Ci.Suite suite = Ci.suite("checkout"); suite.test(name, body);
  System.exit(suite.exitCode());`

With `REPROIT_CI_CAPTURE=1` every test runs inside its own trace, so the wrapped outbound
clients record dependency exchanges plus the determinism envelope exactly as production
capture does. A FAILING test spools a version-2 capture capsule to a bounded on-disk spool
(`REPROIT_CI_SPOOL`, default `.reproit/ci-spool`; total-bytes cap `REPROIT_CI_SPOOL_MAX`,
default 16 MiB, floor 4 KiB, ceiling 64 MiB; over-cap capsules are dropped and counted in
`Ci.stats()` plus the on-disk `dropped.count`, never silently) and announces it with the
`REPROIT:CI-CAPSULE` stderr marker. The failure always reaches the runner untouched.

`reproit check <capsule> --exec "java -cp classes CheckoutTest"` re-runs the command with
`REPROIT_REPLAY` set: only the capsule's named test runs (everything else is skipped or
disabled with the target named), the SDK serves the recorded exchanges in process, and the
observed result is the `REPROIT:CI-TEST` stderr marker the CLI's four-way verdict reads
(reproduced / fixed-under-the-capsule / diverged / inconclusive). A plain rerun passing
OUTSIDE the capsule is flaky evidence, never Fixed. Honest limit, same as Node's: races the
exchange boundary cannot see are Inconclusive, never a fake reproduction.

Fixture: `examples/java-flaky-ci-fixture/` (planted order-dependent failure invisible in a
plain run); gate: `validation/backend/java-flaky-ci-e2e/run.sh` (six legs: plain run
passes, CI run spools, plain rerun passes without a fix, reproduce / fix / revert /
deleted-exchange-diverges under the PORTABILITY bar, replay compiled with plain javac).

## Capsule parity: outbound exchange capture and hermetic replay

The capsule boundary is library-layer only (no `-javaagent`, no bytecode weaving), per the
Track 2x decision. Route outbound HTTP through the delegating client and database statements
through the JDBC wrap; both record onto the ambient trace (`Instrument.scope`, or the servlet
filter's request scope) and both SERVE the recorded exchanges when `REPROIT_REPLAY` names a
capture payload:

```java
HttpClient client = ReproitHttpClient.wrap(HttpClient.newHttpClient());
Connection db = ReproitJdbc.connect(() -> DriverManager.getConnection(url));  // stub in replay
Random random = Instrument.random();  // seeded from the envelope in replay
Clock clock = Instrument.clock();     // offset to the capture moment in replay
```

Bounds are byte-identical to the Node reference: 8 KiB inline body budget (over-cap bodies keep
byte count + sha256 + a truncated marker and replay fails closed on them), 32 name-sorted
headers, 64 db rows, 128 stream chunk boundaries. Streaming responses (SSE) are observed via a
TEE subscriber as the app consumes them and the exchange records at EOF; an abandoned body
records nothing. Replay matching is strict per-operation ordinals; the first unmatched call
emits the `REPROIT:DIVERGENCE` marker (byte-identical to Node's, `bodyDelta` included) and
answers 599 (HTTP) or throws `SQLException` (JDBC). The explicit `Instrument.Http.send` /
`Instrument.Db.run` boundary remains for apps that prefer it.

NAMED capability gaps of the no-weaving boundary (each a gap, never a silent downgrade):

- Only wrapped clients/connections are visible. A bare `HttpClient` or a direct
  `DriverManager.getConnection` records nothing and reaches the real network at replay.
- `System.currentTimeMillis`, `System.nanoTime` and `Instant.now` cannot be intercepted;
  `Instrument.clock()` is the pinned source. Direct reads stay live.
- `Random` instances the app constructs, `Math.random`, and `ThreadLocalRandom` are not
  reseedable; `Instrument.random()` is the pinned source. `SecureRandom` is unpinnable by
  design and stays live everywhere.
- JDBC surface: `executeQuery`/`executeUpdate` on Statement/PreparedStatement with indexed
  parameters. Batch APIs, `CallableStatement`, generated keys, scrollable cursors and
  multi-result `execute()` pass through unrecorded in capture and fail loudly in replay.
- HTTP/2 push promises pass through unrecorded; replay ignores the push handler.
- `TimeZone.setDefault`/`Locale.setDefault` pin zone- and locale-aware code; code reading the
  `TZ` environment variable directly is not affected.

The money-test fixture lives in `examples/java-backend-fixture/` and the four-verdict gate in
`validation/backend/java-hermetic-e2e/run.sh` (capture, portability copy, reproduce / fix /
revert / deleted-exchange-diverges). `sdk/test/backend_replay_parity_test.js` byte-compares the
served exchange, the 599 body, and the divergence marker against the Node reference.

## Level matrix against the Node reference

Founder rule: every capability the Node SDK has, this SDK has, and a genuinely-impossible
surface is a NAMED gap here, never a silent downgrade.

| Node surface (file)                          | Java surface                       | Level |
| -------------------------------------------- | ---------------------------------- | ----- |
| Scan-time trace, bounds, redaction, header (index.js) | `BackendTrace`, `Json`      | same, golden-byte pinned |
| Framework adapters (express.js, fastify.js)  | `ReproitFilter` (any jakarta.servlet container) | same role; servlet is the Java-idiomatic host |
| Production capture + batch shape (capture.js) | `Capture`                         | same, batch shape pinned |
| Agent oracle API (`trace.oracle`, AGENT_* ids, marked-op capture) | `BackendTrace.oracle`, `Capture.AGENT_ORACLES` | same |
| Outbound HTTP capture (instrument.js patches http/https/fetch process-wide) | `ReproitHttpClient.wrap`, `Instrument.Http.send` | same recording; NAMED GAP: no auto-install. Library layer only, no bytecode weaving, so only wrapped clients are visible |
| DB capture (instrument.js patches the pg driver) | `ReproitJdbc`, `Instrument.Db.run` | same recording; NAMED GAP as above, plus the JDBC subset listed under capsule parity |
| Determinism envelope (seeded RNG, pinned clock/TZ/locale) | `Instrument.random()`, `Instrument.clock()`, envelope pin | same seams; NAMED GAP: direct `System.currentTimeMillis`/`Instant.now`, app-constructed `Random`, `Math.random`, `ThreadLocalRandom` stay live; `SecureRandom` unpinnable by design |
| Hermetic replay, strict ordinals, 599, `REPROIT:DIVERGENCE` + `bodyDelta` (replay.js) | `Replay` | same, marker byte-identical |
| Streaming (SSE) exchanges with chunk boundaries | TEE subscriber in `ReproitHttpClient` | same |
| CI capture mode (ci.js wraps node:test)      | `ReproitCi` (JUnit 5) + `Ci.suite` micro-runner | same semantics, markers and spool bounds identical; the runner wrapped differs because the runtimes' test frameworks differ |

## Tests

```sh
cd sdk/reproit-backend-java
mvn test   # unit + batch validation + Jetty servlet e2e against a stub ingest
```

Unit tests pin bounds, redaction, tagging, and batch shape (through a Java port of
`sdk/test/event_batch_v1.js`), plus golden-byte canonical JSON parity with the Node SDK. The e2e
test runs `ReproitFilter` in a real Jetty container with a planted 500 and asserts the tagged
finding batch at a local stub ingest, and the scan-time `x-reproit-events` round-trip.
`AgentOracleTest` pins the marked-oracle contract, `CiTest` the CI capture/replay/spool
semantics against the Node reference's ci.test.js, and `CiExtensionTest` runs the JUnit 5
extension through the embedded platform launcher.
