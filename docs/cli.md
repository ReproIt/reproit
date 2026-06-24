# Using the reproit CLI

reproit drives your app like a user, finds bugs, and hands each one back as a
**repro**: a saved case that fails the same way every single time. No more
"cannot reproduce."

This guide gets you from zero to a saved regression guard, then covers the rest.
If you just want the command list, jump to [Reference](#reference).

## The idea in 30 seconds

reproit has three core verbs:

```sh
reproit map     # learn your app: build a graph of its screens
reproit sweep   # find what's wrong: scan every screen for visible bugs
reproit check   # verify a bug: does it still reproduce? is it fixed yet?
```

(`reproit fuzz` is the deeper, opt-in search for sequence-dependent bugs:
crashes, jank, hangs. `sweep` is the fast default.)

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
reproit map                   # detect the platform, build the screen graph
# -> mapped 47 screens.  Next: reproit fuzz

reproit fuzz                  # hunt for bugs
# -> 3 repros found.  id a3f2c1b8e0d5   confirm: reproit check a3f2c1b8e0d5

reproit check a3f2c1b8e0d5    # does that finding really reproduce?
# -> fail (3/3).  real bug, reproduced every run

reproit keep a3f2c1b8e0d5 --as login-crash   # save it as a guard
# -> saved (quarantined). Verify after the fix: reproit check

# ...you (or your agent) fix the bug...

reproit check                 # run the saved suite
# -> 1 passed.  promoted to a required guard
```

Every command ends by printing the exact next command to run, so you can follow
the trail without memorizing anything.

## The core loop

### `map`: learn the app

`reproit map` crawls your running app and records each screen plus the actions
that move between them. Run it once to start, and again whenever the app changes.

```sh
reproit map            # build the graph (this is the common one)
reproit map --show     # just render the existing graph (fast, no run)
```

On the very first run it scaffolds a `reproit.yaml` config for you. If a login
screen blocks the crawl, it stops and tells you to add test credentials with
[`reproit secrets set`](#test-logins-secrets).

The crawl only reaches what it can actually get to (login walls and empty data
limit it). `map` has a few companion views for understanding coverage and
accessibility; see [More map views](#more-map-views).

### `sweep`: scan every screen (the default find)

`reproit sweep` is the fast "what's wrong here". It does ONE coverage crawl,
visiting every reachable screen once, and reports the bugs simply VISIBLE on each
screen: overflow/clipping, broken content (`[object Object]`, a bare `undefined`),
unlabeled tappables (a11y), and choice-anomalies (one option of a picker that
shifts the layout). You get one finding per (screen x issue), grouped by screen,
nothing collapsed.

```sh
reproit sweep https://app.com  # zero-config: scan a deployed app, no setup
reproit sweep                  # scan the whole app (uses ./reproit.yaml)
reproit sweep login            # scope the crawl to one alias/node
```

Reach for `sweep` first when auditing an app. It is deterministic (no action
permutations) and surfaces every per-screen issue, where `fuzz` collapses to one
finding per seed. Use `fuzz` for the deeper, sequence-dependent bugs.

### `fuzz`: find the deep, sequence-dependent bugs

`reproit fuzz` combinatorially permutes action sequences to provoke the bugs that
only appear after the right actions in the right order: crashes, jank, hangs, and
memory leaks. Each bug it finds becomes a candidate repro with a content-hash id.
(For bugs simply visible on a screen, prefer `sweep` above.)

```sh
reproit fuzz                  # hunt the whole app (uses ./reproit.yaml)
reproit fuzz login            # concentrate on one screen or flow
reproit fuzz https://app.com  # zero-config: point at a deployed app, no setup
reproit fuzz google.com       # a bare host works too (scheme is auto-added)
reproit fuzz --all            # don't stop at the first bug; return every unique bug
```

The positional target is auto-detected: a URL (with or without a scheme, e.g.
`google.com` or `localhost:3000`) runs zero-config against that deployed app (it
synthesizes a web setup, builds the map, and fuzzes, no `reproit.yaml` needed);
anything else is treated as an alias to scope the hunt to.

By default it stops at the first finding so you can fix it before hunting more.
`--all` keeps going and groups duplicates (the same crash reached by different
paths) into one bug each, with the shortest repro. That is the list your AI agent
gets over MCP.

Findings live in a throwaway artifact (gitignored). Nothing is added to your
committed graph or suite until you choose to `keep` it.

### `check`: verify a bug

`reproit check` replays a repro and tells you exactly what happened:

| Outcome | Meaning | Exit code |
|---|---|---|
| **pass** | replayed, all green | 0 |
| **fail** | replayed, still broken (a real regression) | 1 |
| **flaky** | same actions, inconsistent result, so your app has a race | 2 |
| **stale** | the targeted element is gone (the UI changed), couldn't replay | 3 |

```sh
reproit check a3f2c1b8e0d5    # check one finding or saved repro (by id or alias)
reproit check                 # run your whole saved suite
reproit record <id>           # produce an annotated video of the bug
```

The `record` video is paced and annotated: a caption names each action (the
trigger step in red), and the clip ends with a red box around what broke - the
crashing control, the overflowing element, the `[object Object]` text, the choice
that shifts the layout. (dead-end and leak have no on-screen element, so no box.)

Because repros are stored by *structure* (developer keys), a button that simply
moved comes back as **stale**, not a false **fail**. The exit codes are the CI
contract.

### `keep`: turn a bug into a permanent guard

```sh
reproit keep a3f2c1b8e0d5 --as login-crash
```

`keep` saves a repro into your committed suite (`.reproit/repros/`). It is not a
git commit; it writes a local file. A kept repro starts **quarantined**
(reported but non-blocking) and is automatically promoted to a **required** guard
the first time it passes (that is, once you've fixed the bug). Re-keeping the same
case is harmless: it's content-addressed, so it maps to the same id and keeps its
history.

That's the whole loop: `sweep` (or `fuzz`) -> `check <id>` (confirm it's real) ->
`keep` (guard it) -> `check` (prove the fix).

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
`journeys/<name>.yaml` and run with `reproit check <name>`. Use journeys to pin
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
coordinates them in order. See `reproit journey list` and `reproit journey save`.

### Fuzz from a journey

Reaching a deep screen is the expensive part of fuzzing. `reproit fuzz --from
<journey>` replays a journey to its end and then explores outward from there, so a
flow you already have becomes a launchpad for the bugs around it.

### Import existing tests

`reproit import maestro flow.yaml` converts a Maestro flow into a reproit journey
(switching cost is near zero). It maps the common commands, inlines sub-flows,
unrolls loops, and prints a compatibility report; anything with no faithful
equivalent is left as a clearly marked `# TODO` comment rather than dropped.

### Screenshots

`reproit screenshots <tour>` produces store and marketing screenshots by running a
journey in capture mode, fanned across locales and devices. The same journey
doubles as a `check` (where `shoot:` steps just navigate) and as a screenshot run
(where they take pictures). Because screens are locale-invariant, one tour covers
every language with no per-locale selectors. See the [screenshots
reference](#screenshots-1).

### Test logins (secrets)

Test credentials live in an encrypted local vault, never in the repo or in your
journey YAML.

```sh
reproit secrets set alice.password        # prompts (or reads stdin)
reproit secrets set-totp alice.totp <b32> # a 2FA / OTP seed
reproit secrets list                      # names only, never values
```

Declare accounts under `auth.accounts` in `reproit.yaml` and reference them with
`setup: login(alice)`. Secrets are resolved on your machine and redacted from
logs, so a runner never sees a password.

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

The agent gets tools like `reproit_map`, `reproit_fuzz`, `reproit_check`,
`reproit_accessibility`, and `reproit_context` (a scoped graph plus the
selectors it needs to act). Authoring, triage, and fixing are deliberately the
agent's job, not built-in tools; reproit is the ground truth and the verifier.
Full tool list in the [reference](#mcp-tools).

## Cloud

The same `reproit` binary runs on a fleet for the broad, parallel outer loop:
fuzzing on every PR, and ingesting production crashes. The headline use case is
reproducing a **real production crash on your own machine**: the SDK reports the
session, and `reproit cloud reproduce <bucket>` replays it locally.

```sh
reproit cloud findings              # grouped crash buckets (fuzz + production)
reproit cloud blast-radius <bucket> # who's affected: cohorts, %, versions
reproit cloud reproduce <bucket>    # pull a real session and replay it here
```

Local is the fast inner loop in your worktree; cloud is the broad outer loop with
history. Every cloud view is backed by exportable raw data.

---

# Reference

## All commands

```
reproit                       help: the map -> sweep -> check story + top commands
reproit map                   build the app's screen graph (bare map = map structural)
reproit map --show            render the existing graph instead of rebuilding
reproit sweep [target]        scan every screen for visible bugs (the default find)
reproit fuzz [target]         find deep sequence bugs (crash/jank/hang); opt-in
reproit check [repro|journey] verify: pass(0) / fail(1) / flaky(2) / stale(3)
reproit keep [id] [--as name] save a repro into your suite
reproit record <id>           annotated video of a repro (--flicker also scans it)
reproit baseline [--update]   visual-regression diff vs the committed baseline
reproit repros                list saved repros + last status
reproit repro simplify <id> --to ..  swap in a shorter, verified-equivalent sequence
reproit repro why [repro]     rank suspect code for a failure (Ochiai)
reproit watch <id>            open a repro's recorded video
reproit journey list|save     manage scripted journeys
reproit screenshots [tour]    store/marketing shots across locales + devices
reproit import maestro <f>    convert a Maestro flow into a journey
reproit secrets set <k> [v]   store a test-login secret (also: set-totp, list, remove, test)
reproit mcp                   serve reproit to your coding agent (stdio)
reproit cloud ...             fleet + production telemetry (see Cloud)
reproit platforms             UI-framework -> backend matrix
```

### More map views

Beyond `map` (crawl) and `map --show` (render), these help you understand and
audit the graph:

- `map semantic`: an LLM reads your *source* for the screens that *should*
  exist, as a worklist (the one optional model call; never an assertion target).
- `map coverage`: diffs the screens your code declares against the screens the
  crawl actually verified, so "not fully mapped" becomes a named list.
- `map converge`: validates those candidates against the real map and prunes
  guesses.
- `map verify`: re-walks the committed map and reports drift (exit 3).
- `map accessibility`: the accessibility audit: which controls a mouse user can
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
- **i18n** overflow / clip / untranslated / RTL breaks (with `--locale`)
- **overflow** DOM/layout overflow: content clipped or overflowing its container/viewport (web; deterministic structural measurement)

## MCP tools

```
reproit_context(target?)              scoped graph + screens + selectors for a target
reproit_map(show?)                    build/refresh the graph (show = render existing)
reproit_accessibility(state?, kind?)  UI-vs-a11y diff per screen, grounded by selector + file:line
reproit_coverage()                    candidate map from source + coverage ledger + worklist
reproit_sweep(target?)                default find: state-present bugs, one per (screen x issue)
reproit_fuzz(target?, platform?)      deep sequence bugs (crash/jank/hang); deduped unique-bugs list
reproit_check(repro?)                 run a repro / journey / pending finding and classify it
reproit_record(repro, flicker?)       annotated video of a repro (flicker = also scan it)
reproit_baseline(repro?, update?)     visual-regression diff vs the committed baseline
reproit_keep(id?, as?)                save a repro into the suite
reproit_simplify(repro, actions)      adopt a shorter, verified-equivalent sequence
reproit_repros()                      list saved repros + status + actions
reproit_journeys()                    list authored journeys
reproit_journey_save(name, journey)   author a journey (incl. multi-user actors)
reproit_why(repro?)                   rank suspect code (Ochiai)
reproit_cloud_buckets(app?, query?)              impact-ranked finding buckets
reproit_cloud_blast_radius(bucket, app?)         who's affected (cohorts, %, versions)
reproit_cloud_reproduce(bucket, app?)            pull a real session + replay it
reproit_cloud_pull(bucket, as, app?)             pull a bug as a first-class LOCAL repro
reproit_cloud_triage(bucket, status?, fixed_in_build?, assignee?, app?)  read/set triage state
reproit_cloud_resolution_events(app?)            recent prod-truth transitions (monitor regressions)
reproit_cloud_timeline(bucket, app?)             per-bucket occurrence series + resolution
```

Cloud tools take the app id from the `app` argument or `$REPROIT_CLOUD_APP`.

The full production loop (manage + monitor, not just fix): `reproit_cloud_buckets`
(impact-ranked) -> `reproit_cloud_pull` the top -> `reproit_check` (reproduce) ->
fix -> `reproit_check` (verify) -> `reproit_keep` -> `reproit_cloud_triage`
status=fixed --fixed-in-build X (record the fix intent) -> watch
`reproit_cloud_resolution_events` for a regression (prod contradicting the claim).

## Cloud commands

```
reproit cloud login                 service token (distinct from secrets)
reproit cloud fuzz [--pr N]         fan-out job -> stored artifact (auto-links to a PR)
reproit cloud findings              grouped buckets + counts (fuzz + production)
reproit cloud blast-radius <bucket> who's affected: cohorts, %, versions (--export)
reproit cloud reproduce <bucket>    pull a real user session, replay locally
reproit cloud pull --bucket <id> --as <name>   pull a bug as a first-class LOCAL repro
reproit cloud triage --bucket <id> [--status <s> --fixed-in-build <v> --assignee <id>]  read/set triage state
reproit cloud resolution-events     recent prod-truth transitions (monitor regressions)
reproit cloud timeline --bucket <id>  per-bucket occurrence series + resolution
reproit cloud query ... --export    raw data out for your own analysis
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

## Config (reproit.yaml)

Every field supports shell-style environment interpolation: `${VAR}` (empty if
unset), `${VAR:-default}` (fallback), `${VAR:?message}` (required). A minimal,
ready-to-copy config for each platform lives in `examples/configs/`, one file per
platform (`reproit.web-playwright.yaml`, `reproit.winui.yaml`, `reproit.tui.yaml`,
and so on).

## Background

- **Why screens are identified by structure, not text** (so the graph is
  locale-invariant and survives copy edits): [docs/signature.md](signature.md).
- **The accessibility audit** (the UI-vs-a11y-graph diff):
  [docs/operability-graph.md](operability-graph.md).

## Coming from the old CLI?

The previous CLI's commands fold into the three verbs:

| Old | New |
|---|---|
| `init` | `map` (scaffolds on first run) |
| `doctor` | folded into `map` |
| `graph` | `map --show` |
| `run` | `record` |
| `check --record` | `record` |
| `check --visual` | `baseline` |
| `check --flicker` | `record --flicker` |
| `gate` | `check` |
| `soak` | `fuzz --soak` (leak oracle) |
| `visual` | `baseline` |
| `web-diff` | `fuzz --target <engines>` |
| `simplify` | `repro simplify` |
| `localize` / `why` | `repro why` |
| `auth` | `secrets set` |
| `triage` | `cloud findings` / `cloud reproduce` |
| `author` / `analyze` / `fix` | your agent over MCP |

Authoring, triage, and fixing moved to your agent over MCP, because reproit ships
no bundled LLM. (A BYO-key escape hatch can restore them in the bare CLI if
`ANTHROPIC_API_KEY` is set; off by default.)
