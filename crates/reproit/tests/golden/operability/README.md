# Operability marker goldens (drift guard)

Each `<platform>.json` here is the **verbatim** `EXPLORE:GROUNDTRUTH` JSON payload that the
platform's in-process operability agent emits for its fixture, copied byte-for-byte from the real
marker. These goldens are the **single source of truth** for the operability contract:

- The engine contract tests read them, not inline literals (`crates/reproit/src/domain/map.rs`,
  `gaps_from_golden(...)` / `golden_groundtruth(...)`). Each
  `*_in_process_agent_groundtruth_detects_fake_button_gap` test parses the golden through the real
  engine and asserts the same `pointer_only` / `keyboard_unreachable` / `no_role` gap
  classifications.
- A per-platform **capture-diff** step re-runs the real agent, canonicalizes the live marker
  (JSON-parse, sort keys, **drop the volatile `sig`**), and diffs it against the golden. If they
  diverge, the job fails and names the platform plus the changed field. This is what makes a stale
  golden / drifted agent impossible to miss: the bug we hit was a green WPF contract test whose
  inline literal had drifted from the real marker. With the golden inverted, the test asserts the
  contract while the diff catches drift against production.

`canonicalize-diff.mjs` is the canonicalize+diff tool (no deps, plain Node):

```sh
# compare a captured marker (agent stdout, or bare JSON) against the golden
node canonicalize-diff.mjs <web|appkit|wpf|qt|gtk|flutter> <liveMarkerFile|->
```

It drops the top-level `sig` (a structural hash that legitimately changes across toolchain/layout
versions) and treats any other change (element add/remove, an a11y dimension flipping, `gestureKind`
renamed, ...) as real drift.

## What is LIVE in CI vs documented for manual re-capture

Honest status. Only toolchains present in CI can be re-captured automatically.

| platform | golden source                      | CI re-capture?  | how                                                                                                                       |
| -------- | ---------------------------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------- |
| flutter  | `flutter test` operability fixture | **YES, live**   | `capture-flutter.sh` runs the real agent under `flutter test` and diffs (the flutter job already installs the toolchain). |
| web      | engine motivating-case marker      | NO (documented) | see below                                                                                                                 |
| appkit   | built+run Swift agent (macOS)      | NO (documented) | no macOS+a11y agent run in CI                                                                                             |
| wpf      | built+run .NET agent (Windows VM)  | NO (documented) | no Windows VM in CI                                                                                                       |
| qt       | built+run C++ agent (Linux)        | NO (documented) | Qt/offscreen Linux run not wired in CI yet                                                                                |
| gtk      | built+run C agent (Linux/xvfb)     | NO (documented) | GTK4/xvfb run not wired in CI yet                                                                                         |

### web

The web golden (`web.json`, `sig:"abc"`) is the engine's **motivating-case** marker (the
`role:option#0` finding-div), not a verbatim capture of a single web fixture, and the CI
`web-runner` job runs `node --test` **without** installing Chromium
(`runners/web/groundtruth-taps.test.mjs` skips cleanly when no browser is present). So there is no
live web capture-diff today; flagging it honestly rather than wiring a green-but-meaningless step.
To re-capture manually with a browser present, drive `runners/web/runner.mjs`'s `snapshot` +
groundtruth emitter against a fixture under real Chromium and diff the emitted line with
`canonicalize-diff.mjs web <file>`.

### Manual / periodic re-capture (appkit, wpf, qt, gtk)

These agents need a toolchain not in CI. When you touch an agent (or on a periodic cadence),
re-capture and diff on the machine that has the toolchain:

- **appkit** (macOS): build + run `runners/native/appkit-agent` (`build-and-run.sh`), capture its
  `EXPLORE:GROUNDTRUTH` line, then `node canonicalize-diff.mjs appkit <captured-file>`.
- **wpf** (Windows VM, see memory `winvm-dotnet-validation`): build + run
  `runners/native/wpf-agent`, capture the line, `... canonicalize-diff.mjs wpf <file>`.
- **qt** (Linux + Qt 6, `QT_QPA_PLATFORM=offscreen`): build + run
  `runners/native/qt-agent/qt_agent.cpp`, capture, `... canonicalize-diff.mjs qt <file>`.
- **gtk** (Linux + GTK4 under `xvfb-run`): build + run `runners/native/gtk-agent/gtk_agent.c`,
  capture, `... canonicalize-diff.mjs gtk <file>`.

If the diff fails, the agent has drifted: confirm whether the contract still holds, then update the
golden here AND the matching assertions in `crates/reproit/src/domain/map.rs` in the same change.
