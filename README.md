# reproit

**The deterministic UI fuzzer. Find a bug once, reproduce it forever.**

[**reproit.com**](https://reproit.com) · [docs](docs/cli.md)

![reproit finds a bug and reproduces it every run](docs/demo.gif)

reproit drives your app like a user and finds the bugs your tests never covered,
then hands back a *replayable* repro: a seed plus an exact action sequence that
fails the same way every time. AI maps the UI into a state graph; a deterministic
engine does the finding and the replay. So "cannot reproduce" is gone, and every
bug you find becomes a permanent CI guard.

Three core verbs:

```sh
reproit map     # build the app's state graph
reproit sweep   # find what's wrong: scan every screen for visible bugs
reproit check   # verify repros: pass / fail / flaky / stale
```

(`reproit fuzz` explores deeper for sequence-dependent bugs: crash, jank, hang.)

## Supported platforms

reproit reads whatever surface a framework exposes (accessibility tree, DOM,
semantics tree, a PTY) and computes the **same structural, locale-invariant
signature** everywhere, so the graph, repros, and the production SDKs all agree
byte-for-byte.

| Platform | Backend |
|---|---|
| Web (any framework) | Playwright (Chromium + Firefox + WebKit) |
| Flutter | flutter drive + VM service |
| React Native / native mobile | Appium |
| macOS / Windows / Linux native | AX / UI Automation / AT-SPI |
| Terminal UIs | PTY + VT parser |
| Electron / Tauri | embedded webview |
| Dear ImGui / Clay | in-app instrumentation header |

`reproit platforms` prints the full matrix.

## Install

Install script (macOS and Linux): fetches the binary and provisions the web
runner so `reproit fuzz https://yoursite.com` works with no further setup.

```sh
curl -fsSL https://raw.githubusercontent.com/ReproIt/reproit/main/install.sh | sh
```

Or Homebrew:

```sh
brew install ReproIt/tap/reproit
```

Or build from source with Cargo:

```sh
cargo install --git https://github.com/ReproIt/reproit reproit
```

The web fuzzer needs Node.js 18+. The web runner (Playwright + the headless
browser) auto-provisions on first `reproit fuzz <url>` for every install method,
so there is no manual `npm install` step.

## Quickstart

```sh
cd <your-app>
reproit map            # detect the platform, build the graph
reproit sweep          # scan every screen for visible bugs (fast first pass)
reproit fuzz           # explore deeper for crash / jank / hang bugs
reproit check <id>     # confirm a finding replays, before you commit to it
reproit keep <id>      # like it? save it as a regression guard
reproit check          # verify the suite (green after you fix it)
```

`sweep` checks every screen for the bugs visible on it (overflow, content, a11y,
choice-anomaly); `fuzz` explores action sequences for the bugs that only appear
after the right steps in the right order (crash, jank, hang, leak). Use both.

A repro is a seed + action sequence, addressed by a content hash, so it's
identical across machines. `check <id>` replays a finding straight from the fuzz
artifact, so you can confirm it's a real bug before `keep` writes it into your
suite. `keep` isn't a git commit, it saves a local, quarantined (non-blocking)
guard; fix the bug and `check` flips it to PASS and promotes it to required.

## Commands

```sh
reproit map [--show]                  # build/refresh the graph; --show renders it
reproit sweep [target]                # scan every screen for visible bugs (--record for clips)
reproit fuzz [target]                 # explore deeper for sequence bugs (crash/jank/hang)
reproit check [repro]                 # verify: pass(0) / fail(1) / flaky(2) / stale(3)
reproit record <id>                   # annotated repro video (--flicker also scans it)
reproit baseline [--update]           # visual-regression diff vs the committed baseline
reproit screenshots [tour]            # store/marketing shots: a tour across locales + devices
reproit import maestro <flow.yaml>    # convert a Maestro flow into a reproit journey
reproit keep [id] [--as name]         # save a repro into the suite
reproit repros                        # list saved repros + last status
reproit watch <id>                    # open a repro's recorded video
reproit repro simplify|why <id>       # shorten a repro (verified) / localize the failure
reproit secrets set <k> [v]           # test-login creds for the app under test
reproit mcp                           # serve reproit to your coding agent (stdio)
```

Cloud golden path (production bug -> local repro -> triaged fix):

```sh
reproit cloud login --cloud <url> --key sk_live_...
reproit cloud buckets --app app_...
reproit cloud pull --app app_... --bucket bkt_... --as checkout-crash
reproit check checkout-crash
reproit cloud triage --app app_... --bucket bkt_... --status fixed --fixed-in-build 1.2.3
reproit cloud resolution-events --app app_...
```

Cross-cutting flags on `fuzz`/`check`:

```sh
--target ios,android | chromium,firefox,webkit   # run each + diff for divergence
--device "<name>"     # else an interactive picker
--locale de,ar,ja     # fuzz across locales (RTL / overflow / i18n)
--from <journey>      # (fuzz) replay a journey, then branch outward from its end state
--only / --no crash,jank,leak,…
--json --quiet --yes  # CI
```

`import` + `fuzz --from` is the switch path: convert a Maestro flow to a journey,
then fuzz *from* it. Reaching a valid deep state is the costly part, so an
imported flow becomes a launchpad for the bugs it never covered.

## Works on AI-built apps

Point reproit at a deployed Lovable / v0 / Bolt / Replit app, no config, just the URL:

```sh
reproit fuzz https://your-app.example.com
```

It builds the map, then finds and reproduces the bugs the generator shipped. Your
coding agent fixes them; `check` proves the fix. AI builds it, reproit proves it works.

## Cloud

A worker pool runs the **same `reproit` binary** across shards (one seed/device
each): orchestration, fleet, and storage around the CLI, not a reimplementation.
The headline use case is a **production crash reproduced on your machine**: the
SDK reports the real session and `reproit cloud pull --bucket bkt_... --as <name>`
then `reproit check <name>` replays it locally. Self-hosted or managed.

The SDK captures the *structure* of a session, not user data: input values and
personal data never leave your app (an error attaches only PII-safe derived
features). Details: [docs/data-handling.md](docs/data-handling.md).

## MCP

reproit ships **no bundled LLM**: the core (`map`/`fuzz`/`check`) runs key-free
and offline, and the AI lives in *your* agent. `reproit mcp` exposes the engine
so the agent can loop: fuzz → read the repro → fix → `check` to prove it.

**Claude Code:**

```sh
claude mcp add reproit -- /path/to/reproit mcp
```

**Codex:**

```sh
codex mcp add reproit -- /path/to/reproit mcp
```

**OpenCode:** add to `opencode.json`:

```json
{
  "mcp": {
    "reproit": { "type": "local", "command": ["/path/to/reproit", "mcp"], "enabled": true }
  }
}
```

## License

The runner is source-available under the **Elastic License v2** (use and
self-host freely; not as a hosted service to third parties).

---

Internals: `docs/cli.md`, `docs/signature.md`, `docs/oracles.md`, `docs/operability-graph.md`.
