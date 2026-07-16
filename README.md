# reproit

**Find a UI bug once, reproduce it forever.**

[**reproit.com**](https://reproit.com) · [docs](docs/cli.md)

![reproit finds a bug and reproduces it every run](docs/demo.gif)

reproit drives your app like a user, finds bugs your tests missed, and gives you
a replayable repro: the exact steps needed to make the bug happen again. That
turns "cannot reproduce" into a local command you can run before and after the
fix.

The small loop is:

```sh
reproit init           # detect the app and create the smallest working setup
reproit doctor         # check local setup for this app and platform
reproit auth <account> --email ... --password ...  # configure + verify once
reproit fuzz --all     # find deeper, confirmed interaction bugs
reproit fnd_...        # reproduce that finding
reproit keep fnd_...   # keep it as a regression guard
reproit check          # run the saved suite after the fix
```

`reproit scan` audits visible screen-level problems. `reproit fuzz` explores
deeper action sequences. Its default detectors have direct replay predicates: crash, broken
content, hang, broken route, blank screen, and broken asset. Specialist and
experimental detectors remain available explicitly with `--only`.

Reproit maintains its internal screen graph automatically. Before a command
uses it, reproit fingerprints the actual source, configuration, lockfiles, and
CLI version; changed inputs trigger a refresh. Git revision and dirty state are
stored as provenance, but uncommitted work is handled correctly too.

## Supported platforms

reproit uses the same workflow across web, mobile, desktop, terminal, and
instrumented native UI. Each platform has a live backend, and the saved repros
stay portable enough for local runs, CI, and production crash replay.

| Platform | Backend |
|---|---|
| Web (DOM apps) | Playwright Chromium, Firefox, and WebKit |
| Flutter | flutter drive + VM service |
| React Native / native mobile | Appium |
| macOS native | AX (validated with SwiftUI) |
| Windows native | UI Automation (validated with WPF, Avalonia, WinUI 3) |
| Linux native | AT-SPI (validated with GTK, Qt Widgets, Qt Quick/QML, wxWidgets) |
| Terminal UIs | PTY + VT parser |
| Electron | Chromium/CDP |
| Tauri | system WebKit webview through `tauri-driver` |
| Dear ImGui / Clay | in-app instrumentation header |

`reproit platforms` prints the routing matrix. The exact native fixtures,
commands, and pass contract are documented in
[`validation/backends/README.md`](validation/backends/README.md); registered
platform ids are checked against that evidence manifest in the Rust test suite.

## Install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/ReproIt/reproit/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/ReproIt/reproit/main/install.ps1 | iex
```

Or build from source:

```sh
cargo install --git https://github.com/ReproIt/reproit --locked reproit
```

Once installed, update explicitly with:

```sh
reproit update
```

ReproIt checks for a newer release without delaying commands and shows a cached
notice at most once per day. It never replaces the CLI without this command.
Set `REPROIT_NO_UPDATE_CHECK=1` to disable the check. Checks are disabled in CI.

The web runner needs Node.js 18+. Playwright and the headless browser are
provisioned on first web run, so there is no manual `npm install` step.

## Quickstart

```sh
cd <your-app>
reproit doctor                         # see missing platform setup before the run
reproit auth <account> --email ... --password ...  # optional logged-in flows
reproit scan --record                  # fast visible-bug audit + clips
reproit fuzz --all                     # find confirmed bugs with fnd_... ids
reproit fnd_...                        # reproduce that finding
reproit keep fnd_...                   # keep it as a regression guard
reproit check                          # verify the suite after the fix
```

Use the same flow for every platform in the table above. The target changes
(`https://...`, simulator/device, native app, terminal command, or instrumented
binary), but the loop stays `doctor`, optional `auth`, `scan`, `fuzz`, direct
bug id, `keep`, then `check` again after the fix.

`scan` checks each reachable screen for visible problems like overflow, broken
content, missing labels, and odd layout choices. `--record` turns boxable scan
findings into clips. `fuzz` explores longer action sequences and emits the
replayable `fnd_...` findings you can run directly and `keep`.

A finding is not useful until it replays. `reproit <id>` proves that the bug still
happens on your machine. `keep <id>` saves the repro locally as a non-blocking
guard; once you fix the bug, `check` flips it to PASS and makes it part of the
required suite.

There are two recording paths on purpose:

- `scan --record` is an audit convenience: after scan finds visible, boxable
  issues, it saves one short clip per issue into `.reproit/recordings/scan/`.
- `record <id>` is repro evidence: after `fuzz` prints an `fnd_...` id, or after
  you keep a repro, it replays that exact bug once and saves the annotated video
  that `watch <id>` opens later.

`.reproit/` is organized by concept: `map/` is the internal versioned app model,
`runs/` is raw local evidence, `recordings/` is generated video, `tmp/` is
ignored runner scratch, and `repros/` is the saved regression suite.

## Commands

```sh
reproit doctor                        # check app, platform, runner, and auth setup
reproit scan [target]                 # scan every screen for visible bugs (--record for clips)
reproit fuzz [target]                 # find deeper interaction bugs
reproit <fnd_|rep_|bkt_...>           # reproduce one bug
reproit check                         # verify the whole saved suite
reproit record <id>                   # annotated repro video (--flicker also scans it)
reproit baseline [--update]           # visual-regression diff vs the committed baseline
reproit screenshots [tour]            # store/marketing shots: a tour across locales + devices
reproit import maestro <flow.yaml>    # convert a Maestro flow into a reproit journey
reproit keep [id] [--as name]         # keep a repro in the suite
reproit repros                        # list saved repros + last status
reproit bugs [query]                  # impact-ranked confirmed production bugs
reproit debug map show                # advanced: inspect the internal app model
reproit triage <bkt_...> fixed        # record the fix intent
reproit watch <id>                    # open a repro's recorded video
reproit repro simplify|why <id>       # shorten a repro (verified) / localize the failure
reproit auth <account>                # configure/discover/verify a test login
reproit mcp                           # serve reproit to your coding agent (stdio)
```

Cloud golden path (production bug -> local repro -> triaged fix):

```sh
reproit login                                       # once: browser sign-in and project selection
reproit bugs
reproit bkt_...
reproit record bkt_...
reproit triage bkt_... fixed --fixed-in-build 1.2.3
reproit resolution-events
```

`reproit login` can run from any directory. It stores an account credential and
lets bucket ids resolve across every project you can access. A bucket needs a
runnable app configuration, not necessarily source. For a deployed web app,
create a small workspace with `reproit init https://app.example.com`, then run
`reproit bkt_...` there. To reproduce against current local code, run it inside
the app checkout. From another directory, pass
`--config /path/to/app/reproit.yaml`.

Cloud does not upload or clone source code. A URL-backed web configuration owns
the deployed target and browser runner. A source-backed web, Flutter, iOS,
Android, or desktop configuration owns the build command, runtime, simulator or
device, and auth. Cloud supplies the confirmed production actions, failure
signature, safe fixture properties, and replay capsule when one exists. A
bucket replay executes those actions directly; it does not download a source
tree or a second app graph. Scan and fuzz maintain the local app model
automatically for discovery.

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

## Web apps

Point reproit at a running web app. A URL initializes the UI workflow by default:

```sh
reproit init https://your-app.example.com
reproit scan
reproit fuzz
```

`scan` checks the current UI. `fuzz` explores deeper interactions and saves every
confirmed finding for exact replay with `reproit fnd_...`.

For A2UI-generated interfaces, pass the generated JSON or JSONL stream directly:

```sh
reproit scan generated-ui.jsonl
reproit fuzz generated-ui.jsonl
reproit fnd_...
```

Reproit validates the official v0.9 basic catalog, runs the stream through the
official React and Lit renderers, minimizes a failure while preserving its exact
signature, and stores the result under the same `fnd_...` workflow.

## Cloud

A worker pool runs the **same `reproit` binary** across shards (one seed/device
each): orchestration, fleet, and storage around the CLI, not a reimplementation.
The headline use case is a **production crash reproduced on your machine**: the
SDK reports the real session and `reproit bkt_...` reproduces it locally.
Self-hosted or managed.

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

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

---

Internals: `docs/cli.md`, `docs/signature.md`, `docs/oracles.md`, `docs/operability-graph.md`.
