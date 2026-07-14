# Using the reproit CLI

reproit drives your app like a user, finds bugs, and hands each one back as a
**repro**: a saved case that fails the same way every single time. No more
"cannot reproduce."

This guide gets you from zero to a saved regression guard, then covers the rest.
If you just want the command list, jump to [Reference](#reference).

## The idea in 30 seconds

reproit has three core verbs:

```sh
reproit scan    # audit visible bugs on every reachable screen
reproit fuzz    # find deeper sequence-dependent bugs
reproit check   # verify replayable bugs from fuzz/keep
```

`scan` is the fast audit pass; `fuzz` emits replayable `fnd_...` findings you can
`check` and `keep`. Both maintain the internal app model automatically.

Two things make it different:

- **It's deterministic.** A bug is captured as a seed plus an exact list of
  actions. Replay it and you get the identical failure, on any machine. That
  captured case is called a *repro*.
- **There is no AI inside it.** The engine that finds and replays bugs is plain,
  offline, and key-free. AI (your coding agent) plugs in from the outside over
  [MCP](#use-it-with-your-ai-agent-mcp) to read repros and fix them. reproit then
  proves the fix.

It also never relies on on-screen text to identify a screen, so the same app in
English or German is the same graph, and your repros survive copy edits and
translations.

## Your first run

From inside your app's project:

```sh
reproit scan --record         # audit visible bugs and save clips
# -> 6 issues across 4 screens.  Next: reproit fuzz --all

reproit fuzz --all            # hunt for confirmed, replayable bugs
# -> 3 repros found.  id fnd_a3f2c1b8e0d5   reproduce: reproit fnd_a3f2c1b8e0d5

reproit fnd_a3f2c1b8e0d5          # reproduce that finding
# -> fail (3/3).  real bug, reproduced every run

reproit keep fnd_a3f2c1b8e0d5 --as login-crash   # keep it as a guard
# -> saved (quarantined). Verify after the fix: reproit check

# ...you (or your agent) fix the bug...

reproit check                 # run the saved suite
# -> 1 passed.  promoted to a required guard
```

Every command ends by printing the exact next command to run, so you can follow
the trail without memorizing anything.

The screen graph is internal lifecycle state. `scan`, `fuzz`, `check`, auth
discovery, screenshots, imports, accessibility, coverage, and agent context
ensure it exists and is current before use. Reproit fingerprints relevant source
files, lockfiles, journeys, configuration, and its own version. Any change causes
an automatic refresh; Git commit and dirty state are recorded as provenance.

## The core loop

### Internal app model

Reproit crawls the running app and records each screen plus the actions between
them. Users do not maintain this graph or decide when it is stale.

```sh
reproit debug map show                 # inspect the current graph
reproit debug map structural --budget 20  # force a bounded rebuild
reproit debug map verify               # force a full live drift audit
```

The map is a directed graph, not a tree. Cycles are normal: tabs, back buttons,
menus, and lists often return to known screens. The crawler records those edges,
marks the state/action as tried, and spends the next step on another frontier
instead of looping on the same cycle.

On the first run reproit learns the model. If login blocks coverage, configure a
test account with [`reproit auth`](#test-logins-auth).

The crawl only reaches what it can actually get to (login walls and empty data
limit it). Advanced diagnostic views remain under `reproit debug map`.

### `scan`: scan every screen (the default find)

`reproit scan` is the fast "what's wrong here". It does ONE coverage crawl,
visiting every reachable screen once, and reports the bugs simply VISIBLE on each
screen: broken content (`[object Object]`, a bare `undefined`) and
choice-anomalies (one option of a picker that shifts the layout). You get one
finding per (screen x issue), grouped by screen,
nothing collapsed.

```sh
reproit scan https://app.com  # zero-config: scan a deployed app, no setup
reproit scan                  # scan the whole app (uses ./reproit.yaml)
reproit scan login            # scope the crawl to one alias/node
reproit scan ui.jsonl         # validate and render an A2UI v0.9 stream
reproit scan --record         # also save an annotated clip per boxable finding
```

`--record` (web) replays the path to each finding's screen and saves an annotated
video with a red box on the bug, one clip per (screen x issue), into
`.reproit/recordings/scan/<scan-run>/` (or `--out <dir>`). It clips the findings with an
on-screen element (content, broken-route, choice-anomaly, and the hang/jank
trigger). leak / crash have no single element to box, so those are
skipped.

Reach for `scan` first when auditing an app. It is deterministic (no action
permutations) and surfaces every per-screen issue, where `fuzz` collapses to one
finding per seed. `scan --record` is the fastest way to hand someone a clip of a
visible bug. Use `fuzz` when you need a replayable `fnd_...` finding that can be
checked and kept as a guard.

### `fuzz`: find the deep, sequence-dependent bugs

`reproit fuzz` combinatorially permutes action sequences to provoke replayable
bugs: crashes, jank, hangs, leaks, and any invariant that reproduces from a
stable action sequence. Each bug it finds becomes a candidate repro with a
content-hash `fnd_...` id. For bugs simply visible on a screen, run `scan` first
to audit and clip them; run `fuzz --all` when you want ids you can `check` and
`keep`.

```sh
reproit fuzz                  # hunt the whole app (uses ./reproit.yaml)
reproit fuzz login            # concentrate on one screen or flow
reproit fuzz https://app.com  # zero-config: point at a deployed app, no setup
reproit fuzz google.com       # a bare host works too (scheme is auto-added)
reproit fuzz ui.jsonl         # schema-valid A2UI mutations across React and Lit
reproit fuzz --all            # don't stop at the first bug; return every unique bug
```

The positional target is auto-detected: a URL (with or without a scheme, e.g.
`google.com` or `localhost:3000`) runs zero-config against that deployed app (it
synthesizes a web setup, builds the map, and fuzzes, no `reproit.yaml` needed);
anything else is treated as an alias to scope the hunt to.

An A2UI v0.9 JSON or JSONL target is detected structurally. Reproit validates it
against the official basic-catalog schemas, runs the official React and Lit
renderers, and applies only schema-valid stream mutations. Every finding stores
the smallest message stream that still produces the exact same signature, so
`reproit fnd_...` replays without an A2UI checkout or a separate command.

By default it stops at the first finding so you can fix it before hunting more.
`--all` keeps going and groups duplicates (the same crash reached by different
paths) into one bug each, with the shortest repro. That is the list your AI agent
gets over MCP.

Findings live in a throwaway artifact (gitignored). Nothing is added to your
committed graph or suite until you choose to `keep` it.

### Reproduce one bug; check the suite

Run `reproit <id>` for one bug. `reproit check` runs the whole saved suite. Both
classify the replay the same way:

| Outcome | Meaning | Exit code |
|---|---|---|
| **pass** | replayed, all green | 0 |
| **fail** | replayed, still broken (a real regression) | 1 |
| **flaky** | same actions, inconsistent result, so your app has a race | 2 |
| **stale** | the targeted element is gone (the UI changed), couldn't replay | 3 |

```sh
reproit rep_a3f2c1b8e0d5          # reproduce one saved repro (fnd_... works too)
reproit check                 # run your whole saved suite
reproit record <id>           # produce an annotated video of the bug
```

The `record` video is paced and annotated: a caption names each action (the
trigger step in red), and the clip ends with a red box around what broke - the
crashing control, the overflowing element, the `[object Object]` text, the choice
that shifts the layout. (Leak has no on-screen element, so no box.)

This is different from `scan --record`: scan clips are quick audit artifacts, one
per visible issue. `record <id>` is evidence for one replayable repro id
(`fnd_...`, `rep_...`, or an alias), and is what `watch <id>` opens later.

Because repros are stored by *structure* (developer keys), a button that simply
moved comes back as **stale**, not a false **fail**. The exit codes are the CI
contract.

### `keep`: turn a bug into a permanent guard

```sh
reproit keep fnd_a3f2c1b8e0d5 --as login-crash
```

`keep` saves a repro into your committed suite (`.reproit/repros/`). It is not a
git commit; it writes a local file. A kept repro starts **quarantined**
(reported but non-blocking) and is automatically promoted to a **required** guard
the first time it passes (that is, once you've fixed the bug). Re-keeping the same
case is harmless: it's content-addressed, so it maps to the same id and keeps its
history.

That's the whole loop: `scan` (audit and clips) -> `fuzz --all` (replayable
ids) -> `check <id>` (confirm it's real) -> `keep` (guard it) -> `check`
(prove the fix).

## Saving and re-running bugs

- `reproit repros` lists your saved repros with each one's last status and action
  sequence.
- `reproit watch <id>` opens a repro's recorded video (record one with
  `reproit record <id>`).
- `reproit repro simplify <id> --to '<actions>'` swaps in a shorter action
  sequence, but only if reproit can verify it still reproduces the same bug.
  Fuzz-found repros are sometimes tangled; this cleans them up safely. Your agent
  proposes a minimal sequence, reproit replays it, and adopts it only if it still
  triggers the bug.
- `reproit repro why [repro]` ranks the source code most likely to blame for a
  failure (spectrum-based fault localization). It needs both passing and failing
  runs, which `fuzz` produces, and is strongest on instrumented targets.

## Going further

### Journeys (scripted paths)

A *journey* is a short, declarative script through your app, stored as
`journeys/<name>.yaml` and run with `reproit journey <name>`. Use journeys to pin
important flows (login, checkout) and to give `fuzz` a deep starting point.

Each step is one of: `do:` (an action), `goto:` (pathfind to a screen),
`expect:` (assert state/text/count), or `fill:` (type into fields, with secrets
pulled from the vault). A top-level `setup: login(alice)` handles auth.

```yaml
setup: login(alice)
steps:
  - { goto: checkout }
  - { fill: { key:card: "4242424242424242" } }
  - { do: tap:key:pay }
  - { expect: { text: "Thank you" } }
```

Multi-user flows (one user posts, another sees it) are supported: add an `actors`
block and tag each step with its actor. reproit runs one device per actor and
coordinates them in order. See `reproit journey list` and `reproit journey create`.

### Structural contracts

Contracts express app-specific facts without matching English copy or writing
runner code. Put them at the top level of `reproit.yaml` for scan and fuzz, or
inside a journey for that flow. Reproit evaluates them over normalized actions,
actors, states, routes, visible text, oracle signals, network statuses, response
shapes, and counts from every runner.

```yaml
contracts:
  - id: peer-sees-message
    when:
      actor: alice
      action: tap:key:testid:send
    must:
      eventually:
        condition:
          all:
            - is: { actor: bob }
            - is: { text: Message delivered }
        withinSteps: 8
```

Formulas support `is`, `always`, `eventually`, `next`, `implies`, `all`, `any`,
and `not`. The default `scope` is `trace`; use `scope: state` only when each
single observation can prove or disprove the property. Contract action keys are
fed back into exploration as hints. A discovered violation receives a stable
fingerprint, exact replay confirmation, structural evidence, and shrink
protection, so minimization cannot silently replace it with a different bug.

### Fuzz from a journey

Reaching a deep screen is the expensive part of fuzzing. `reproit fuzz --from
<journey>` replays a journey to its end and then explores outward from there, so a
flow you already have becomes a launchpad for the bugs around it.

For a journey with `actors`, the authored steps are an immutable shared-state
checkpoint. Reproit launches one isolated session per actor, verifies the whole
checkpoint, then generates actor-aware interleavings using safe outgoing
transitions from each actor's structural state. A candidate is replayed from a
fresh checkpoint and minimized without deleting checkpoint steps. Confirmed
repros are written as `journeys/multi-<id>.yaml`, so the handoff is one command:
`reproit journey multi-<id>`.

### Import existing tests

`reproit import maestro flow.yaml` converts a Maestro flow into a reproit journey
(switching cost is near zero). It maps the common commands, inlines sub-flows,
unrolls loops, and prints an import summary; anything with no faithful
equivalent is left as a clearly marked `# TODO` comment rather than dropped.
When `.reproit/map/appmap.json` exists, text-only Maestro taps are resolved
through the observed map if the label matches one unique actionable element;
otherwise they stay TODOs until the app exposes a stable selector.

### Screenshots

`reproit screenshots <tour>` produces store and marketing screenshots by running a
journey in capture mode, fanned across locales and devices. The same journey
doubles as a `check` (where `shoot:` steps just navigate) and as a screenshot run
(where they take pictures). Because screens are locale-invariant, one tour covers
every language with no per-locale selectors. See the [screenshots
reference](#screenshots-1).

### Test logins (auth)

Test credentials live in an encrypted local vault, never in the repo or in your
journey YAML.

Once an account exists, the normal path is simply:

```sh
reproit auth alice
```

Reproit maps the unauthenticated UI when needed, recognizes semantic credential
and OTP fields across screen transitions, generates `login-alice.yaml`, and
accepts it only after a clean verification run. The explicit commands below are
the advanced/configuration surface.

Discovery is language independent. It uses the universal structural
[`inputPurpose` contract](auth-contract.md), never visible labels.

```sh
# Password login: drive the UI with journeys/login.yaml.
reproit auth alice --email alice@example.com --password "$ALICE_PASSWORD"

# Phone OTP login: deterministic test-mode code.
reproit auth driver --phone +15555550123 --otp 123456

# Session/API-style auth: restore a saved authenticated state, skip the UI.
reproit auth admin --session '{"localStorage":{"token":"test-token"}}'
reproit auth service --strategy api --session '{"headers":{"Authorization":"Bearer test-token"}}'
```

`auth` writes non-secret account metadata under `auth.accounts`, stores
provided values in the local vault. Journeys stay simple: `setup: login(alice)`.
Inside the login journey, use `secret:email`, `secret:phone`, `secret:username`,
`secret:password`, `secret:totp`, or `secret:otp`; the host resolves those values
before the runner types them, and redacts them from logs. For stored-session or
API-style accounts, use `setup: auth(admin)` or an actor binding like
`{ auth: admin }`.

### Many platforms, many locales

`fuzz` and `check` take cross-cutting flags:

```sh
--target chromium,firefox,webkit   # run each and diff them (finds divergence bugs)
--target ios,android               # same idea across mobile platforms
--locale de,ar,ja                  # fuzz across languages (RTL, overflow, i18n)
--device "iPhone 16 Pro Max"       # otherwise you get an interactive picker
```

## Use it with your AI agent (MCP)

reproit ships no built-in AI. Instead, `reproit mcp` exposes the engine to your
coding agent so the agent can run the loop itself: fuzz, read the repro, fix the
code, then `check` to prove it (a green check is deterministic, so the agent
*knows* it fixed the bug).

Register it once:

```sh
claude mcp add reproit -- /path/to/reproit mcp     # Claude Code
codex mcp add reproit -- /path/to/reproit mcp      # Codex
```

The agent gets tools like `reproit_fuzz`, `reproit_check`,
`reproit_accessibility`, and `reproit_context` (a scoped graph plus the
selectors it needs to act). Model maintenance is automatic and is deliberately
not exposed as an agent decision. Authoring, triage, and fixing remain the
agent's job; reproit is the ground truth and verifier.
Full tool list in the [reference](#mcp-tools).

## Cloud

The same `reproit` binary runs on a fleet for the broad, parallel outer loop:
fuzzing on every PR, and ingesting production crashes. The headline use case is
reproducing a **real production crash on your own machine**: the SDK reports the
session, and `reproit <bkt_...>` saves and reproduces it locally.

```sh
reproit cloud setup --app app_... --key sk_live_...    # once: validate, bind, verify
reproit bugs                                             # impact-ranked bucket ids
reproit bkt_...                                          # reproduce locally
reproit triage bkt_... fixed --fixed-in-build 1.2.3
reproit cloud resolution-events --app app_...
```

Local is the fast inner loop in your worktree; cloud is the broad outer loop with
history. Every cloud view is backed by exportable raw data.

---

# Reference

## All commands

```
reproit                       help: the scan -> fuzz -> check -> keep story
reproit scan [target]         scan every screen for visible bugs (--record for clips)
reproit fuzz [target]         find deeper interaction bugs
reproit <fnd_|rep_|bkt_...>    reproduce one bug
reproit check                  verify the whole saved suite
reproit keep [id] [--as name] keep a repro in your suite
reproit record <id>           annotated video of a repro (--flicker also scans it)
reproit baseline [--update]   visual-regression diff vs the committed baseline
reproit repros                list saved repros + last status
reproit repro simplify <id> --to ..  swap in a shorter, verified-equivalent sequence
reproit repro why [repro]     rank suspect code for a failure (Ochiai)
reproit watch <id>            open a repro's recorded video
reproit journey list|save     manage scripted journeys
reproit screenshots [tour]    store/marketing shots across locales + devices
reproit import maestro <f>    convert a Maestro flow into a journey
reproit auth <account>        configure, discover, and verify a test login
reproit mcp                   serve reproit to your coding agent (stdio)
reproit cloud ...             fleet + production telemetry (see Cloud)
reproit platforms             UI-framework -> backend matrix
reproit update                verify and install the latest CLI release
reproit debug map ...         advanced internal-model diagnostics
```

### Internal-model diagnostics

Normal commands refresh the graph automatically. These advanced views explain or
force its behavior:

- `debug map show`: render the current graph.
- `debug map structural`: force a full crawl.
- `debug map semantic`: an LLM reads your *source* for the screens that *should*
  exist, as a worklist (the one optional model call; never an assertion target).
- `debug map coverage`: diffs the screens your code declares against the screens the
  crawl actually verified, so "not fully mapped" becomes a named list.
- `debug map converge`: validates those candidates against the real map and prunes
  guesses.
- `debug map verify`: re-walks the committed map and reports drift (exit 3).
- `debug map accessibility`: the accessibility audit: which controls a mouse user can
  operate but a keyboard / screen-reader user cannot, per screen, each located by
  selector and source file:line. `--format md` prints an exportable, WCAG-cited
  report (redirect to a file); `--json` gives the structured form;
  `--baseline <appmap.json>` reports only the gaps NEW vs that baseline and exits
  1 if any appeared (a CI regression gate). See
  [docs/operability-graph.md](operability-graph.md).

## Flags (on fuzz / check)

```
--target ios|android|web|all   multi (a,b,c) -> run each + diff for divergence
--device "<name>"              else an interactive picker (when a TTY)
--locale de,ar,ja              fuzz across locales (RTL / overflow / i18n)
--from <journey>               (fuzz) replay a journey, then explore from its end
--times N                      repeat, to surface flakiness
--only / --no crash,jank,leak  narrow the oracles (default: all)
--strict                       new repros block instead of starting quarantined
```

## Globals (every command)

```
--json     machine-readable output (CI, scripts, the MCP bridge)
--quiet    minimal output
--yes      never prompt (non-interactive / CI)
--config   path to reproit.yaml (default: ./reproit.yaml)
```

Precedence: flag > config > default.

## Exit codes (the CI contract)

```
0  clean / all pass
1  real regression (replayed, still broken)
2  flaky (same actions, inconsistent result -> app race)
3  stale (UI changed, couldn't replay -> re-record, not a failure)
```

## Oracles

`fuzz` runs all of these by default; findings are tagged so you can filter with
`--only` / `--no`.

- **crash** uncaught exceptions / process death
- **jank** dropped frames past a threshold
- **leak** heap growth over a repeating cycle
- **visual** screenshot regression vs a baseline
- **divergence** disagreement between targets (run with multiple `--target`)
- **a11y** accessibility violations
- **overflow** DOM/layout overflow: content clipped or overflowing its container/viewport, including RTL / long-string breaks under other locales (web; deterministic structural measurement; run with `--locale`)

## MCP tools

```
reproit_context(target?)              scoped graph + screens + selectors for a target
reproit_accessibility(state?, kind?)  UI-vs-a11y diff per screen, grounded by selector + file:line
reproit_coverage()                    candidate map from source + coverage ledger + worklist
reproit_scan(target?)                 default find: state-present bugs, one per (screen x issue)
reproit_fuzz(target?, platform?)      deep sequence bugs (crash/jank/hang); deduped unique-bugs list
reproit_check(repro?)                 run a repro / journey / pending finding and classify it
reproit_record(repro, flicker?)       annotated video of a repro (flicker = also scan it)
reproit_baseline(update?)             visual-regression diff vs the committed baseline
reproit_keep(id?, as?)                save a repro into the suite
reproit_simplify(repro, actions)      adopt a shorter, verified-equivalent sequence
reproit_repros()                      list saved repros + status + actions
reproit_journeys()                    list authored journeys
reproit_journey_save(name, journey)   author a journey (incl. multi-user actors)
reproit_why(repro?)                   rank suspect code (Ochiai)
reproit_cloud_buckets(app?, query?)              impact-ranked finding buckets
reproit_cloud_blast_radius(bucket, app?)         who's affected (cohorts, %, versions)
reproit_cloud_reproduce(bucket, as, run?, app?)  pull a real session + optionally replay it
reproit_cloud_pull(bucket?, top?, as, app?)      pull a bug as a first-class LOCAL repro
reproit_cloud_triage(bucket, status?, fixed_in_build?, assignee?, app?)  read/set triage state
reproit_cloud_resolution_events(app?)            recent prod-truth transitions (monitor regressions)
reproit_cloud_timeline(bucket, app?)             per-bucket occurrence series + resolution
```

`cloud setup` persists the selected app and validated credential, so `bugs`,
`pull`, and `triage` need no repeated app or key flags. Explicit flags and
`$REPROIT_CLOUD_APP` remain available for automation and multi-project use.

The full production loop (manage + monitor, not just fix): `reproit_cloud_buckets`
(impact-ranked) -> `reproit_cloud_pull` the top -> `reproit_check` (reproduce) ->
fix -> `reproit_check` (verify) -> `reproit_keep` -> `reproit_cloud_triage`
status=fixed --fixed-in-build X (record the fix intent) -> watch
`reproit_cloud_resolution_events` for a regression (prod contradicting the claim).

## Cloud commands

```
reproit login                       cloud/project key, sk_live_...
reproit cloud setup --app <app>     one-time repo + SDK + CI wiring and live verification
reproit bugs [query]                impact-ranked confirmed production bugs
reproit <bkt_...>                   pull and verify locally
reproit triage <bucket> <status>    update lifecycle state
reproit cloud fuzz [--pr N]         fuzz locally, store confirmed result in Cloud
reproit cloud buckets --app <app>    impact-ranked bucket ids
reproit cloud findings --app <app>   cohorts and user discriminators
reproit cloud blast-radius --app <app> --bucket <id>   who's affected
reproit cloud triage --app <app> --bucket <id> [--status <s> --fixed-in-build <v> --assignee <id>]
reproit cloud resolution-events --app <app>
reproit cloud timeline --app <app> --bucket <id>
reproit cloud query --app <app> [--query <text>] --export
```

## screenshots

```sh
reproit screenshots [tour]
  --locale de,ar,ja      # fan across locales (RTL / i18n)
  --target ios,android   # fan across platforms / engines
  --device "a,b"         # fan across devices
  --out screenshots      # output root
  --path-template "{locale}/{device}"   # override the auto layout
  --no-verify            # skip the cross-screen verify gate (on by default)
```

Output is journey-led and collapses axes that don't vary:
`<out>/<journey>[/<platform>][/<locale>][/<device>]/<name>.png`. The `platform`
level appears only when you fan more than one. For exact control (e.g. the layout
`fastlane deliver` expects) set a `--path-template` with `{journey}` `{platform}`
`{locale}` `{device}`. Config lives under `screenshots:` in `reproit.yaml`; a
runnable example tour is at `examples/journeys/marketing.yaml`. Capture works on
every supported platform via that platform's native grab.

## `.reproit/` layout

The local project state is grouped by concept:

```
.reproit/
  map/                  # appmap.json, visits.json, semantic candidates
  runs/                 # raw evidence from scan/fuzz/check/record runs
  recordings/
    scan/               # quick audit clips from scan --record
    repro/              # record <id> videos opened by watch <id>
  repros/               # saved regression guards
  tmp/                  # transient runner scratch
  secrets.vault         # local auth vault
```

`runs/`, `recordings/`, `tmp/`, logs, and vault files are local-only. `repros/`
is the guard suite; `map/` is the learned graph if you choose to review it.

## Config (reproit.yaml)

Every field supports shell-style environment interpolation: `${VAR}` (empty if
unset), `${VAR:-default}` (fallback), `${VAR:?message}` (required). A minimal,
ready-to-copy config for each platform lives in `examples/configs/`, one file per
platform (`reproit.web.yaml`, `reproit.winui.yaml`, `reproit.tui.yaml`,
and so on).

## Background

- **Why screens are identified by structure, not text** (so the graph is
  locale-invariant and survives copy edits): [docs/signature.md](signature.md).
- **The accessibility audit** (the UI-vs-a11y-graph diff):
  [docs/operability-graph.md](operability-graph.md).

reproit ships no bundled LLM. Authoring, triage, and fixing live in your agent
over MCP; reproit is the engine that finds, replays, and verifies.
