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
    Endpoint = "https://cloud.example.com/v1/capture-batches",
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

Sampling: operations whose return reports `success == false` or HTTP 5xx are always captured.
Healthy operations are captured only under `HealthySamplePerMille` (default 0). Each operation
becomes one universal source-neutral capture batch with typed request, state, dependency, effect,
and failure events:

```sh
reproit occ_...
```

Bounds are fixed: queue depth 64 operations with drop-oldest overflow, one operation per batch,
1,024 causal events per operation, bounded flush interval, per-request timeout, and at most
`RetryLimit` (cap 5) retries. A 4xx response is never retried. Redaction runs in
`Begin`/`Effect`/`Finish`, before anything is queued. Uploads run on one background thread over a
shared HttpClient, off the request path.

`UniversalRecorder` is the framework-neutral API for Windows services, desktop applications,
installers, migrations, scheduled jobs, commands, messages, and other .NET software. Its semantic
methods match the Node and Rust recorder cores. `CaptureValues.Replayable`, `Structural`, and
`EnvironmentBound` preserve the portability boundary at the call site. External session, trace,
span, and actor identifiers that do not fit the wire token grammar become deterministic SHA-256
correlation tokens instead of being dropped.

## Capsule boundary: outbound exchanges and hermetic replay

.NET has no monkeypatching, so the exchange boundary is explicit and opt-in
(Track 2x boundary: HttpMessageHandler + ADO.NET wrap):

```csharp
var client = new HttpClient(Instrument.Handler());       // outbound HTTP
var db = Ado.Wrap(new NpgsqlConnection(connectionString)); // ADO.NET, pg wire shape
```

Every dependency call made while a request trace is ambient records an `exchange` (the
request the app sent and the response the dependency returned), bounded exactly like the
Node reference: 8 KiB inline body budget (over it: byte count + sha256 + truncated marker,
replay fails closed), 32 headers capped over name-sorted order (digest over every byte),
64 db rows, 128 stream chunk boundaries. Streaming responses (SSE / chunked) record their
chunk boundaries through a TEE stream as the app consumes the body; an abandoned body
records nothing. `Instrument.Db.RunAsync` remains for statements no ADO.NET provider
carries.

With `REPROIT_REPLAY` naming a capture, the SAME boundary serves the recorded exchanges:
no socket opens, `Ado.Wrap`'s connect stub answers `Open()` so the app boots with the
database down, matching is strict per-operation ordinals, and the first unmatched call
fails closed with the structured `REPROIT:DIVERGENCE` stderr line (byte-identical to the
Node SDK's, `bodyDelta` included). `sdk/test/backend_replay_parity_test.js` pins the
served bytes and the marker against the Node golden bytes;
`validation/backend/dotnet-hermetic-e2e/run.sh` is the money-test gate
(capture, PORTABILITY copy, reproduce / fix / revert / deleted-exchange verdicts).

Named capability gaps, not silent downgrades:

- `System.Security.Cryptography.RandomNumberGenerator` reads the OS CSPRNG directly and
  cannot be pinned; only `Instrument.RandomSource` (the envelope-seeded `System.Random`)
  replays deterministically.
- Direct `DateTime.Now` / `DateTime.UtcNow` reads cannot be intercepted without profiler
  APIs; `Instrument.Time` is the pinned `TimeProvider`. The time ZONE is pinned
  process-wide on Unix; Windows resolves the zone from the registry and keeps the
  readable `Instrument.ReplayTimeZone()` fallback.
- Recorded db row cells are reduced to JSON-safe primitives (DateTime/Guid/byte[] become
  strings); replay serves those primitives back.
- CultureInfo (locale) is not pinned: the envelope's Node reference carries no locale
  field, and .NET culture comes from the OS.

## Agent oracle API

LLM/agent operations can mark authored assertions on their own trace with
`trace.Oracle(id, detail)`. The ids are the three registry agent oracles
(`agent-response-content`, `agent-guardrail-violation`, `agent-loop-bound-exceeded`); an unknown
id throws (`InvalidOperation`), so a typo cannot mint an oracle category. The marker rides as an
`emit` effect (resource `reproit-oracle`), so the wire vocabulary is unchanged. Capture mode
always uploads a marked operation, even without a 5xx, and its failure observation is a
`contract-violation` carrying the marked id instead of the 5xx default:

```csharp
trace.Oracle(Capture.AgentGuardrailOracle, new Dictionary<string, object?>
{
    ["tool"] = "delete_order",
});
```

## CI capture mode (the flaky-CI wedge)

`Ci.TestAsync(suite, test, body)` wraps an xUnit test body with a TEST trigger identity: the
existing `operation` field carries `test:<suite>#<test>` and a failed test's capsule carries the
existing `backend-authored-invariant` registry oracle (a test IS an authored invariant). No new
protocol fields, no new oracle ids.

```csharp
[Fact]
public Task OrderTotal() =>
    Ci.TestAsync("checkout", "order total applies the configured tax rate", async () =>
        Assert.Equal(125d, await Order.TotalAsync(Client, ConfigUrl, 100)));
```

- `REPROIT_CI_CAPTURE=1`: every wrapped test runs inside its own trace, the Instrument boundary
  records dependency exchanges and the determinism envelope, and a FAILING test spools a
  version-2 capsule to the bounded on-disk spool (`REPROIT_CI_SPOOL`, default
  `.reproit/ci-spool`; total-bytes cap `REPROIT_CI_SPOOL_MAX`, default 16 MiB, floor 4 KiB,
  ceiling 64 MiB; over-cap capsules are dropped and counted in `dropped.count`, never silently).
- `REPROIT_REPLAY=<capsule>`: the SAME wrapper re-runs only the capsule's named test with the
  recorded exchanges served in process, and reports the observed result as the structured
  `REPROIT:CI-TEST` stderr marker `reproit check` parses.
- Neither env: the wrapper runs the body untouched.

`reproit check <capsule> --exec "dotnet test <proj> --filter FullyQualifiedName=<test> --logger
\"console;verbosity=detailed\" 1>&2"` re-executes the exact failing run and maps the observed
result to the four-way verdict (reproduced / fixed / diverged / inconclusive). The logger and
redirect are load-bearing: the VSTest host swallows raw test console output, and the detailed
console logger re-prints the SDK's markers verbatim on stdout for the redirect to hand to
`reproit check`. `validation/backend/dotnet-flaky-ci-e2e/run.sh` is the gate
(plain-run-passes, flaky rerun, reproduce / fix / revert / deleted-exchange verdicts from a
PORTABILITY copy); `fixtures/dotnet-flaky-ci-fixture` is the planted order-dependent failure.

Honest limit, same as the Node reference: replay pins the envelope and the recorded exchanges,
which is the whole boundary this SDK can see. A race the boundary cannot see (scheduling, shared
memory) is not reproduced by this capsule; `reproit check` reports such runs Inconclusive.

## Level matrix against the Node reference

Founder rule: every backend SDK sits at the same level as `sdk/reproit-backend-node` in all
ways; anything genuinely impossible is a named gap here, never silent.

| Node surface | .NET surface | Status |
| --- | --- | --- |
| Scan-time trace core (`BackendTrace`, bounds, redaction) | `BackendTrace` | level |
| `traceContextFromHeaders` / `selection` / `httpInput` | `Reproit.*` | level |
| `canonicalJson` (golden-bytes parity) | `Json.Canonical` | level |
| Express middleware + Fastify plugin | ASP.NET Core `UseReproit` | level (per ecosystem) |
| Ambient trace (`traceStorage` / `currentTrace`) | `Instrument.ScopeAsync` (AsyncLocal) | level |
| Production capture mode (`Capture`) | `Capture` | level |
| Agent oracle API (`trace.oracle`) | `Trace.Oracle`, `Capture.MarkedOracle` | level |
| Exchange capture (`instrument.install()`) | `Instrument.Handler` / `Ado.Wrap` | named gap 1 |
| Hermetic replay + `REPROIT:DIVERGENCE` | `Replay` (marker byte-identical) | level, gaps 4 |
| CI capture mode (`ci.suite`) | `Ci.TestAsync` | level, named gaps 2 and 3 |
| `ci.stats()` | `Ci.Stats()` | level |
| `ci.suite` unknown-option rejection | typed API, no options bag | level (non-gap) |

Named gaps (genuinely impossible surfaces, stated rather than faked):

1. .NET has no monkeypatching, so the exchange boundary is explicit opt-in
   (`Instrument.Handler`, `Ado.Wrap`, `Db.RunAsync`); a call not routed through it is
   invisible to capture and unavailable at replay. Node's `instrument.install()` rewires the
   process's http/fetch/pg clients with no call-site changes.
2. The CI wrapper is an explicit call, not a drop-in test-runner shim: xUnit v2 exposes no
   hook that both wraps the body and observes its failure. Replay's skip-of-other-tests is
   `--filter` selection, because xUnit v2 has no dynamic skip.
3. The VSTest host swallows raw test console output, so the CI/divergence markers cross
   `dotnet test` only via `--logger "console;verbosity=detailed"` (verbatim re-print on
   stdout) plus a `1>&2` redirect. Outside VSTest the markers are plain stderr, as in Node.
4. Replay determinism gaps are the four listed under the capsule boundary section above
   (OS CSPRNG, direct `DateTime.Now`, db cell primitives, culture).

## Tests

```sh
cd sdk/reproit-backend-dotnet
dotnet test ReproitBackend.Tests   # unit + batch validation + Kestrel e2e against a stub ingest
../../validation/backend/dotnet-flaky-ci-e2e/run.sh   # CI-capture wedge gate (six legs)
```

The suite includes a golden-bytes canonical JSON parity test against the Node SDK's
`canonicalJson` (requires `node` on PATH; set `REPROIT_CLI_ROOT` when running out of tree).
