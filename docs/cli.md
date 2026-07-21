# Using the reproit CLI

reproit drives your app like a user, finds bugs, and hands each one back as a **repro**: a saved
case that fails the same way every single time. No more "cannot reproduce."

This guide gets you from zero to a saved regression guard, then covers the rest. If you just want
the command list, jump to [Reference](#reference).

## The idea in 30 seconds

reproit has three core workflows:

```sh
reproit scan    # audit visible bugs on every reachable screen
reproit fuzz    # find deeper sequence-dependent bugs
reproit check   # verify replayable bugs from fuzz/keep
```

`scan` is the fast audit pass; `fuzz` emits replayable `fnd_...` findings you can `check` and
`keep`. Both maintain the internal app model automatically.

Two things make it different:

- **It's deterministic.** A bug is captured as a seed plus an exact list of actions. Replay it and
  you get the identical failure, on any machine. That captured case is called a _repro_.
- **There is no AI inside it.** The engine that finds and replays bugs is plain, offline, and
  key-free. AI (your coding agent) plugs in from the outside over
  [MCP](#use-it-with-your-ai-agent-mcp) to read repros and fix them. reproit then proves the fix.

It also never relies on on-screen text to identify a screen, so the same app in English or German is
the same graph, and your repros survive copy edits and translations.

## Your first run

From inside your app's project:

```sh
reproit scan --record-video   # audit visible bugs and save clips
# -> 6 issues across 4 screens.  Next: reproit fuzz --all

reproit fuzz --all            # hunt for confirmed, replayable bugs
# -> 3 repros found.  id fnd_a3f2c1b8e0d5   reproduce: reproit fnd_a3f2c1b8e0d5

reproit fnd_a3f2c1b8e0d5          # reproduce that finding
# -> fail (3/3).  real bug, reproduced every run

reproit proof fnd_a3f2c1b8e0d5    # explain authority, replay, minimization, and promotion

reproit keep fnd_a3f2c1b8e0d5 --as login-crash   # keep it as a guard
# -> saved (quarantined). Verify after the fix: reproit check

# ...you (or your agent) fix the bug...

reproit check                 # run the saved suite
# -> 1 passed.  promoted to a required guard
```

Every command ends by printing the exact next command to run, so you can follow the trail without
memorizing anything.

The screen graph is internal lifecycle state. `scan`, `fuzz`, `check`, auth discovery, screenshots,
imports, accessibility, coverage, and agent context ensure it exists and is current before use.
ReproIt fingerprints relevant source files, lockfiles, journeys, configuration, and its own version.
Any change causes an automatic refresh; Git commit and dirty state are recorded as provenance.

## The core loop

### Internal app model

ReproIt crawls the running app and records each screen plus the actions between them. Users do not
maintain this graph or decide when it is stale.

```sh
reproit debug map show                 # inspect the current graph
reproit debug map structural --budget 20  # force a bounded rebuild
reproit debug map verify               # force a full live drift audit
```

The map is a directed graph, not a tree. Cycles are normal: tabs, back buttons, menus, and lists
often return to known screens. The crawler records those edges, marks the state/action as tried, and
spends the next step on another frontier instead of looping on the same cycle.

On the first run reproit learns the model. If login blocks coverage, configure a test account with
[`reproit auth`](#test-logins-auth).

The crawl only reaches what it can actually get to (login walls and empty data limit it). Advanced
diagnostic views remain under `reproit debug map`.

### `scan`: scan every screen (the default find)

`reproit scan` is the fast "what's wrong here". It does ONE coverage crawl, visiting every reachable
screen once, and reports the bugs simply VISIBLE on each screen: broken content (`[object Object]`,
a bare `undefined`), choice-anomalies (one option of a picker that shifts the layout), and verified
broken routes. You get one finding per (screen x issue), grouped by screen, nothing collapsed.
Every finding retains its `authoritative` or `specialist` classification; classification describes
the oracle's policy boundary and never hides an observation whose predicate held.

```sh
reproit scan https://app.com  # zero-config: scan a deployed app, no setup
reproit scan                  # scan the whole app (uses ./reproit.yaml)
reproit scan login            # scope the crawl to one alias/node
reproit scan ui.jsonl         # validate and render an A2UI v0.9 stream
reproit scan --record-video   # also save an annotated clip per boxable finding
```

`--record-video` (web) runs the path to each finding's screen and saves an annotated video with a
red box on the bug, one clip per (screen x issue), into
`.reproit/recordings/scan/<scan-run>/` (or `--out <dir>`). It clips the findings with an on-screen
element (content, broken-route, choice-anomaly). Sequence-dependent hang, jank, leak, and crash
findings remain in `fuzz`; they are not inferred from a single screen crawl.

Reach for `scan` first when auditing an app. It is deterministic (no action permutations) and
surfaces every per-screen issue, where `fuzz` collapses to one finding per seed.
`scan --record-video` is the fastest way to hand someone a clip of a visible bug. Use `fuzz` when
you need a replayable `fnd_...` finding that can be checked and kept as a guard.

### `fuzz`: find the deep, sequence-dependent bugs

`reproit fuzz` combinatorially permutes action sequences to provoke replayable bugs: crashes, jank,
hangs, leaks, and any invariant that reproduces from a stable action sequence. Each bug it finds
becomes a candidate repro with a content-hash `fnd_...` id. For bugs simply visible on a screen, run
`scan` first to audit and clip them; run `fuzz --all` when you want ids you can `check` and `keep`.

```sh
reproit fuzz                  # hunt the whole app (uses ./reproit.yaml)
reproit fuzz login            # concentrate on one screen or flow
reproit fuzz https://app.com  # zero-config: point at a deployed app, no setup
reproit fuzz google.com       # a bare host works too (scheme is auto-added)
reproit fuzz ui.jsonl         # schema-valid A2UI mutations across React and Lit
reproit fuzz --all            # don't stop at the first bug; return every unique bug
```

The positional target is auto-detected: a URL (with or without a scheme, e.g. `google.com` or
`localhost:3000`) runs zero-config against that deployed app (it synthesizes a web setup, builds the
map, and fuzzes, no `reproit.yaml` needed); anything else is treated as an alias to scope the hunt
to.

An A2UI v0.9 JSON or JSONL target is detected structurally. ReproIt validates it against the
official basic-catalog schemas, runs the official React and Lit renderers, and applies only
schema-valid stream mutations. Every finding stores the smallest message stream that still produces
the exact same signature, so `reproit fnd_...` replays without an A2UI checkout or a separate
command.

By default it stops at the first finding so you can fix it before hunting more. `--all` keeps going
and groups duplicates (the same crash reached by different paths) into one bug each, with the
shortest repro. That is the list your AI agent gets over MCP.

Findings live in a throwaway artifact (gitignored). Nothing is added to your committed graph or
suite until you choose to `keep` it.

### Reproduce one bug; check the suite

Run `reproit <id>` for one bug or `reproit @name` for a saved alias or journey. `reproit check`
runs the whole saved suite. All three forms classify replay the same way:

| Outcome   | Meaning                                                        | Exit code |
| --------- | -------------------------------------------------------------- | --------- |
| **pass**  | replayed, all green                                            | 0         |
| **fail**  | replayed, still broken (a real regression)                     | 1         |
| **flaky** | same actions, inconsistent result, so your app has a race      | 2         |
| **stale** | the targeted element is gone (the UI changed), couldn't replay | 3         |

```sh
reproit rep_a3f2c1b8e0d5          # reproduce one saved repro (fnd_... works too)
reproit @login-crash              # reproduce one saved repro by alias
reproit proof rep_a3f2c1b8e0d5    # inspect its immutable proof ledger
reproit candidates                # show candidates and exact promotion blockers
reproit check                     # run your whole saved suite
reproit check --changed [BASE]    # mapped repros first, then the full suite
reproit create                # demonstrate a bug and preserve the original human capture
reproit create --attach       # start from an already-running app
reproit create --push         # create, review in browser, then push the original
reproit create --cloud-tester # verify and shrink an SDK-marked Cloud capture
reproit <id> --record-video   # run the bug and produce annotated video evidence
```

The repro video is paced and annotated: a caption names each action (the trigger step in red),
and the clip ends with a red box around what broke - the crashing control, the overflowing element,
the `[object Object]` text, the choice that shifts the layout. (Leak has no on-screen element, so no
box.)

`create` captures what the tester actually experienced. It launches the configured app
by default; `--attach` begins from the current state of an app that is already running. The tester
uses the app normally and returns to the terminal to stop. Repro It stores an immutable original in
`.reproit/captures/cap_.../`. It does not need an oracle, replay the session, or remove actions.
The manifest reports video, action, and state-graph channels independently, so a visual-only bug is
still a valid capture and unavailable structural evidence is never invented. The default macOS
path starts main-display video before launching the configured app after Screen Recording
permission is granted. It does not passively infer actions or states. An instrumented SDK may
export them to JSON while the capture runs; pass that
live export path with `--actions-file` and Repro It reads and freezes it after you stop. Other hosts
currently require such an SDK export. A capture with no finalized video and no action export fails
closed and reports the private staging directory for review or deletion.

Original captures remain local unless push is explicitly requested. `create --push` opens a
browser review where the signed-in user selects the organization project, title, description,
severity, and visibility. After approval, the CLI uploads each file and its SHA-256 hash, then the
Cloud verifies every hash before publishing the private capture page. `--no-open` prints the review
link for a headless terminal. The local original remains immutable. An interrupted upload can be
resumed with `reproit push cap_...`.

The capture id is also the direct command. Bare `reproit cap_...` shows its local summary,
`--watch` opens the original video, `reproit push cap_...` starts or resumes browser review, and
`--open` opens the completed Cloud capture page. Global `--json` returns the same status as
structured output. A separate `inspect` command is unnecessary.

Any later deterministic replay or minimized repro is a derived artifact and must reference its
parent `cap_...`; it never
replaces or mutates the original. The older SDK/Cloud tester workflow is available explicitly as
`create --cloud-tester`: it pulls a marked rolling path, verifies the captured state, and derives a
minimized repro only when verification succeeds.

This is different from `scan --record-video`: scan clips are quick audit artifacts, one per visible
issue. `<id> --record-video` is evidence for one replayable repro id (`fnd_...`, `rep_...`, or an
alias), and is what `watch <id>` opens later.

Because repros are stored by _structure_ (developer keys), a button that simply moved comes back as
**stale**, not a false **fail**. The exit codes are the CI contract.

### `keep`: turn a bug into a permanent guard

```sh
reproit keep fnd_a3f2c1b8e0d5 --as login-crash
```

`keep` saves a repro into your committed suite (`.reproit/repros/`). It is not a git commit; it
writes a local file. A kept repro starts **quarantined** (reported but non-blocking) and is
automatically promoted to a **required** guard the first time it passes (that is, once you've fixed
the bug). Re-keeping the same case is harmless: it's content-addressed, so it maps to the same id
and keeps its history.

That's the whole loop: `scan` (audit and clips) -> `fuzz --all` (replayable ids) -> `reproit <id>`
(confirm it's real) -> `keep` (guard it) -> `check` (prove the fix).

## Saving and re-running bugs

- `reproit repros` lists your saved repros with each one's last status and action sequence.
- `reproit watch <id>` opens a repro's recorded video (make one with
  `reproit <id> --record-video`).
- `reproit repro simplify <id> --to '<actions>'` swaps in a shorter action sequence, but only if
  reproit can verify it still reproduces the same bug. Fuzz-found repros are sometimes tangled; this
  cleans them up safely. Your agent proposes a minimal sequence, reproit replays it, and adopts it
  only if it still triggers the bug.
- `reproit repro why [repro]` ranks the source code most likely to blame for a failure
  (spectrum-based fault localization). It needs both passing and failing runs, which `fuzz`
  produces, and is strongest on instrumented targets.

## Going further

### Journeys (scripted paths)

A _journey_ is a short, declarative script through your app, stored as `journeys/<name>.yaml` and
run with `reproit journey <name>`. Use journeys to pin important flows (login, checkout) and to give
`fuzz` a deep starting point.

Each step is one of: `do:` (an action), `goto:` (pathfind to a screen), `expect:` (assert
state/text/count), or `fill:` (type into fields, with secrets pulled from the vault). A top-level
`setup: login(alice)` handles auth.

```yaml
setup: login(alice)
steps:
  - { goto: checkout }
  - { fill: { key:card: "4242424242424242" } }
  - { do: tap:key:pay }
  - { expect: { text: "Thank you" } }
```

Multi-user flows (one user posts, another sees it) are supported: add an `actors` block and tag each
step with its actor. reproit runs one device per actor and coordinates them in order. See
`reproit journey list` and `reproit journey create`.

For concurrent exploration, the application owner may declare exact cross-actor action pairs that
commute. Undeclared pairs remain dependent, so they are never reordered or pruned:

```yaml
actors: [alice, bob]
independentActions:
  - { left: "tap:key:refresh", right: "tap:key:open-settings" }
steps:
  - { actor: alice, do: "tap:key:refresh" }
  - { actor: bob, do: "tap:key:open-settings" }
```

These declarations reduce equivalent schedules during search only. They do not create authority or
change the contract and replay requirements for a confirmed finding.

### Structural contracts

Contracts express app-specific facts without matching English copy or writing runner code. Put them
at the top level of `reproit.yaml` for scan and fuzz, or inside a journey for that flow. ReproIt
evaluates them over normalized actions, actors, states, routes, visible text, oracle signals,
network statuses, response shapes, and counts from every runner.

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

Formulas support `is`, `always`, `eventually`, `next`, `implies`, `all`, `any`, and `not`. The
default `scope` is `trace`; use `scope: state` only when each single observation can prove or
disprove the property. Contract action keys are fed back into exploration as hints. A discovered
violation receives a stable fingerprint, exact replay confirmation, structural evidence, and shrink
protection, so minimization cannot silently replace it with a different bug.

### Fuzz from a journey

Reaching a deep screen is the expensive part of fuzzing. `reproit fuzz --from
<journey>` replays a
journey to its end and then explores outward from there, so a flow you already have becomes a
launchpad for the bugs around it.

For a journey with `actors`, the authored steps are an immutable shared-state checkpoint. ReproIt
launches one isolated session per actor, verifies the whole checkpoint, then generates actor-aware
interleavings using safe outgoing transitions from each actor's structural state. A candidate is
replayed from a fresh checkpoint and minimized without deleting checkpoint steps. Confirmed repros
are written as `journeys/multi-<id>.yaml`, so the handoff is one command:
`reproit journey multi-<id>`.

### Import existing tests

`reproit import maestro flow.yaml` converts a Maestro flow into a reproit journey (switching cost is
near zero). It maps the common commands, inlines sub-flows, unrolls loops, and prints an import
summary; anything with no faithful equivalent is left as a clearly marked `# TODO` comment rather
than dropped. When `.reproit/map/appmap.json` exists, text-only Maestro taps are resolved through
the observed map if the label matches one unique actionable element; otherwise they stay TODOs until
the app exposes a stable selector.

### Screenshots

`reproit screenshots <tour>` produces store and marketing screenshots by running a journey in
capture mode, fanned across locales and devices. The same journey doubles as a `check` (where
`shoot:` steps just navigate) and as a screenshot run (where they take pictures). Because screens
are locale-invariant, one tour covers every language with no per-locale selectors. See the
[screenshots reference](#screenshots-1).

### Test logins (auth)

Test credentials live in an encrypted local vault, never in the repo or in your journey YAML.

Once an account exists, the normal path is simply:

```sh
reproit auth alice
```

ReproIt maps the unauthenticated UI when needed, recognizes semantic credential and OTP fields
across screen transitions, generates `login-alice.yaml`, and accepts it only after a clean
verification run. The explicit commands below are the advanced/configuration surface.

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

`auth` writes non-secret account metadata under `auth.accounts`, stores provided values in the local
vault. Journeys stay simple: `setup: login(alice)`. Inside the login journey, use `secret:email`,
`secret:phone`, `secret:username`, `secret:password`, `secret:totp`, or `secret:otp`; the host
resolves those values before the runner types them, and redacts them from logs. For stored-session
or API-style accounts, use `setup: auth(admin)` or an actor binding like `{ auth: admin }`.

### Many platforms, many locales

`fuzz` and `check` take cross-cutting flags:

```sh
--target chromium,firefox,webkit   # run each and diff them (finds divergence bugs)
--target ios,android               # same idea across mobile platforms
--locale de,ar,ja                  # fuzz across languages (RTL, overflow, i18n)
--device "iPhone 16 Pro Max"       # otherwise you get an interactive picker
```

## Use it with your AI agent (MCP)

reproit ships no built-in AI. Instead, `reproit mcp` exposes the engine to your coding agent so the
agent can run the loop itself: fuzz, read the repro, fix the code, then `check` to prove it (a green
check is deterministic, so the agent _knows_ it fixed the bug).

Register it once:

```sh
claude mcp add reproit -- /path/to/reproit mcp     # Claude Code
codex mcp add reproit -- /path/to/reproit mcp      # Codex
```

The agent gets tools like `reproit_fuzz`, `reproit_check`, `reproit_accessibility`, and
`reproit_context` (a scoped graph plus the selectors it needs to act). Model maintenance is
automatic and is deliberately not exposed as an agent decision. Authoring, triage, and fixing remain
the agent's job; reproit is the ground truth and verifier. Full tool list in the
[reference](#mcp-tools).

## Cloud

The same `reproit` binary runs on a fleet for the broad, parallel outer loop: fuzzing on every PR,
and ingesting production crashes. The headline use case is reproducing a **real production crash on
your own machine**: the SDK reports the session, and `reproit <bkt_...>` saves and reproduces it
locally.

```sh
reproit login                                          # once: browser sign-in and project selection
reproit bugs                                             # impact-ranked bucket ids
reproit bkt_...                                          # reproduce locally
reproit bkt_... --record-video                           # pull if needed and save video
reproit triage bkt_... fixed --fixed-in-build 1.2.3
reproit resolution-events
```

Login is account-scoped and can run anywhere. Reproduction is execution-scoped: it needs a
`reproit.yaml` that can launch the target. That may be a source checkout, or a URL workspace created
with `reproit init
https://app.example.com`. From elsewhere, pass
`--config /path/to/app/reproit.yaml`. ReproIt downloads the confirmed structural path and failure
signature, then executes them with the configured browser, local runner, auth, device, or simulator.
Bucket replay does not download a source tree or app graph. It executes the saved structural actions
directly; scan and fuzz maintain the discovery graph automatically.

Local is the fast inner loop in your worktree; cloud is the broad outer loop with history. Every
cloud view is backed by exportable raw data.

---

# Reference

## All commands

```
reproit                       help: the scan -> fuzz -> check -> keep story
reproit scan [target]         scan every screen for visible bugs (--record-video for clips)
reproit fuzz [target]         find deeper interaction bugs
reproit <fnd_|rep_|bkt_...>    reproduce one bug
reproit @saved-name            reproduce one saved repro or journey by name
reproit proof <id>             explain its immutable proof ledger
reproit candidates             list candidates with exact promotion blockers
reproit check                  verify the whole saved suite
reproit check --changed [BASE] run mapped repros first, then the complete saved suite
reproit reset                 remove only regenerable local project state
reproit reset --all           remove all local Reproit state after confirmation
reproit reset --all --init    remove all state and initialize the project again
reproit keep [id] [--as name] keep a repro in your suite
reproit create                preserve an immutable human-authored original
reproit create --push         create, browser-review, and push the original
reproit cap_...               show an immutable original capture
reproit cap_... --watch       open its video
reproit push cap_...          review and push it to Cloud
reproit cap_... --open        open its completed Cloud page
reproit create --cloud-tester verify/shrink an SDK-marked Cloud capture
reproit <id> --record-video   annotated video of a repro (--flicker also scans it)
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
reproit login                 connect production telemetry to this account
reproit platforms             UI-framework -> backend matrix
reproit update                verify and install the latest CLI release
reproit debug map ...         advanced internal-model diagnostics
```

### Internal-model diagnostics

Normal commands refresh the graph automatically. These advanced views explain or force its behavior:

- `debug map show`: render the current graph.
- `debug map model`: project the observed graph as an explicitly incomplete, non-authoritative
  state machine with unknown actions.
- `debug map budget`: report guidance-only campaign saturation and a bounded next action budget.
- `debug map suggest-contracts`: emit local draft contracts from verified reversible transitions.
  Drafts never become authority until an application owner reviews and adds them.
- `debug map structural`: force a full crawl.
- `debug map semantic`: an LLM reads your _source_ for the screens that _should_ exist, as a
  worklist (the one optional model call; never an assertion target).
- `debug map coverage`: diffs the screens your code declares against the screens the crawl actually
  verified, so "not fully mapped" becomes a named list.
- `debug map converge`: validates those candidates against the real map and prunes guesses.
- `debug map verify`: re-walks the committed map and reports drift (exit 3).
- `debug map accessibility`: the accessibility audit: which controls a mouse user can operate but a
  keyboard / screen-reader user cannot, per screen, each located by selector and source file:line.
  `--format md` prints an exportable, WCAG-cited report (redirect to a file); `--json` gives the
  structured form; `--baseline <appmap.json>` reports only the gaps NEW vs that baseline and exits 1
  if any appeared (a CI regression gate). See [docs/operability-graph.md](operability-graph.md).

## Flags (on fuzz / check)

See [Oracle reference](oracles.md) for the confirmed default set, specialist detectors, platform
coverage, and `VIOLATION` / `SATISFIED` / `ABSTAIN` semantics.

```
--target ios|android|web|all   multi (a,b,c) -> run each + diff for divergence
--device "<name>"              else an interactive picker (when a TTY)
--locale de,ar,ja              fuzz across locales (RTL / overflow / i18n)
--from <journey>               (fuzz) replay a journey, then explore from its end
--times N                      repeat, to surface flakiness
--only / --no crash,jank,leak  narrow the oracles (default: confirmed set)
--strict                       new repros block instead of starting quarantined
```

`check --changed [BASE]` is a safe ordering optimization. Repros with an exact saved source mapping
to the git diff run first, followed by every other saved repro. If git or mapping evidence is
missing, Reproit runs the normal full suite.

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

Findings are tagged so you can filter with `--only` / `--no`. The default set contains only
categories with authoritative evidence and exact replay.

- **crash** uncaught exceptions / process death
- **jank** dropped frames past a threshold
- **leak** heap growth over a repeating cycle
- **visual** screenshot regression vs a baseline
- **divergence** disagreement between targets (run with multiple `--target`)
- **a11y** accessibility violations
- **overflow** content outside an app-declared layout container in two settled samples. DOM apps
  declare the container with `data-reproit-contain`; scrolling, truncation, transforms, missing
  ownership, and unstable geometry abstain. The shared collector covers web, Electron, Tauri, and
  DOM frameworks such as React, Vue, Svelte, and Angular. Use `--locale` to exercise long and RTL
  strings.

## MCP tools

```
reproit_context(target?)              scoped graph + screens + selectors for a target
reproit_accessibility(state?, kind?)  UI-vs-a11y diff per screen, grounded by selector + file:line
reproit_coverage()                    candidate map from source + coverage ledger + worklist
reproit_scan(target?, record_video?)  default find: state-present bugs, one per (screen x issue)
reproit_fuzz(target?, platform?)      deep sequence bugs (crash/jank/hang); deduped unique-bugs list
reproit_check(repro?, changed?, record_video?, flicker?)  run and classify; changed only reorders
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

`reproit login` persists the account credential and selected project. Bucket ids resolve across
every project that account may access, so reproduction, recording, triage, and timelines never need
an app id.

The full production loop (manage + monitor, not just fix): `reproit_cloud_buckets` (impact-ranked)
-> `reproit_cloud_pull` the top -> `reproit_check` (reproduce) -> fix -> `reproit_check` (verify) ->
`reproit_keep` -> `reproit_cloud_triage` status=fixed --fixed-in-build X (record the fix intent) ->
watch `reproit_cloud_resolution_events` for a regression (prod contradicting the claim).

## Production commands

```
reproit login                       sign in in the browser and select a discovered project
reproit bugs [query]                impact-ranked confirmed production bugs
reproit <bkt_...>                   pull and verify locally
reproit <bkt_...> --record-video    pull if needed and save video of the exact repro
reproit triage <bucket> <status>    update lifecycle state
reproit timeline <bucket>           occurrence history and production resolution
reproit diagnose <report>           match a report to a confirmed bug
reproit resolution-events           recent confirmations and regressions
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
`<out>/<journey>[/<platform>][/<locale>][/<device>]/<name>.png`. The `platform` level appears only
when you fan more than one. For exact control (e.g. the layout `fastlane deliver` expects) set a
`--path-template` with `{journey}` `{platform}` `{locale}` `{device}`. Config lives under
`screenshots:` in `reproit.yaml`; a runnable example tour is at `examples/journeys/marketing.yaml`.
Capture works on every supported platform via that platform's native grab.

## `.reproit/` layout

The local project state is grouped by concept:

```
.reproit/
  map/                  # appmap.json, visits.json, semantic candidates
  runs/                 # raw evidence from scan/fuzz/check runs
  recordings/
    scan/               # quick audit clips from scan --record-video
    repro/              # <id> --record-video videos opened by watch <id>
  captures/             # immutable, private human-authored originals
  repros/               # saved regression guards
  tmp/                  # transient runner scratch
  secrets.vault         # local auth vault
```

`runs/`, `recordings/`, `captures/`, `tmp/`, logs, and vault files are local-only. `repros/` is the
guard suite; `map/` is the learned graph if you choose to review it.

`reproit init` never clears existing state. `reproit reset` removes only `map/`, `runs/`,
`recordings/`, `tmp/`, and `tools/`; it preserves configuration, repros, captures, findings,
capsules, and secrets. `reproit reset --all` removes the complete `.reproit/` directory and the
active project config after an interactive confirmation (or `--yes`). Add `--init` to initialize
again after the full reset. Application source files and journeys are outside both reset scopes.

## Config (reproit.yaml)

Every field supports shell-style environment interpolation: `${VAR}` (empty if unset),
`${VAR:-default}` (fallback), `${VAR:?message}` (required). A minimal, ready-to-copy config for each
platform lives in `examples/configs/`, one file per platform (`reproit.web.yaml`,
`reproit.winui.yaml`, `reproit.tui.yaml`, and so on).

## Background

- **Why screens are identified by structure, not text** (so the graph is locale-invariant and
  survives copy edits): [docs/signature.md](signature.md).
- **The accessibility audit** (the UI-vs-a11y-graph diff):
  [docs/operability-graph.md](operability-graph.md).

reproit ships no bundled LLM. Authoring, triage, and fixing live in your agent over MCP; reproit is
the engine that finds, replays, and verifies.
