# Backend quickstart

ReproIt drives an HTTP/GraphQL/gRPC service from its schema: `scan` exercises the read-only
operations against their declared contracts, `fuzz` generates valid and wrong-typed request
sequences and confirms every violation by exact replay before it is called a finding.

## The four commands

```
# 1. Initialize from the schema your service serves (or a schema file on disk).
#    The schema is snapshotted into the project and its origin becomes the target.
reproit init http://127.0.0.1:4477/openapi.json      # or: reproit init openapi.yaml

# 2. Boot a DISPOSABLE instance of your service, then confirm the setup:
reproit doctor          # schema parses, target answers, adapter tier

# 3. Find bugs.
reproit scan            # read-only contract checks
reproit fuzz            # stateful sequences, wrong-typed probes, lifecycle checks

# 4. Work a finding.
reproit fnd_<id>        # replay one finding exactly (exit 1 while it reproduces)
reproit inspect fnd_<id>   # step through it operation by operation, effects included
```

Backend findings persist under `.reproit/findings/` and replay by id; the saved-suite
`keep`/`check` guard flow currently requires an app-platform configuration and is not yet
available in a schema-only backend project.

If `reproit init` (no arguments) runs in a backend repo without a schema, it detects the
framework from the manifests (Cargo.toml, package.json, pyproject/requirements, pom/gradle,
Gemfile, composer.json, go.mod) and prints the framework-specific way to produce one. No config
is written until a schema exists: the schema is the contract everything else derives from.
When no generator is practical, `reproit init --learn` can derive a draft schema straight
from the source (see below).

## Getting a schema, per framework

- FastAPI: already served at `/openapi.json`; `reproit init http://localhost:8000/openapi.json`.
- django-ninja serves `/api/openapi.json`; Django REST needs drf-spectacular (`/api/schema/`).
- Spring: springdoc-openapi serves `/v3/api-docs`.
- Express/Koa/hapi: swagger-jsdoc, or a hand-written `openapi.yaml`.
- Fastify: `@fastify/swagger` serves `/documentation/json`.
- axum/actix/warp: utoipa (utoipa-swagger-ui serves `/api-docs/openapi.json`).
- Rails: rswag writes `swagger/v1/swagger.yaml`. Sinatra: hand-write `openapi.yaml`.
- Laravel: l5-swagger. Symfony: NelmioApiDocBundle (`/api/doc.json`).
- Go (gin/echo/fiber/chi/net-http): swaggo/swag writes `docs/swagger.json`.

GraphQL introspection JSON and protobuf descriptors work the same way as OpenAPI documents.

## No schema at all: `reproit init --learn`

If your framework has no schema generator wired up (a bare Express app, plain axum, Flask
without extensions), `reproit init --learn` derives a **draft** schema from the route
definitions in your source:

```
reproit init --learn                              # static derivation only
reproit init --learn --target http://localhost:3000   # plus one observed GET per route
```

It detects the framework from the manifests, extracts route paths and methods with
per-framework source patterns (`app.get('/x')`, `@app.get("/x")`, `.route("/x", get(...))`,
`r.GET("/x")`, `#[get("/x")]`, `Route::get('/x')`, `resources :x`, `@GetMapping`, ...),
normalizes path params (`:id`, `<int:id>`, `{id:regex}`) to OpenAPI `{id}`, and writes
`openapi.yaml` plus the standard backend `reproit.yaml`. With a resolvable target (`--target`
or `REPROIT_BACKEND_URL`) it additionally sends one bounded GET per derived parameterless GET
route (never any other method; at most 32 routes in a 10 second budget) and records the
observed status and JSON shape, with the adapter's effect kinds as comments when the trail
answers.

Be clear about what the draft is: routes read from source with loose placeholder types, marked
`x-reproit-derived: true` and headed by a comment saying so. Anything the patterns cannot
extract confidently is skipped and counted, never guessed, and a project where nothing can be
derived fails with the schema guide instead of writing an empty schema. A loose draft means
fewer contract claims and therefore fewer checks than a real schema; that is the
zero-false-positive discipline, not a bug. Review the draft, tighten the types and statuses your service
actually promises, then `reproit doctor`.

## Where requests go: target precedence

The service base URL is resolved in this order; the first present value wins:

1. `--target <url>` on `scan`/`fuzz` (a positional URL, `reproit scan http://...`, is the same)
2. the `REPROIT_BACKEND_URL` environment variable
3. `backend.target` in reproit.yaml (`reproit init <schema-url>` writes the schema URL's origin)
4. the schema's own `servers` entry

Inside a backend project a URL argument always means "the backend target". To run the
zero-config browser scan against a URL from inside a backend project, pass `--platform web`.

## Verdict tiers: black-box vs effect-grounded

Every request carries an `x-reproit-trace` header. If your service mounts the ReproIt backend
adapter (one line, see the `sdk/` READMEs: Express `app.use(...)`, FastAPI
`app.add_middleware(...)`, axum `.layer(ReproitLayer::new(...))`, Rack `use ...`, and so on),
it answers with its effect trail and verdicts are effect-grounded: what the handler actually
read and wrote, not just what the response claimed. Without the adapter, ReproIt still finds
real bugs but only from responses (status, shape, round-trip identity): the black-box tier.

`reproit doctor` reports which tier you are on and, when the adapter is absent, prints the
one-line mount for the framework it detected. Scan and fuzz state the tier once in their
summary. Neither tier ever guesses: a check that needs evidence it does not have abstains.

## What the run did not reach

A findings count is not a coverage report. `14 operations exercised, 0 confirmed
findings` looks like a clean sweep, but it reads exactly the same when every mutation was
rejected with a 400 and no contract was ever evaluated. So scan and fuzz report reach per
declared operation, and lead with the gap:

```
backend fuzz: 14 operation(s) exercised, 0 confirmed finding(s), 0 candidate(s)
  coverage: 1/4 declared operation(s) evaluated
    no success to evaluate (3):
      POST blockUser: 3 attempt(s), last 400 - {"error":"blocked_type must be one of: user, sponsor"}
      POST createPost: 3 attempt(s), last 400 - {"error":"blocked_type must be one of: user, sponsor"}
      GET listNearby: 3 attempt(s), all rate limited - {"error":"rate limited"}
```

Two questions the aggregate could not answer, now answered per operation:

- `reached`: was the request sent at all? A `never sent` operation names why (scan sends
  read-only GETs only; a request the schema could not build reports the build error).
- `evaluated`: did any attempt return a success the oracles could judge? An operation
  that only ever 4xx'd was reached but its contract was never tested.

The last non-2xx body is included because it usually names the input the declared schema
got wrong, which is the most common reason a mutation never lands. `--json` carries the
full table (`coverage`, plus `operationsEvaluated`) with per-status counts:
`attempts`, `ok`, `clientError`, `rateLimited`, `serverError`, `transportErrors`. The
terminal summary is silent when every declared operation was evaluated, because then the
aggregate is honest on its own.

## Proving a fix, and retracting a wrong contract

Every confirmed finding persists a replayable artifact under `.reproit/findings/<id>/`. Together
they are a regression suite that grows with every bug found:

```
reproit verify                  # replay them all; exit 1 if any still reproduces
reproit verify fnd_9ef028c142a0 # replay only that one
```

A **held** finding is machine-checkable proof the defect is gone: the replay re-sends the exact
recorded request, so a fix cannot be faked by not reaching the endpoint. A **reproducing** or
**inconclusive** finding fails closed.

There is a fourth answer. A finding is "this operation violated this contract", and the contract
lives in your schema, which you edit. When a first run against an existing API is what authored the
schema, the most common true outcome is that the *contract* was wrong, and the correct fix is to
withdraw the claim, not to change the product. Withdrawing it makes `scan` go clean immediately,
but the recorded finding stays true under the contract it was recorded against, so replaying it
could never go green.

So `verify` re-resolves each finding's operation against the schema you have now. If the operation
is gone, or if the response no longer violates the claim the schema currently makes, the finding is
reported as **retracted**:

```
verify: 0 held, 0 still reproducing, 0 inconclusive, 1 retracted
  fnd_9ef028c142a0 retracted on getNearby: the schema no longer makes the violated claim about getNearby
reproit verify --prune-retracted   # delete findings whose constraint no longer exists
```

Retracted does not block, because withdrawing a claim is an explicit schema edit that shows up in
review. It is never counted as held: nothing was proven about the implementation. Only an evaluable
non-reproduction under the current contract retracts, so a flaky or unreachable run cannot retract a
live bug, and a schema that cannot be read at all never retracts anything.

## Production capture replay

With the adapter in capture mode, a production failure ships its full event trail. One command
re-evaluates it locally, deterministically, with no live service:

```
reproit check capture.json      # exit 1 while it still reproduces, 0 once fixed
```

`reproit inspect capture.json` steps through the same payload; `reproit debug replay-capture`
is the low-level form of the check.
