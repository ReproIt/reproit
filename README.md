# reproit

**The deterministic UI fuzzer. Find a bug once, reproduce it forever.**

[**reproit.com**](https://reproit.com) · [docs](docs/cli.md)

![reproit finds a bug and reproduces it every run](docs/demo.gif)

reproit drives your app like a user and finds the bugs your tests never covered,
then hands back a *replayable* repro: a seed plus an exact action sequence that
fails the same way every time. AI maps the UI into a state graph; a deterministic
engine does the finding and the replay. So "cannot reproduce" is gone, and every
bug you find becomes a permanent CI guard.

Three verbs:

```sh
reproit map     # build the app's state graph
reproit fuzz    # find bugs (each saved as a replayable repro)
reproit check   # verify repros: pass / fail / flaky / stale
```

## Every platform

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

Homebrew (macOS and Linux):

```sh
brew install ReproIt/tap/reproit
```

Or build from source with Cargo:

```sh
cargo install --git https://github.com/ReproIt/reproit reproit
```

Web also needs Playwright once: `cd web-runner && npm install && npx playwright install`.

> Published under the [`ReproIt`](https://github.com/ReproIt) GitHub org.

## Quickstart

```sh
cd <your-app>
reproit map            # detect the platform, build the graph
reproit fuzz           # find bugs; each finding prints a content-hash id
reproit check <id>     # confirm a finding replays, before you commit to it
reproit keep <id>      # like it? save it as a regression guard
reproit check          # verify the suite (green after you fix it)
```

A repro is a seed + action sequence, addressed by a content hash, so it's
identical across machines. `check <id>` replays a finding straight from the fuzz
artifact, so you can confirm it's a real bug before `keep` writes it into your
suite. `keep` isn't a git commit, it saves a local, quarantined (non-blocking)
guard; fix the bug and `check` flips it to PASS and promotes it to required.

## Commands

```sh
reproit map [--show]                  # build/refresh the graph; --show renders it
reproit fuzz [target]                 # find bugs (a screen/flow, or the whole app)
reproit check [repro]                 # verify: pass(0) / fail(1) / flaky(2) / stale(3)
reproit keep [id] [--as name]         # save a repro into the suite
reproit repros                        # list saved repros + last status
reproit watch <id>                    # open a repro's recorded video
reproit why [repro]                   # rank suspect code (Ochiai fault localization)
reproit secrets set <k> [v]           # test-login creds for the app under test
reproit mcp                           # serve reproit to your coding agent (stdio)
reproit cloud login|fuzz|findings|blast-radius|reproduce|query
```

Cross-cutting flags on `fuzz`/`check`:

```sh
--target ios,android | chromium,firefox,webkit   # run each + diff for divergence
--device "<name>"     # else an interactive picker
--locale de,ar,ja     # fuzz across locales (RTL / overflow / i18n)
--record              # annotated repro video
--only / --no crash,jank,leak,…
--json --quiet --yes  # CI
```

## How it finds more

Seeded `xorshift` walks over the state graph, fully replayable; `--frontier`
heads for the least-visited state; failures are minimized (ddmin). Screens are
keyed **structurally** (roles + developer keys, text excluded), so the graph is
locale-invariant and doesn't explode on dynamic content. Value-state apps
(calculators, counters) work too: effect detection keeps the walk moving when
only a value changes, and bounded value-classes give them distinct states.
Oracles: crashes, jank, leaks, cross-engine divergence, a11y, i18n.

## Works on AI-built apps

Point reproit at a deployed Lovable / v0 / Bolt / Replit app and it finds and
reproduces the bugs the generator shipped. Your coding agent fixes them; `check`
proves the fix. AI builds it, reproit proves it works.

## Cloud

A worker pool runs the **same `reproit` binary** across shards (one seed/device
each): orchestration, fleet, and storage around the CLI, not a reimplementation.
The headline use case is a **production crash reproduced on your machine**: the
SDK reports the real session and `reproit cloud reproduce <bucket>` replays it
locally. Self-hosted or managed.

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

or add it by hand to `~/.codex/config.toml`:

```toml
[mcp_servers.reproit]
command = "/path/to/reproit"
args = ["mcp"]
```

## License

The runner is source-available under the **Elastic License v2** (use and
self-host freely; not as a hosted service to third parties). The cloud is
proprietary.

---

Internals: `docs/cli.md`, `docs/signature.md`, `SPEC.md`.
