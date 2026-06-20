# reproit CLI

The canonical spec for the reproit command-line interface.

## Principles

- **Pure commands.** Same input, same behavior. No hidden state branching (no
  "did I init?"). A command's effect never depends on invisible prior runs.
- **map -> fuzz -> check.** Map the app, break it, check it stays fixed.
- **No bundled LLM.** reproit is deterministic and key-free. AI lives in your
  agent (Claude Code, Cursor, ...) via MCP. The bug-finding engine never calls a
  model.
- **Language-independent.** Everything anchors on structure + developer keys,
  never on-screen text. "Welcome" and "Begruessungsbildschirm" are the same node.
- **The output teaches the next command.** Every command ends by printing the
  exact next action as a copy-pasteable command.

## The one object: a `repro`

A *repro* is a saved, replayable case: a seed plus an action sequence, addressed
by a content hash (stable across machines, self-deduping) and an optional human
alias.

- `fuzz` discovers repros (candidates, in an ephemeral artifact) and prints each
  one's content-hash id.
- `check` runs a repro: a saved one (by id/alias) **or a pending fuzz finding by
  id**, so you can confirm a finding replays before you commit to it.
- `keep` saves a repro into your committed suite.

`keep` is not a git commit: it writes a local file under `.reproit/repros/` and
the repro lands **quarantined** (non-blocking) until its first green. So the
order is `fuzz` -> `check <id>` (confirm it's real) -> `keep` (make it a guard)
-> `check` (prove the fix). You never have to save a finding just to look at it.

A bug is a red repro; a fixed bug is a green one. There is no separate "test" vs
"finding" concept.

## The three layers (where the LLM sits)

| Layer                                              | Who               | LLM?      |
|----------------------------------------------------|-------------------|-----------|
| Build the graph (structural, locale-invariant)     | `reproit map`     | no        |
| Target by alias (`fuzz login`)                     | reproit           | no        |
| Name/alias nodes ("login")                         | you, or agent once| optional  |
| "write a test that rejects a bad password" -> acts | your agent (MCP)  | **yes**   |
| fuzz / check / why / reproduce                     | reproit           | no        |

The graph is built deterministically. The LLM only *operates on* it (naming
nodes, authoring tests). Because nodes/elements are keyed by structure, the agent
can take a request in any language and emit actions by stable key that replay in
any locale.

## Language-independence (hard invariant)

reproit must never anchor identity on user-facing localized text. It holds in
three places:

1. **Screen signatures** are built from structure + developer keys, with
   localized text excluded. The same screen in English and German hashes to the
   same node.
2. **Targets** (`fuzz login`) resolve via aliases bound to structural node ids,
   or via structure, never via a string match on display text.
3. **Repro selectors** store "tap the element keyed `submit`", not "tap the
   element labeled 'Anmelden'".

That invariant is about **identity**. **Action selectors** can still address by
visible name: `tap:key:<id>` is exact and locale-proof (the durable choice for
cross-locale journeys or ambiguous labels); `tap:label:<text>` resolves by visible
label (Playwright/Appium-style), works uninstrumented, stable within the run's
locale, so it's the seamless default and keys are the upgrade.

Every runner resolves selectors against **visible/on-screen elements only**:
`role:#<idx>` and positional `#<n>` index visible elements, never one offstage
(another PageView/Tab page, a `display:none` node). Visibility is each runner's
native check, not host-side. Visible text also labels `map show`; locale is a
fuzz dimension (`--locale de,ar,ja`).

## Local commands

```
reproit                       help: the map->fuzz->check story + top commands

reproit map structural        crawl the running app -> verified map (bare `map` = this)
reproit map semantic          LLM read the code -> candidate map
reproit map coverage          diff: screens declared (code) vs verified (crawl)
reproit map converge          validate semantic vs structural, prune
reproit map show              render the map (mermaid | dot | html)
reproit map verify            re-walk, report drift (exit 3)
reproit fuzz [target]         find repros using the map (pure; emits a fuzz artifact)
                              target = an alias/node; all oracles on by default
                              --all: collect every bug, deduped into unique bugs
reproit keep [id] [--as name] save a repro into your suite (interactive if no id)
reproit check [repro|journey] run repros/journeys: pass / fail / flaky / stale  (--record)
reproit screenshots [tour]    store/marketing shots: a journey tour in capture mode
reproit import maestro <f>    convert a Maestro flow into a reproit journey (stdout or -o)
reproit journey list          list authored journeys (declarative YAML paths)
reproit simplify <id> --to .. adopt a shorter, verified-equivalent action sequence
reproit repros                list your saved repros + last status
reproit watch <id>            open a repro's recorded video in your default player
reproit why [repro]           rank suspect code for a failure (spectrum/Ochiai)

reproit secrets set <k> [v]   store a vault secret (also: set-totp, list, remove, test)
reproit mcp                   local stdio MCP server your agent spawns
reproit cloud ...             see below
reproit platforms             UI-framework -> backend matrix (info)
```

### `map`

Two ways to get the app's map, plus views over them. Bare `map` = `map structural`.

- `map structural` — crawl the running app into the **verified** graph (screens by
  signature + the actions between them). Real, but only as deep as it can reach;
  login or empty data gate it, so it's shallow on multi-user apps. Scaffolds the
  repo on first run; if login blocks the first screen it stops and points you at
  `reproit secrets set`.
- `map semantic` — an LLM reads your **source** (routes/nav/API) for the screens
  that *should* exist, into `.reproit/candidate_map.json`, each tagged with a gap
  reason (needs_data / needs_peer / needs_login / frontier). The only model call;
  a worklist, never an assertion target.
- `map coverage` — the diff: screens the code declares (semantic) vs screens the
  crawl verified (structural), with each unverified screen's reason. "Not fully
  mapped" becomes a named list. Candidates join verified screens by route, then name.
- `map converge` — validate the semantic candidates against the structural map,
  promote what's verified, prune source-less guesses, repeat until stable.
- `map show` — render the map (mermaid | dot | html). `map verify` — re-walk and
  report drift (exit 3).

`map structural` drives the app; `map semantic` reads the code.

### journeys

A journey is a declarative YAML path through the app (`journeys/<name>.yaml`), run
with `reproit check <name>`, classified pass/fail/flaky/stale. Author with
`reproit journey save` (or `reproit_journey_save`); list with `reproit journey list`.

Steps, each exactly one of:

- `do: tap:<selector>` / `back` — an action.
- `goto: <screen>` — pathfind the map (single-actor).
- `expect: { state | text | count }` — assert against the live screen.
- `fill: { <selector>: <value> }` — type; a `secret:` value is resolved from the
  vault host-side and redacted from logs (the runner never sees it).

Top-level `setup: login(<acct>)` / `auth(<acct>)` establishes auth;
`tier: headless` opts out of the default sim tier.

**Multi-user.** Add `actors` and tag each step with its actor:

```yaml
actors: { alice: { login: alice }, bob: { login: bob } }
steps:
  - { actor: alice, do: tap:key:testid:post-beacon }
  - { actor: bob,   expect: { text: "alice's beacon" } }   # waits-until-present
```

reproit launches one device per actor and a host **conductor** that hands each its
next action in turn (so A's effect is observable to B), assigning distinct roles
atomically. Multi-user supports `do`/`expect(text|count)`/`fill`, on web and
Flutter.

### `fuzz`

Explores the running app to find bugs: walks the live UI and runs all oracles
(crash/jank/leak/visual/divergence/a11y/i18n), emitting a **fuzz artifact**
(ephemeral, gitignored under `.reproit/fuzz/<run>/`). Each finding gets a
content-hash id to `check` then `keep`; new screens it hits are reported, not
merged into the committed graph.

- `target` reaches an alias/node, then concentrates: mutate inputs (empty, huge,
  emoji, unicode, injection, wrong type), mutate actions (reorder, drop, dup,
  interrupt), and explore locally.
- All oracles on by default; `--only` / `--no` to narrow.
- Each finding prints its **content-hash id** (the same id `keep` would store
  under) plus the two commands it teaches: `reproit check <id>` to confirm it
  replays now, `reproit keep <id> --as <name>` to save it as a guard.
- By default fuzz stops at the first finding (fix it before hunting more).
  **`--all`** keeps hunting across the seed budget and groups findings by crash
  signature (oracle + message + top stack frame) into **unique bugs**, so the
  same bug reached by different paths collapses to one bucket with a canonical
  (shortest) repro. This is the deduped "fuzz and fix" work-list; `reproit mcp`
  passes `--all` so an agent gets every real bug, once, in a single call.

### `check`

Runs a repro and classifies it. The argument is a saved repro (id or alias), a
**pending fuzz finding by id** (replayed straight from the fuzz artifact, before
it is kept), or omitted to run the whole saved suite. Four outcomes:

- **pass**: replayed, green.
- **fail**: replayed, still broken (regression). Exit 1.
- **flaky**: same seed/actions, inconsistent result => the app is
  non-deterministic (a race). Reported with a rate (e.g. 7/10). Exit 2.
- **stale**: the targeted element is gone (UI changed); couldn't replay. Warn,
  re-record. Exit 3.

Stale/fixed/regression are classified against the current `map`, repros are
stored semantically (by key), so a moved button is "stale", not a false "fail".
`--record` produces an annotated video (taps, seed, crash moment).

### `screenshots`

Generate store/marketing screenshots by running a journey **tour** in *capture
mode*, fanned across locales and devices and organized for fastlane.

A tour is just a journey: it declares WHERE to shoot with `do: shoot:<name>`
steps. The command decides WHETHER pictures are taken, so one journey serves two
purposes:

- `reproit check <journey>` runs the steps to verify behavior; `shoot:` steps are
  inert (navigate-only, no pictures, no capture overhead).
- `reproit screenshots <journey>` runs the same steps in capture mode: each
  `shoot:<name>` writes `<name>.png`. Because the state signature is
  locale-invariant, one tour covers every locale with no per-locale selectors.

**Where shots go.** The root is `--out` (or `screenshots.out`, default
`screenshots/`). Under it the layout is *journey-led* and collapses the axes that
do not vary: `<out>/<journey>[/<platform>][/<locale>][/<device>]/<name>.png`. The
`platform` level appears only when you fan more than one platform; `locale` and
`device` only when those are set. So:

- one platform/locale, named device -> `screenshots/<journey>/<device>/<name>.png`
- fan locales -> `screenshots/<journey>/<locale>/<device>/<name>.png`
- fan platforms too -> `screenshots/<journey>/<platform>/<locale>/<device>/...`

For full control (e.g. to emit the exact structure `fastlane deliver` / `supply`
expect), set a `--path-template` / `screenshots.pathTemplate` with the
placeholders `{journey}` `{platform}` `{locale}` `{device}`.

```sh
reproit screenshots [tour]
  --locale de,ar,ja      # fan across locales (RTL / i18n); overrides config
  --target ios,android   # fan across platforms/engines
  --device "a,b"         # fan across devices
  --out screenshots      # output root
  --path-template "{locale}/{device}"   # override the auto layout
  --no-verify            # skip the cross-screen verify gate (on by default)
```

Config (`reproit.yaml`):

```yaml
screenshots:
  tour: marketing            # journey whose shoot: steps name the shots
  out: screenshots           # output root (journey-led subdirs land under it)
  locales: [en, de, ar, ja]
  devices: ["iPhone 16 Pro Max", "iPad Pro 13"]
  verifySignature: true
  # pathTemplate: "{locale}/{device}"   # optional: full control of the layout
```

A runnable example tour lives at `examples/journeys/marketing.yaml`.

Capture works on every supported platform: iOS / Android (simctl / `adb
screencap`), web / Electron / Tauri (Playwright `page.screenshot` / CDP / W3C
WebDriver), macOS / Windows / Linux desktop (window grab via `screencapture` /
`PrintWindow` / ImageMagick), terminal UIs (the vt100 cell grid rendered to PNG),
and Dear ImGui / Clay (an in-app framebuffer-capture hook). The shoot trigger is
uniform across all of them: a tour authors `do: shoot:<name>`.

The v1 verify gate cross-checks that every locale of a given platform/device
produced the same set of shots, so a screen that drifted or was skipped in one
locale fails loudly instead of shipping a gap.

### `import`

Convert a flow from another tool into a reproit journey, so switching costs ~0.
Currently supports **Maestro**: `reproit import maestro flow.yaml` reads the
Maestro YAML and prints a journey (or writes it with `-o`). The common commands
map onto the reproit grammar: `tapOn` -> `tap:label:` / `tap:key:`, `inputText`
-> `type:`, `assertVisible` -> `assert:textPresent:` / `assert:count:`,
`takeScreenshot` -> `do:shoot:`, `back` -> `do:back`. `launchApp` is handled by
reproit and noted; anything without a clean equivalent (scroll, swipe, runFlow)
is emitted as a `# TODO(maestro)` comment so a command is never silently dropped,
and a summary of mapped/TODO/handled counts is printed.

### `keep`

Promotes a repro from the fuzz artifact into the committed suite
(`.reproit/repros/`). Interactive picker if no id. `--as <name>` assigns a human
alias. A kept repro lands **quarantined** (reported, non-blocking) and
auto-promotes to **required** on its first green, unless `--strict`.

Because repros are content-addressed, the same case always keeps to the same id,
so re-keeping is **idempotent**: it reports `already saved as <alias>` and
preserves the existing alias, status, and check history. It never demotes a guard
that already went green. `--as` on a re-keep renames the alias (`already saved;
alias <old> -> <new>`).

### `watch`

`reproit watch <id>` opens a repro's recorded video in your default player. It
looks in the per-id slot (`.reproit/media/<id>.*`) first, otherwise it promotes
the newest recording under `.reproit/runs/` into that slot, so repeat watches are
instant. Produce a recording with `reproit check <id> --record`. `--json` prints
the resolved path instead of launching a player. `.reproit/media/` is gitignored,
so cached recordings are never committed.

### `simplify`

`reproit simplify <id> --to '<actions-json>'` replaces a repro with a shorter,
cleaner action sequence, but only one reproit can **verify** still reproduces the
same finding. A fuzz-found repro can be tangled (a long path that ends on a
positional `role:#idx` selector or post-crash UI), which reads poorly and goes
**stale** after a fix removes that UI. Your agent reads the repro's actions
(`repros` / `reproit_repros`), proposes a minimal equivalent using developer
keys, and reproit replays the candidate: if it reproduces and is no longer, it is
adopted (new content-hash id, the alias and status carried over) and the old one
retired; otherwise it is rejected. The engine verifies, so a simplification can
never be wrong, your agent proposes, reproit disposes.

### `why`

Spectrum-based fault localization (Ochiai). Contrasts coverage of passing vs
failing runs and ranks the code most likely to blame:

```
suspiciousness = ef / sqrt((ef + nf) * (ef + ep))
```

where ef/ep = failing/passing runs that executed a line, nf = failing runs that
did not. Needs both passing and failing runs (fuzz produces them) and coverage
(strongest on instrumented targets). Feeds the agent over MCP as evidence.

### secrets

Test-login credentials live in an encrypted vault (`.reproit/secrets.vault`,
gitignored), never in the repo or the journey YAML.

```
reproit secrets set <key> [value]   store a secret (e.g. alice.password); stdin if no value
reproit secrets set-totp <key> <b32>  store a base32 TOTP seed (2FA / OTP)
reproit secrets list                names only, never values
reproit secrets remove <key>
reproit secrets test <account>      what an account resolves to (env keys + a live TOTP), no password
```

Accounts are declared under `auth.accounts` in `reproit.yaml` (`name`, optional
non-secret `userId`, vault-key refs for password/TOTP/session) and referenced by
`setup: login(<acct>)` / per-actor `login`. Secrets are host-side: reproit
resolves `secret:` values into the action before handing it to any runner and
redacts them from the captured log, so no runner handles a secret and the value
never lands in evidence. A `userId` also lets reset steps clear an account by
reference (`${account.<name>.userId}`).

### Environment interpolation

Every field in `reproit.yaml` (not just `app.defines`) supports shell-style env
interpolation: `${VAR}` (empty if unset), `${VAR:-default}` (fallback), and
`${VAR:?message}` (required, fails to load if unset). Example (note
`webRunnerDir` lives under `app:`):
`app.webRunnerDir: ${REPROIT_WEB_RUNNER_DIR:-./web-runner}`.

A minimal, ready-to-copy config for every supported platform (each with the
correct `app:` fields) lives in `examples/configs/`, one file per platform
(`reproit.web-playwright.yaml`, `reproit.winui.yaml`, `reproit.tui.yaml`, ...).

## Flags

```
--target ios|android|web|all   multi (a,b,c) -> run all + diff for divergence
--device "<name>"              else interactive picker (when a TTY)
--locale de,ar,ja             fuzz across locales (RTL/overflow/i18n bugs)
--record                       annotated video
--times N                      repeats, for flakiness
--only / --no crash,jank,leak  narrow oracles (default: all)
--strict                       check/keep: new repros block instead of quarantine
```

## Globals (every command)

```
--json     machine-readable output (CI, scripts, the MCP bridge)
--quiet    minimal output (CI logs)
--yes      never prompt (non-interactive / CI)
--config   path to reproit.yaml (default: ./reproit.yaml, optional)
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

`fuzz` runs all by default; findings are tagged by type so you can filter.

- **crash**: uncaught exceptions / process death.
- **jank**: dropped frames past a threshold.
- **leak**: heap growth over a repeating cycle.
- **visual**: screenshot regression vs baseline.
- **divergence**: cross-target disagreement (run on multiple `--target`).
- **a11y**: accessibility violations.
- **i18n**: overflow/clip/untranslated/RTL breaks (with `--locale`).

## Cloud commands

Same binary, run on a fleet + production telemetry.

```
reproit cloud login                 service token (distinct from `secrets`)
reproit cloud fuzz [--pr N]         fan-out job -> stored artifact (auto-links to PR)
reproit cloud findings              grouped buckets + counts (fuzz + production)
reproit cloud blast-radius <bucket> who's affected: cohorts, %, versions (--export)
reproit cloud reproduce <bucket>    pull a real user session, replay locally
reproit cloud query ... --export    raw data out for your own analysis
```

Local = the inner loop (fast, focused, your worktree). Cloud = the outer loop
(broad, parallel, multi-platform fleet, CI on PRs, persistent history). The
headline cloud use case: a production crash -> reproduce it deterministically on
your machine.

Every cloud view is backed by exportable raw data; the dashboard is a view, the
data is the asset.

## MCP

`reproit mcp` starts a local stdio MCP server your agent spawns (register once:
`claude mcp add reproit -- reproit mcp`). It bridges local + cloud so the agent
runs the whole loop. Tools:

```
reproit_context(target?)        scoped graph + screens + elements/selectors (from map show)
reproit_map(show?)              build/refresh the graph (show = render existing)
reproit_coverage()              LLM candidate map from source + coverage ledger + pending worklist
reproit_fuzz(target?, platform?)  bug-finding; returns the DEDUPED unique-bugs work-list (--all)
reproit_check(repro?, record?, actions?)  run a saved repro / journey / pending finding / inline candidate
reproit_keep(id?, as?)          save a repro into the committed suite
reproit_simplify(repro, actions)  adopt a shorter, verified-equivalent sequence for a repro
reproit_repros()                list saved repros + last status + each one's actions
reproit_journeys()              list authored journeys (single- and multi-actor)
reproit_journey_save(name, journey)  author journeys/<name>.yaml (incl. multi-user actors)
reproit_why(repro?)             rank suspect code for a failure (Ochiai)
reproit_cloud_buckets(app?, query?)
reproit_cloud_blast_radius(bucket, app?)
reproit_cloud_reproduce(bucket, app?)
```

`author` / `analyze` / `fix` are deliberately NOT tools: the host agent does
authoring, triage and fixing itself (no bundled LLM), using `reproit_context` as
ground truth and `reproit_check` to verify. Cloud tools take the app id from the
`app` arg or `$REPROIT_CLOUD_APP`.

The agent does author/triage/fix using these as ground truth, and verifies its
own work with `check` (deterministic => the agent knows it fixed it).

## End-to-end flow

```
$ reproit map                 -> mapped 47 screens.  Next: reproit fuzz
$ reproit fuzz                -> 3 repros found.      id a3f2c1b8e0d5  confirm: reproit check a3f2c1b8e0d5
$ reproit check a3f2c1b8e0d5  -> fail (3/3).          real bug, reproduced every run
$ reproit keep a3f2c1b8e0d5 --as login-crash  -> saved (quarantined). Verify after the fix: reproit check
$ reproit check               -> 1 passed.           promoted to a required guard

# production loop
prod crash -> reproit cloud findings -> cloud blast-radius <b> -> cloud reproduce <b>
           -> fix -> reproit check (green) -> committed regression guard
```

## Migration from the previous CLI

| Old                       | New                                        |
|---------------------------|--------------------------------------------|
| `init`                    | `map`                                      |
| `doctor`                  | folded into `map` (surfaces missing tools) |
| `map` / `graph`           | `map` / `map show`                       |
| `fuzz`                    | `fuzz` (all oracles, `--target`, scoped)   |
| `run`                     | `check --record`                           |
| `gate`                    | `check`                                    |
| `soak`                    | `fuzz --soak` (leak oracle)                |
| `visual`                  | `check --visual`                           |
| `web-diff`                | `fuzz --target <engines>` (divergence)     |
| `localize`                | `why`                                      |
| `auth`                    | `secrets set`                              |
| `triage find/reproduce`   | `cloud findings` / `cloud reproduce`       |
| `publish`                 | `cloud` (automatic on fuzz)                |
| `comment`                 | removed (auto PR link)                     |
| `author` / `analyze` / `fix` | agent via MCP (deterministic core stays)|

`author`/`analyze`/`fix` move from built-in commands to the agent over MCP, since
reproit ships no bundled LLM. An optional BYO-key escape hatch can restore them in
the bare CLI if `ANTHROPIC_API_KEY` is set (off by default).
