# Bare `reproit init` ground truth (Phase 0)

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
