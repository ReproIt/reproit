# Bare `reproit init` ground truth (Phase 0)


> Vocabulary note (2026-07-30, revised 2026-08-02): this file is a verbatim
> record of runs made before the CLI vocabulary purge. The commands it quotes
> are spelled the same way today (`reproit scan`, `reproit fuzz`,
> `reproit surface`); they spent three days under a `reproit internal` prefix
> that has since been removed. `reproit find` is the discovery verb a human
> uses. The recorded outputs are unchanged because rewriting a record would
> falsify it.

Date: 2026-07-30. Binary: `target/release/reproit` built from main at 114c2b9.
Method: three throwaway fixtures in a scratch directory, zero CLI code changes, every run
non-interactive (`< /dev/null`). No prompt ever appeared on any path; init is fully
non-interactive today, so interactive and piped behavior are identical.

## Fixture A: stock Express app

Shape: real `package.json` (express ^4.19.2, `start: node server.js`, npm installed),
`server.js` with GET /items, GET /items/:id, POST /items, GET /search?q=. Planted bug:
POST /items without `name` throws (`name.trim()` on undefined), Express returns HTTP 500
with an HTML stack trace. Verified live via curl before any reproit run.

### What init did

Exit 0, no prompts, no flags needed. Verbatim stdout:

```
  derived 4 operations on 3 paths from express source (1 files scanned)
  no running target (pass --target <url> or set REPROIT_BACKEND_URL to also record
  observed responses)
  write .../openapi.yaml (derived draft)
  write .../reproit.yaml
  write .../.reproit/.gitignore

  reproit initialized from a DERIVED DRAFT schema (3 routes from source, 0 enriched live).
  1. review openapi.yaml: it is a draft, not your service's contract
  2. tighten param/body/response types for the routes you rely on
  3. reproit doctor         # schema, target, and adapter tier
  4. reproit scan           # read-only contract checks
     reproit fuzz           # stateful interaction bugs
```

(The summary line says "3 routes" while the first line says "4 operations on 3 paths";
it is 4 operations on 3 path templates plus /search, i.e. the wording is off by its own
counting scheme.)

### Detected vs missed

- Detected: express framework, all 4 routes, path param `{id}` (typed `string`), the POST
  request body (as bare `type: object`).
- Missed: the `q` query param on /search (no `parameters` entry at all); every body field
  (`name`, `price` absent, so "missing required field" is unexpressable); all response
  shapes and status codes (no `responses` blocks anywhere); the `start` script is never
  used. Init printed "0 enriched live" and moved on rather than booting the app it could
  plainly see how to boot.

### Scaffold contents

- `openapi.yaml`: honest DRAFT header, `x-reproit-derived: true`, loose but truthful.
- `reproit.yaml`: 5 lines, `backend.enabled` plus the schema list. No target, no start
  command, nothing about how to run the app.
- `.reproit/.gitignore`: fine.

### The natural next commands

- `reproit doctor`: exit 1, "doctor: required checks failed". Passing checks are green;
  the hard stop is `MISSING target / no target: the schema has no servers entry` with the
  fix line naming `--target`, `REPROIT_BACKEND_URL`, or `backend.target`. Also demands
  `reproit login` for a cloud app even for a purely local first run.
- Bare `reproit`: prints help. Top-level help advertises `find`, not the `scan`/`fuzz`
  that init's next-steps name (both exist; the two surfaces disagree on the on-ramp).
- `reproit scan` (no flag): `Error: the schema has no absolute server URL; set
  REPROIT_BACKEND_URL to the disposable service`. Same for `fuzz` and `find`.
- `reproit scan --target http://localhost:3555` (app started by hand): 0 findings.
  `POST post_items: scan executes read-only GET operations only`, and
  `GET get_items_id: 1 attempt(s), last 404` because it invents an id, gets 404, and has
  no success to evaluate.
- `reproit fuzz --target ...`: the planted bug IS reached, three times:
  `POST post_items: 3 attempt(s), last 500 - <!DOCTYPE html> ... TypeError: Cannot read
  properties of undefined (reading 'trim')`. But the run reports
  `0 confirmed finding(s), 3 candidate(s)`. The report JSON shows why: each candidate says
  `contract-valid request returned HTTP 500` and
  `stateful or non-idempotent confirmation requires REPROIT_BACKEND_RESET_URL`.
  Setting REPROIT_BACKEND_RESET_URL to a plausible URL did not change the outcome.
- `REPROIT_BACKEND_URL=... reproit find`: runs scan then fuzz, same end state:
  bug hit, zero confirmed, buried in a "no success to evaluate" coverage note.
- `reproit list`: `Error: parsing .../reproit.yaml / missing field 'app'`. The config
  that init itself wrote is unreadable by `list`; the backend scaffold and the
  guards/candidates surface disagree on the schema.
- Re-running `reproit init`: `Error: reproit.yaml already exists (use --force to
  overwrite)`, exit 1.

### What a first-time user must already know (fixture A)

1. That the target URL goes in via `--target`/`REPROIT_BACKEND_URL`/`backend.target`,
   and that they must boot their own app first; init read the `start` script and ignored it.
2. That `scan` is GET-only, so the interesting bug class needs `fuzz`.
3. That a hit 500 is only a "candidate" until a reset URL exists, what a reset URL is,
   and that setting one still may not confirm. Nothing prints "your bug is right here";
   the 500 lives in a coverage footnote and a JSON file under `.reproit/runs/`.
4. That `reproit list` cannot read this project's config at all.

## Fixture B: raw node `http.createServer`, no framework

Shape: single `server.js`, 2 GET routes (/health, /time), no dependencies.

- With no `package.json` (server.js only): hard fail, exit 1, zero files written:
  `Error: no supported UI project or backend framework recognised here. A manifest may
  well be present: what is missing is a framework this can read (axum, actix-web, rocket,
  warp, express, fastify, koa, hapi, NestJS, FastAPI, Flask, Django, gin, echo, fiber,
  chi, gorilla/mux, net/http, Rails, Sinatra, Laravel, Spring, ASP.NET). Pass --platform
  to override`. Note the list claims `net/http` (Go) support while node's http module,
  arguably the most common raw server in the wild, is unrecognisable.
- With a minimal `package.json` (name + `start` script, no deps): exit 0, but init
  silently classifies the repo as a WEB UI project. It writes the browser-driving
  `reproit.yaml` (`app.platform: web`, Playwright runner, journeys/devices/evidence
  blocks) with `url: "http://localhost:3000"` as a guess and
  `webRunnerDir: "../reproit/runners/web"`, a relative path into a sibling checkout that
  does not exist on a user's machine. No route derivation, no schema, and the next-steps
  text tells the user to edit `app.url` by hand. For a backend-only repo this scaffold is
  the wrong workflow with plausible-looking contents.

## Fixture C: empty git repo

`git init` and nothing else. Bare `reproit init`: identical hard fail to fixture B's
no-manifest case, exit 1, zero files written, same framework-list error ending in
"Pass --platform to override". No scaffold, no partial output, no statement of what init
looked for beyond the framework list.

## Prioritized gaps, mapped to Phase 1

1. Init hard-fails on unrecognised repos (fixtures B and C): exit 1, zero files, an error
   whose only action is a flag. Maps to Phase 1.1 (detection-miss degrades instead of
   bailing: scaffold with an empty route list and a concrete next-input line) and 1.2
   (flags are overrides; `--platform` must never be the only way forward).
2. Init never boots the app despite reading the `start` script, so 0 routes are enriched,
   response shapes are absent, and the whole downstream chain (doctor target check, scan
   404 on a guessed id, fuzz body synthesis) is starved. Maps directly to Phase 1.3
   (auto-boot-and-probe, bounded). This one gap causes most of fixture A's friction.
3. The planted 500 is reached but never surfaced as a finding: "candidate" plus a
   reset-URL demand, reported inside a coverage footnote. Until confirmation works
   unflagged (or candidates are printed as first-class next-steps), the demo answer to
   "does it find the bug" is no. Phase 1.3 gives fuzz a reset-capable, init-booted target;
   the phrasing itself is Phase 4 material but blocks the demo today.
4. The target URL is demanded three different ways (`--target`, env var, `backend.target`)
   by four commands, and doctor exits 1 over it. With 1.3 in place init should record the
   observed URL, making the derived default plus printed note of Phase 1.2 possible.
5. Scaffold inconsistencies that Phase 1.4's smoke tests should assert on: `reproit list`
   cannot parse the backend `reproit.yaml` init writes (missing `app`); init's next-steps
   name `scan`/`fuzz` while top-level help sells `find`; the summary line miscounts
   ("3 routes" vs "4 operations"); the misdetected web scaffold in fixture B ships a
   `webRunnerDir` path that only exists in this monorepo.
6. Fixture B's package.json path shows detection-miss can also mean silent
   MISclassification, not just a bail. The Phase 1.1 degrade path needs to state what it
   assumed ("no backend framework read; treating as web UI at <url>, override with
   --platform backend") instead of writing a confident but wrong scaffold.

## Phase 1 result (2026-07-30)

Re-ran all three fixtures with the rebuilt release binary after the Phase 1 changes.
Rule verified: bare `reproit init`, zero flags, always exit 0, always a scaffold, and
`reproit list` parses every config init writes.

### Fixture A: stock Express app

Exit 0, no flags. Init now boots the app itself from the `start` script, enriches the
parameterless GET routes live, and tears the process down (no leftover node process).
Verbatim stdout:

```
  derived 4 operations on 3 paths from express source (1 files scanned)
  booting the package.json `start` script on port 54429 to observe responses (torn down
  after init; override with --target <url>)
  probed 2 of 2 parameterless GET routes at http://127.0.0.1:54429 (booted from the
  package.json `start` script): 2 answered, no adapter detected (black-box observations)
  write .../openapi.yaml (derived draft)
  write .../reproit.yaml
  write .../.reproit/.gitignore

  reproit initialized from a DERIVED DRAFT schema (4 operations on 3 paths from source,
  2 enriched live).
  1. review openapi.yaml: it is a draft, not your service's contract
  2. tighten param/body/response types for the routes you rely on
  3. reproit doctor         # schema, target, and adapter tier
  4. reproit find           # find bugs (surface scan, then deep fuzz)
```

- The summary now counts in the derivation line's own scheme (operations on paths), and
  next steps name `find`, matching the top-level help.
- `/items` and `/search` carry `# observed live during init: HTTP 200` responses blocks
  with sampled body shapes in openapi.yaml.
- `reproit list`: exit 0 ("no saved guards. Find failures with `reproit find` ...").
  The "missing field 'app'" failure on init's own config is gone.
- Already-running servers are only trusted after a two-signal match: a derived route must
  answer non-404 AND an unserved nonce path must answer 404. Caught in the field during
  this validation: DynamoDB-local on port 8000 answers every path with 400, and a bare
  "not 404" check adopted it as the target on the first attempt. The nonce check rejects
  it and init boots the repo's own start script instead.
- Re-running `reproit init`: exit 0, "reproit.yaml already exists; leaving it untouched
  (use --force to regenerate)". No longer an error.

### Fixture B: raw node `http.createServer`

- Without package.json: exit 0 (was exit 1 with the framework-list error). Scaffolds the
  backend shape with an empty draft, states the assumption, names the next input:

```
  no UI project or backend framework recognised here; assuming a backend service
  (override with --platform flutter|web|rn|android)
  write .../openapi.yaml
  write .../reproit.yaml
  write .../.reproit/.gitignore

  reproit initialized.
  1. add the routes your service serves to openapi.yaml
     (or rerun `reproit init` from the service's source root, or
     `reproit init <schema url or file>` to import a real schema)
  2. reproit find   # find bugs once a target is running
```

- With a bare package.json: identical degrade, exit 0. The silent web misclassification
  is gone: no `platform: web`, no guessed url, no `../reproit/runners/web` path. A
  package.json now detects as web only on real UI markers (react/vue/svelte/angular/
  next/vite/astro/solid-js dependency, or an index.html); `init --platform web` writes
  the managed provisioned runner dir, never the monorepo-relative path.
- `reproit list` parses both scaffolds, exit 0.

### Fixture C: empty git repo

Exit 0 (was exit 1, zero files). Same degrade output as fixture B's no-manifest case:
empty draft schema, backend reproit.yaml, gitignore, assumption stated, next input named.
`reproit list` exit 0. Re-run exit 0 with the already-exists notice.

### Covered by CI

`crates/reproit/tests/init_smoke.rs` pins all of the above: the three fixtures exit 0
with a well-formed scaffold `list` can read, the express fixture derives 4 operations on
3 paths (the `/search` query-param route included) and auto-enriches with no flags when
node is present, the bare-package.json repo never degrades to web, and re-running init
stays exit 0. `zero_derived_routes_still_scaffolds_an_empty_draft` (backend_learn) pins
the in-process degrade path.

## Phase 2 dynamic result (2026-07-30)

Rebuilt fixture A (stock Express, same shape and planted bug as Phase 0) in a scratch
directory and re-ran the rebuilt release binary. The dynamic-language track turns the
Phase 1 "parameterless GET probe" into one synthesized bounded request per derived
operation, recorded as an observed baseline contract with first-class provenance.

### Fixture A, bare `reproit init` (no server running: init boots and tears down)

Exit 0, no flags. Verbatim stdout:

```
  derived 4 operations on 3 paths from express source (1 files scanned)
  booting the package.json `start` script on port 64345 to observe responses (torn down after init; override with --target <url>)
  probed 4 of 4 derived operations at http://127.0.0.1:64345 (booted from the package.json `start` script): 4 answered, no adapter detected (black-box observations)
  write .../openapi.yaml (derived draft)
  write .../reproit.yaml
  write .../.reproit/.gitignore

  reproit initialized from a DERIVED DRAFT schema (4 operations on 3 paths from source, 4 enriched live).
  1. review openapi.yaml: it is a draft, not your service's contract
  2. tighten param/body/response types for the routes you rely on
  3. reproit doctor         # schema, target, and adapter tier
  4. reproit find           # find bugs (surface scan, then deep fuzz)
```

All 4 operations now carry observed baseline contracts (Phase 1 recorded 2):

- `GET /items` and `GET /search`: `200` responses with sampled body shapes, as before.
- `GET /items/{id}`: probed with a synthesized param, recorded in the draft:
  `# observed live during init: HTTP 200; path params synthesized: id=1`.
- `POST /items`: the inline handler's `const { name, price } = req.body` is parsed by
  the new destructure reader, so init synthesized one minimal body and observed the
  contract: `# observed live during init: HTTP 201; request body synthesized from
  parsed source fields: {"name":"reproit","price":"reproit"}`, with the `201` response
  shape sampled. The parsed field names also land in the requestBody schema (untyped:
  a name read from a destructure carries no type claim).
- Every operation is marked `x-reproit-provenance: inferred`; every observed response
  block is marked `x-reproit-provenance: observed`. The header states the one-way rule:
  only the user may mark an entry `confirmed`, and no code path writes that value
  (pinned by test).
- No leftover node process after init on any run.

### The planted 500 violates the observed contract

Server booted by hand on 3555, `REPROIT_BACKEND_URL=... reproit fuzz`, verbatim:

```
lifecycle: 0 new, 0 regressed, 0 persisting, 0 fixed
backend fuzz: 12 operation(s) exercised, 0 confirmed finding(s), 3 candidate(s), 0 execution error(s)
  tier: black-box (no adapter; response-level checks only)
  coverage: 2/4 declared operation(s) evaluated
    no success to evaluate (2):
      GET get_items_id: 3 attempt(s), last 404 - {"error":"not found"}
      POST post_items: 3 attempt(s), last 500 - <!DOCTYPE html> ... TypeError: Cannot read properties of null (reading &#39;trim&#39;) ...
```

Every candidate in the report carries `"reason": "contract-valid request returned
HTTP 500"`: with `201` recorded as the observed success status, the missing-field 500
is a violation of the observed/derived contract, which is the Phase 2 acceptance bar.
(Confirmation still demands a reset URL; surfacing candidates as first-class findings
remains the known Phase 3/4 gap recorded in the Phase 0 section.)

### Safety invariant: mutating probes only against an init-booted server

With the fixture already running on the conventional port 3000 (a user-owned server
that passes the two-signal repo match), bare init probes read-only and holds the POST
back entirely, saying so in both the output and the draft. Verbatim:

```
  derived 4 operations on 3 paths from express source (1 files scanned)
  found a server on port 3000 answering /items (it matches the derived routes); assuming it is this service. Override with --target <url>
  probed 3 of 4 derived operations at http://127.0.0.1:3000 (already running, matched a derived route): 3 answered, no adapter detected (black-box observations); 1 skipped (POST /items: init never sends mutating requests to a server it did not boot itself)
```

The draft carries `# not probed during init: init never sends mutating requests to a
server it did not boot itself` on the POST, and the server's item list was unchanged
after init (verified by count). A POST whose body fields no reader can parse is skipped
the same way, with `request body fields not parseable from source, so no honest request
exists` (pinned by the smoke test's raw-node fixture).

### Scope landed vs deferred

- Landed: probe planning (`probe_plan.rs`, pure and colocated-tested), any-method
  bounded sending in `enrich.rs`, provenance-marked emission in `emit.rs`, and the
  Node inline `req.body` destructure reader (`node_body.rs`). Boot/teardown machinery
  from Phase 1 is reused unchanged.
- Python/Ruby/PHP: the planner and emitter are language-agnostic (they work off the
  shared `Derived` shape, which python_ast already fills with pydantic body fields), so
  safe-method observed contracts work today against a running or `--target` server.
  Mutating observation requires an init-booted server, and Phase 1's boot machinery
  only knows package.json start scripts; teaching it Procfiles/uvicorn is the next
  increment, deliberately not smuggled into this one.
- Query-param synthesis: no reader parses query parameter NAMES yet (the `q` gap from
  Phase 0 stands), so there is nothing honest to synthesize; `/search` is probed bare
  and observed as such. That is a reader gap, not a prober gap.

## Phase 2 typed result (2026-07-30)

Typed-language track: `reproit init` now derives the RESPONSE contract from handler
return types and serializer structs, for Go and Rust. Method: one fixture app per
language in the session scratchpad, real release binary built from this branch, server
NOT running during init (so every responses entry is provenance "inferred", nothing
observed), then the planted bug exercised live through bare `reproit scan --target`.
Every run below is verbatim.

Order of derivation inside each language, per the plan: status codes, then top-level
body shape, then field types, then optionality. Everything stops at what the type
system states; each abstention is listed under "left underived" with its reason.

### Go fixture (net/http, Go 1.22 method patterns)

Shape: `go.mod` + one `main.go`. Structs `Item` (id/name/price json-tagged,
`tags []string json:"tags,omitempty"`, one unexported `cost int`) and `Stats`.
Handlers: `listItems` encodes the package-level `var items []Item`; `getStats` encodes
a `Stats{...}` literal; `createItem` decodes into `var body Item`, answers
`http.Error(w, "bad json", 400)` or `WriteHeader(http.StatusCreated)` + encode.
Planted bug: `items` stays nil until the first create, and encoding/json serializes a
nil slice as `null`, not `[]`, so GET /items on a fresh store answers `null` where the
type states a JSON array. Verified live via curl before any reproit run.

Bare `reproit init` (no flags, no server): exit 0,
`derived 3 operations on 2 paths from net/http source (1 files scanned)`, 0 enriched
live. The draft's derived entries, verbatim from openapi.yaml:

```
  "/items":
    get:
      operationId: get_items
      x-reproit-provenance: inferred
      responses:
        # inferred from `listItems` return types in source
        "200":
          description: inferred from the handler's return types; verify before relying on it
          x-reproit-provenance: inferred
          content:
            application/json:
              schema:
                type: array
                items:
                  type: object
                  properties:
                    "id":
                      type: string
                    "name":
                      type: string
                    "price":
                      type: number
                    "tags":
                      type: array
                      items:
                        type: string
                  required:
                    - "id"
                    - "name"
                    - "price"
```

POST /items carries `"201"` (typed Item body, from the WriteHeader/Encode pair) and
`"400"` (status only, from `http.Error`); GET /stats carries `"200"` with
`total: integer` + `currency: string`, both required. Every entry carries both the
`# inferred from ... return types in source` comment and the machine-readable
`x-reproit-provenance: inferred` marker (the dynamic track's one-way vocabulary);
none claims observation.

What was derived (Go): statuses from `c.JSON(status, v)`-family calls, fiber's chained
`Status(n).JSON(v)`, `WriteHeader` paired with the next `Encode` (an unpaired Encode is
net/http's implicit 200), and `http.Error`'s third argument; named constants resolve
through one table (`http.StatusCreated` -> 201). Body shapes through typed locals,
composite literals, and file-level `var` declarations, down to the serializer struct's
json tags. Field types from Go types (string/bool/ints/floats/slices/maps, `time.Time`
as string). Optionality: `omitempty` is not required; an untagged exported field
serializes under its Go name and is required; unexported fields never serialize.

What was honestly left underived (Go), each pinned by a unit test:
- A status behind a computed expression (`c.JSON(statusFor(x), ...)`) states nothing.
- A body behind an untyped local (`resp := fetch()`) or a `gin.H`/map literal states a
  status but no shape (`gin.H` is `type: object` with no properties).
- A bare pointer field (`*string` without omitempty) claims NO type: nil serializes as
  null, and pointers are how Go states an intentionally absent value. With `omitempty`
  the nil case is omitted, so the pointee's type is claimed.
- An embedded (promoted) field makes the whole serializer abstain, same rule as serde
  flatten; two conflicting declarations of one struct name resolve to neither.
- A slice DOES claim `array` although nil serializes as null: that is what the type
  means as a contract, and the nil case is exactly the divergence worth catching.

End to end against the planted bug (fresh store, server booted, bare scan):

```
lifecycle: 1 new, 0 regressed, 0 persisting, 0 fixed
backend scan: 2 operation(s) exercised, 1 confirmed finding(s), 0 candidate(s), 0 execution error(s)
  tier: black-box (no adapter; response-level checks only)
  coverage: 2/3 declared operation(s) evaluated
    never sent (1):
      POST post_items: scan executes read-only GET operations only
  fnd_19746c9916d5  get_items: $output must be an array
```

Controls: with the responses blocks stripped from the same schema, the identical scan
reports 0 findings (the bug is invisible without the inferred contract); after one POST
populates the store, the full-schema scan reports 0 findings and `1 fixed` (the typed
happy path passes, so the finding is the bug, not the contract).

### Rust fixture (axum 0.8)

Shape: Cargo.toml + one `src/main.rs`. `Item` derives Serialize/Deserialize;
`Stats { total: u32, currency: String }` derives Serialize. Handlers:
`list_items() -> Json<Vec<Item>>`, `get_stats() -> Result<Json<Stats>, StatusCode>`,
`create_item(Json(body)) -> (StatusCode, Json<Item>)` returning
`(StatusCode::CREATED, ...)`. Planted bug: a legacy guard answers
`Err(StatusCode::NO_CONTENT)` when the store is empty, where the Ok arm of the return
type states a 200 carrying Stats; an empty store is a valid state and a bodiless 204
breaks every client parsing the promised JSON.

Bare `reproit init` (no flags, no server): exit 0,
`derived 3 operations on 2 paths from axum source (1 files scanned)`, 0 enriched live.
GET /items gets `"200"` as `type: array` of the typed Item object (id/name/price all
required); POST /items gets `"201"` (from the `StatusCode::CREATED` literal paired with
the tuple return's `Json<Item>`); GET /stats gets `"200"` with `total: integer` +
`currency: string` required. Provenance comment plus
`x-reproit-provenance: inferred` on every entry, e.g.
`# inferred from get_stats return types in source`.

What was derived (Rust): a `Json<T>` return states a 200 carrying T (through `Result`'s
Ok arm); a `(StatusCode, Json<T>)` return carries T at each `StatusCode::X` literal the
body names; actix `HttpResponse::Created().json(v)` states both in one expression, with
the value typed through struct-literal locals. Serializer fields read serde exactly:
`rename_all`/`rename`, `skip`/`skip_serializing` excluded, `skip_serializing_if` as
omitempty (optional, inner type claimed for Option), Vec/arrays/maps/Uuid mapped, and
required = not-skippable.

What was honestly left underived (Rust), each pinned by a unit test:
- A tuple return naming no status literal abstains entirely (a computed status is not
  a stated one).
- A `Result`'s error arm states no status: `Err(StatusCode::NO_CONTENT)` in the fixture
  is deliberately NOT claimed as a declared response, which is precisely why the scan
  below can flag it.
- A bare `Option<T>` field is required (always present) but claims no type, because
  None serializes as null; a `serialize_with`/`with`/`serde_as` field claims no type
  because a custom serializer rewrites the value.
- `#[serde(flatten)]` makes the serializer abstain; a bare type name declared
  differently in two modules resolves to neither.

End to end against the planted bug (fresh store, bare scan; curl first confirmed
`stats status: 204`):

```
lifecycle: 1 new, 0 regressed, 0 persisting, 0 fixed
backend scan: 2 operation(s) exercised, 1 confirmed finding(s), 0 candidate(s), 0 execution error(s)
  tier: black-box (no adapter; response-level checks only)
  coverage: 2/3 declared operation(s) evaluated
    never sent (1):
      POST post_items: scan executes read-only GET operations only
  fnd_7235664f25f4  get_stats: operation reported successful status 204 outside its declared success statuses [200]
```

Controls: responses stripped -> 0 findings, exit 0 (without the inferred success set
the 204 is silent, exactly Phase 0's "no success to evaluate" dead end); store
populated -> 0 findings, `1 fixed`.

One attribution lesson from getting here: the first two Rust bug candidates were bad
fixtures and were rejected after live runs. A 500 on a valid GET is confirmed by the
generic server-error oracle WITH OR WITHOUT the inferred contract (verified both ways),
so it proves nothing about this track; a 404 on a valid GET is deliberately silent in
the evaluator (a 4xx can be a correct rejection, and flagging it would be a
false-positive engine). What the inferred contract uniquely arms on the read-only path
is the response-status oracle (a 2xx outside the declared success set) and the
response-shape oracle (a 200 body violating the typed schema, the Go fixture's class).

### Deliberately not done in this pass

- Java and C#: untouched. Their readers (`java_ast.rs`, `dotnet_ast.rs`) collect
  request-side facts only; the response pattern did not generalize cleanly enough to
  land them honestly in this pass. Spring controller returns (`ResponseEntity<T>`,
  `@ResponseStatus`) and ASP.NET (`ActionResult<T>`, `Ok(...)`/`CreatedAtAction`) each
  need their own status/body pairing rules; half-reading them would emit contracts the
  services do not state.
- Dynamic languages: untouched here; their observe-not-infer track landed separately
  (see the Phase 2 dynamic result above). Where both tracks speak about one status,
  the emitter keeps the typed schema and the comment records both provenances.
- Provenance hardening (plan 2.3) came with the dynamic track's one-way Provenance
  vocabulary; the inferred entries here reuse it, and nothing upgrades a mark.

## Phase 3 result (2026-07-30)

Fixture A rebuilt from scratch in scratch space (same shape as Phase 0: express-style
routes, `start: node server.js`, planted 500 on POST /items without `name`; the 500 is
an HTML stack trace served as text/html, as real Express does). Release binary built
from this branch (on top of the Phase 2 dynamic/typed work), zero flags, no env vars.
The loop closes in exactly three user commands.

### `reproit init` (command 1 of 3)

Exit 0. Verbatim stdout (paths elided):

```
  derived 4 operations on 3 paths from express source (1 files scanned)
  booting the package.json `start` script on port 64089 to observe responses (torn
  down when this run completes; override with --target <url>)
  probed 3 of 4 derived operations at http://127.0.0.1:64089 (booted from the
  package.json `start` script): 3 answered, no adapter detected (black-box
  observations); 1 skipped (POST /items: request body fields not parseable from
  source, so no honest request exists)
  write .../openapi.yaml (derived draft)
  write .../reproit.yaml
  write .../.reproit/.gitignore

  reproit initialized from a DERIVED DRAFT schema (4 operations on 3 paths from
  source, 3 enriched live).
  1. review openapi.yaml: it is a draft, not your service's contract
  2. tighten param/body/response types for the routes you rely on
  3. reproit doctor         # schema, target, and adapter tier
  4. reproit find           # find bugs (surface scan, then deep fuzz)
```

(This fixture's POST handler reads `body.name` without a destructure, so the Phase 2
body reader abstains and init skips the mutating probe with the reason stated; the
Phase 2 result above shows the destructured variant probing 4 of 4.)

### `reproit find` (command 2 of 3)

Exit 1 (a bug was found). No target flag, no REPROIT_BACKEND_URL, no reset URL. find
inspected the scaffold (3 safe operations, 1 mutation), booted the service itself, ran
scan for the safe routes and fuzz for the mutation, and CONFIRMED the planted 500 via
boot-restart reset. Verbatim stdout:

```
  booting the package.json `start` script on port 64128 to observe responses (torn
  down when this run completes; override with --target <url>)
find: fast surface pass
lifecycle: 0 new, 0 regressed, 0 persisting, 0 fixed
backend scan: 3 operation(s) exercised, 0 confirmed finding(s), 0 candidate(s), 0
execution error(s)
  tier: black-box (no adapter; response-level checks only)
  coverage: 3/4 declared operation(s) evaluated
    never sent (1):
      POST post_items: scan executes read-only GET operations only
find: deep interaction pass
lifecycle: 1 new, 0 regressed, 0 persisting, 0 fixed
backend fuzz: 12 operation(s) exercised, 1 confirmed finding(s), 0 candidate(s), 0
execution error(s)
  tier: black-box (no adapter; response-level checks only)
  coverage: 3/4 declared operation(s) evaluated
    no success to evaluate (1):
      POST post_items: 3 attempt(s), last 500 - <!DOCTYPE html><pre>TypeError: Cannot
      read properties of undefined (reading 'trim') at IncomingMessage.<anonymous> ...
  fnd_9b38629236ad  post_items: contract-valid request returned HTTP 500
EXIT=1
```

The Phase 0 dead-end (`0 confirmed finding(s), 3 candidate(s)` plus "stateful or
non-idempotent confirmation requires REPROIT_BACKEND_RESET_URL") is gone: when the run
booted the process itself, a full process restart IS the reset, so confirmation
replays from clean state without any reset URL. The finding prints first-class with
its id, not as a coverage footnote.

### `reproit keep` (command 3 of 3)

Exit 0. Verbatim stdout:

```
  kept rep_9b38629236ad (quarantined)
  verify: reproit rep_9b38629236ad
  write .github/workflows/reproit.yml (CI runs `reproit check`: 0 pass, 1 regression,
  2 flaky, 3 stale)
```

Committed-ready state after keep:

```
.github/workflows/reproit.yml
.reproit/repros/9b38629236ad/backend.json
.reproit/repros/9b38629236ad/fuzz.md
.reproit/repros/9b38629236ad/meta.json
.reproit/repros/9b38629236ad/replay.json
```

The workflow checks out, npm-installs (the fixture has a package.json), installs the
released binary via install.sh, and runs `reproit check`; the four-way exit-code
mapping is stated in the file. An existing `reproit.yml` with a `jobs:` map gets the
job appended instead; one that already runs `reproit check` is left untouched.

### `reproit check` against broken vs fixed

Broken app (the planted 500 still present), exit 1:

```
  guard rep_9b38629236ad: REPRODUCED (the bug is back)
  guards: 1 kept, 1 failing
```

Fixed app (missing `name` now rejected with a 400), exit 0:

```
  guard rep_9b38629236ad: held (does not reproduce)
  guards: 1 kept, 0 failing
```

check boots the service the same way find does, replays the kept guard with its
recorded requests rebased onto the current target origin (a guard found on one
ephemeral booted port must replay on the port this run booted), and tears the process
down; `pgrep` confirms no leftover node process after every command.

### Defects fixed on the way (found by this validation)

- The kept guard replayed against the DISCOVERING run's absolute URL and died with
  `tcp connect error` on the dead port. Recorded requests now rebase onto
  `REPROIT_BACKEND_URL` (the same precedence every live run honors).
- The fix (500 -> 400 rejection) initially verified as `could not verify ... fails
  closed`: the generic proof-of-fix demanded a 2xx. For a server-error finding a
  deliberate 4xx rejection of the recorded input IS the fix, so the replay verdict
  accepts it (auth/rate statuses 401/403/407/429 stay inconclusive).

### Covered by CI

`crates/reproit/tests/loop_smoke.rs` pins the whole loop on the rebuilt fixture
(self-gated on node): three commands, the 500 confirmed (not a candidate), guard +
workflow written, check exit 1 on the broken app and exit 0 on the fixed one.
Engine selection, the oracle expert door, CI wiring idempotence, and URL rebasing are
pinned by colocated tests (find_command, repro::keep, backend replay_command).


## Phase 4 result (2026-07-30)

The honesty invariant, encoded: `reproit init`, `reproit doctor`, and the find/check
surfaces never state a bare absence. Every gap now reads as (what is known, then the
exact next input, with the command or file that provides it). Rebuilt the three
fixtures in scratch space plus a two-service monorepo, re-ran the rebuilt release
binary, and captured every changed message verbatim, before against after.

### Fixture A (stock Express): doctor no longer fails over the target

Init unchanged in shape (exit 0, boots, probes 4 of 4, `find` next step). Doctor,
which in Phase 0 was the first dead end, now exits 0. Before (Phase 0):

```
  MISSING target
          no target: the schema has no servers entry
          fix: pass `--target <url>` to scan/fuzz, set REPROIT_BACKEND_URL, or set backend.target in reproit.yaml
doctor: required checks failed   (exit 1)
```

After (verbatim):

```
  ok      target
          no explicit target; `reproit find` boots the package.json `start` script itself and tears it down after the run (override with --target <url>, REPROIT_BACKEND_URL, or backend.target)
doctor: required checks passed   (exit 0)
```

Doctor decides this with the same two-signal trust the boot machinery uses, without
booting anything (`boot::auto_target_plan`): a verified already-running server
reports as "a server on port N answers <route> and matches this schema, so runs will
use it" and is then probed for reachability and adapter tier.

### Bare `reproit scan` with no target (fixture A, nothing running)

Before: `Error: the schema has no absolute server URL; set REPROIT_BACKEND_URL to
the disposable service`. After (verbatim):

```
Error: no service URL is named yet. Run bare `reproit find` (it boots the service itself from the package.json start script), or start your service and pass --target <url> (or set REPROIT_BACKEND_URL / backend.target)
```

### Fixtures B and C (raw node / empty repo): the failing checks name the next input

Doctor still exits 1 on the empty-draft scaffold (there is genuinely nothing to run
yet), but both required failures are now phrased as the next input. Before:
`MISSING schema ... fix: the schema parses but declares no executable operations`
and the servers-entry target failure above. After (verbatim):

```
  MISSING schema
          openapi.yaml (0 operation(s))
          fix: the schema parses but declares 0 operations so far; add the routes your service serves, or rerun `reproit init` from the service's source root to derive them
  MISSING target
          no target named yet, and nothing here to boot one from
          fix: start your service and name it: `reproit find --target <url>` (or set REPROIT_BACKEND_URL / backend.target, or add a servers entry to the schema)
```

Init's degrade wording for a detected-but-schemaless framework also dropped its
"but no schema" phrasing: it now prints "detected <framework> (from <manifest>);
the schema is still to-write, so scaffolding a backend draft." with the same hint
and override lines.

### Monorepo root: the multi-service Ambiguous bail is now an honest degrade

Two express services under one root with a root package.json. Before, init exited 1:
`Error: init found 2 services under this root (svc-a, svc-b). Run it from one
service's directory, so the derived schema describes a single service`. After,
exit 0, nothing written (verified: no reproit.yaml/openapi.yaml/.reproit), verbatim:

```
  found 2 services under this root: svc-a, svc-b.
  One derived schema would merge routes no single service serves, so nothing was scaffolded.
  Next: run `reproit init` inside the service you want (e.g. `cd svc-a && reproit init`)
```

### Other rewrites in the same sweep (old -> new, all verbatim)

- fuzz candidate confirmation (user-owned target): "stateful or non-idempotent
  confirmation requires REPROIT_BACKEND_RESET_URL" -> "to confirm from clean state,
  run bare `reproit find` (a service reproit boots itself restarts as the reset), or
  set REPROIT_BACKEND_RESET_URL to a state-reset endpoint".
- scan/fuzz over an empty schema: "the backend schema(s) contain no executable
  operations" -> "the schema (<files>) declares 0 executable operations so far; add
  the routes your service serves (paths and methods), or rerun `reproit init` from
  the service's source root to derive them".
- `reproit init <url>` on an empty schema: "declares no executable operations;
  nothing to scan or fuzz" -> "declares 0 executable operations so far; point init
  at the schema that lists your operations (e.g. /openapi.json), or run bare
  `reproit init` from the service's source root to derive one".
- check gate with no configured schema: "backend project has no schema; set
  backend.schemas" -> "the backend schema for this check is still to-configure:
  list your schema file(s) under backend.schemas in reproit.yaml, or run
  `reproit init` to derive a draft from source".
- schema_paths: "backend.enabled is true but backend.schemas is empty" and
  "backend schema <path> does not exist" both gained the same to-do phrasing with
  the file/command to provide.
- `reproit surface`: "no schema in this service: nothing declares this surface, so
  nothing tests it" -> "the schema for this service is still to-write: nothing
  declares this surface yet (run `reproit init` here to derive a draft from these
  routes)".
- config loader: "unsupported platform <p>; known: <list>" -> "app.platform <p> is
  not one reproit knows; set it to one of: <list>".

### Encoded as an architecture test

`a_gap_is_always_phrased_as_the_next_input` (crates/reproit/tests/architecture.rs)
scans the non-comment production text of the init/doctor/find/check surfaces for the
banned dead-end phrasings ("no schema", "unreproducible", "not reproducible",
"has no servers entry", "no absolute server URL", "nothing to scan or fuzz",
"not supported", "unsupported", "confirmation requires REPROIT_BACKEND_RESET_URL",
"pass --platform to override") and pins the replacement next-input wordings so a
reword cannot quietly restore the bounce. Its docstring names the failure it
prevents: a first-run user bouncing off a dead-end error.

### Structure

doctor.rs crossed the 1000-line reviewability cap during the sweep and was split at
the backend/app boundary (workflows/doctor/backend.rs), the same split check.rs
uses; the multi-schema narrowing ratchet now covers both files.
