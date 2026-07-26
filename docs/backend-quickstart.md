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

The Rust adapter is not on crates.io, which does not stop you using it: a git
dependency resolves and builds today.

```toml
[dependencies]
reproit-backend = { git = "https://github.com/ReproIt/reproit", features = ["axum"] }
```

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

## Keeping the schema honest

A hand-written schema is not checked by anything, and a wrong one is expensive in
a way that looks like success: a mistyped path 404s on every attempt while still
counting as an exercised operation, and a route missing from the schema is real
surface nothing will ever test. `doctor` compares the declared contract against
the routes it extracts from your source:

```
  warn    contract
          schema and source disagree (4 declared operation(s) matched)
    declared but not served by the source (1): these 404 at runtime
          GET /api/v1/user/{id}
    served by the source but not declared (2): add these so they are tested
          GET /api/v1/users/{id}
          GET /healthz
```

In a repo that holds more than one service, set `backend.source: <dir>` so the check reads only
that service's code. Without it, the check would compare this schema against a sibling service's
routes and confidently tell you to delete correct operations, so an undeclared multi-service root
abstains and names the services it found. `--learn` refuses there for the same reason: a schema
derived from three services describes none of them.

On a Rust service it also compares the declared request body against the handler's types, which
is where a schema costs the most: the route is right, so every attempt reaches the service and
every one is rejected, and the operation reads as exercised while evaluating nothing.

```
    declared body fields the handler disagrees with (3):
      POST /v1/blocks .blocked_type: declared open, but the handler accepts only [user, sponsor]
      POST /v1/blocks .blocked_id: the handler requires it, but the schema does not mark it required
      POST /v1/blocks .nite: the handler's body type has no `nite` field
```

Every family's sources are parsed: Rust by `syn`, and Python, Node, Ruby, PHP and Java by their
tree-sitter grammars. Go still has only its pattern reader. That matters for what an absence means: a file either
parses, or it does not and is COUNTED, and the report says so instead of treating a file it never
read as a file with nothing in it. Route and type extraction for the non-Rust families is still
pattern-based; what the grammar adds is knowing when those patterns were reading nothing.

```
    declared, but no route matched in source (1): NOT reliable: 1 source file(s) could not
    be parsed, so a route may simply never have been read. Fix those first
```

A Rust type carries no value range, so the check reads the source rather than only the signature.
`rating: i8` says nothing; `matches!(body.rating, -1 | 0 | 1)` two lines into the handler says
everything, and that is the constraint that actually rejects the request:

```
      POST /v1/rate .rating: declared 1..5, but the handler accepts only [-1, 0, 1]
                            (an explicit value guard in the handler)
```

Every supported family is read through the signal its own ecosystem already uses:

| | closed value set | range | optional |
|---|---|---|---|
| Rust | unit-only enum, `matches!(x, A \| B)`, `[A, B].contains(&x)` | `validate`/`garde` `range(...)`, `(A..=B).contains(&x)` | `Option<T>` |
| Python | `Literal["a", "b"]`, a `str, Enum` class | `Field(ge=, le=, gt=, lt=)` | `Optional[T]`, `\| None`, a default |
| Go | struct tag `oneof=a b` | struct tag `min=`/`max=`/`gte=`/`lte=` | pointer, `omitempty`, absent `required` |
| Node | `z.enum([...])`, a TS string union, `@IsIn([...])` | `.min()`/`.max()`, `@Min`/`@Max` | `.optional()`, `field?:`, `@IsOptional` |
| Ruby | `validates inclusion: { in: %w[a b] }` | `validates numericality: { greater_than_or_equal_to: }` | absent `presence: true` |
| PHP | Laravel `'in:a,b'` | Laravel `'min:1\|max:5'` | absent `required` |
| Java | an enum-typed field | `@Min`/`@Max` | absent `@NotNull`/`@NotBlank` |

Every report names its evidence, so a verdict can be checked against the line that produced it,
and the summary says how many request bodies were actually traced to a handler: an operation whose
handler could not be resolved was not compared, and a clean result does not speak for it.

It also abstains when the type itself is ambiguous. Every one of these languages namespaces types
by module while this reader keys them by bare name, so two modules declaring the same name would
otherwise silently overwrite each other, with the winner depending on directory walk order. A name
declared twice with different fields is dropped rather than resolved to a guess.

It abstains on anything whose accepted set has to be inferred: an enum variant carrying data, a
`matches!` arm with a guard expression, validation spread across several statements. Those are
real constraints it cannot see, and reporting them would be the same overclaiming the schema is
guilty of. 

For paths it compares method and path template, never types: the extractor sees
routes, not handler signatures, and claiming a type mismatch it cannot observe
would be the same overclaiming the schema is guilty of. A path parameter named
differently in each (`{id}` vs `{user_id}`) is the same route and is not drift.
When the comparison cannot run at all (unrecognized framework, unreadable
source, a GraphQL or protobuf schema with no URL routes) it says "not checked"
rather than reporting a pass.

## Resetting state between runs

Fuzzing a stateful service without a reset means run N inherits whatever run N-1
left behind, so findings stop being independently reproducible. Declare it:

```yaml
backend:
  reset:
    steps:
      - kind: http
        method: POST
        url: http://localhost:8080/test/reset
        required: true
      - kind: command
        run: ./scripts/seed-fixtures.sh
        required: true
```

Steps run in order before the sweep and before every fuzz round. Best-effort
unless `required`, which fails the run closed: a reset that silently did not
happen is worse than none, because the run still presents its findings as
reproducible from a clean state. The contract is recorded into each finding
artifact, so a replay re-establishes the same preconditions. This replaces
`REPROIT_BACKEND_RESET_URL`, which still works as the single-URL legacy form.

## Operations that need a resource to exist

An operation on `/posts/{id}/going` needs a real post. The run creates one, so the request lands,
but CONFIRMING the violation used to be the problem: a non-idempotent POST cannot be re-sent to
check (the resource is already in the acted-on state), so confirmation needed a whole-service reset
and, without `REPROIT_BACKEND_RESET_URL`, a real violation was filed as an unconfirmed candidate
that never blocked anything.

It does not need a reset. The finding records the create as a setup step plus a binding, so the
sequence replays against a FRESH resource: create, take the id that create returns, act.

```json
"setup":   [{ "request": { "operation": "createPost" } }],
"bindings": [{ "sourceStep": 0, "sourceOutputPath": "id", "inputPath": "path.post_id" }]
```

That makes the artifact self-contained: it still reproduces after a restart that dropped every row
the original run made, and `verify` flips it to held the moment the handler is fixed.

The link is drawn from STRUCTURE, never names: a create is used only for an operation whose path
extends the created collection by exactly one parameter (`POST /posts` for `/posts/{id}/going`).
There is no singularisation and no matching a field because it sounds like an identity. A path with
a second unsourced parameter has no established precondition, so it abstains rather than inventing
one and reporting a bug about a request nobody made.

## Adopting ReproIt on a repo that already has bugs

The gate blocks on new-or-regressed findings, which needs a baseline to compare against. On the
very first gated run there is none, so findings already in the tree would be silently recorded as
known and never block again: adopt ReproIt on a repo with a live reproducible bug and CI is
permanently green on exactly that bug. So the first run fails instead:

```
no baseline yet, and 3 finding(s) already reproduce. These are pre-existing, not introduced
by this change, so the gate cannot call them clean or silently adopt them.
  Fix them, or run `reproit check --update-baseline` to adopt them as known.
```

Adopting is a fine answer, it just has to be a decision. After `--update-baseline` those findings
stop blocking and any new one still does.

## Living with a known finding

`check --update-baseline` accepts everything currently reproducing, which
silently accepts anything else present. To accept exactly one:

```
reproit accept fnd_445ab4e5432f --reason "id lands in v2, tracked in HEY-412" --until 2026-12-31
reproit accept fnd_445ab4e5432f --remove
```

A reason is required. The accept names one finding's fingerprint, so it can
never cover a finding nobody looked at, and a passing gate still prints what it
is carrying. Past `--until` the finding blocks again and the gate says the
acceptance expired, so silence lapses loudly rather than becoming permanent. An
accept whose finding stopped reproducing is reported as stale but does not fail
the build: unlike a dependency allowlist entry, it can only ever silence the one
fingerprint it names.

## Gating a repo with several services

```
reproit check --service api/reproit.yaml --service worker/reproit.yaml
```

One command, one exit code, a per-service line. Fails if any service fails, and
a service whose gate could not run at all counts as a failure rather than being
skipped. Each service resolves its own target and owns its own `.reproit/` store.

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
