# CLI reference

The public CLI is organized around outcomes, not backend mechanisms.

## Configure and diagnose

```sh
reproit init [URL] [--platform PLATFORM]
reproit doctor
```

`init` detects the project and writes the smallest usable configuration. It does not modify
application source. `doctor` checks the selected platform, runner, target, credentials, and native
toolchain. Every failed check includes a repair when Reproit knows a safe one. JSON output includes
the same `detail` and `fix` fields:

```sh
reproit --json doctor
```

## Capture a known failure

```sh
reproit internal capture [OPTIONS] [-- COMMAND...]
```

Important forms:

```sh
reproit internal capture --include-output -- cargo test failing_test
reproit internal capture --attach --title "menu closes" --record-video
reproit internal capture --bundle support.rpb
```

`--timeout-ms` bounds command execution. `--include-output` stores bounded stdout and stderr as
restricted local evidence. `--local-only` prevents Cloud upload. Imported evidence cannot supply
an executable mechanism.

## Find unknown failures

```sh
reproit find [TARGET]
```

`find` runs a fast surface pass, then bounded deep exploration, exact replay confirmation, and
minimization. A finding is emitted only when its oracle authority and exact identity are preserved.
Incomplete coverage and unsupported capabilities remain explicit blockers.

## Reproduce one case

```sh
reproit fnd_...
reproit occ_...
reproit @saved-name
```

The result distinguishes:

- exact failure reproduced;
- clean result;
- a different failure;
- flaky result;
- stale or unsupported evidence;
- infrastructure failure.

A different failure never counts as a reproduction.

## Guard a fix

```sh
reproit keep fnd_... [--as NAME]
reproit keep capture.json [--exec "node server.js"]   # hermetic guard
reproit keep GUARD --refresh [--yes]                  # re-record a drifted guard
reproit check [CAPTURE]
reproit check [CAPTURE] --exec "node server.js"
reproit check --changed [BASE]   # repeat count and device matrix come from reproit.yaml `gate:`
reproit check --junit report.xml
```

`keep` preserves a confirmed case. `check` runs all saved guards unless a single capture reference
is supplied. `--changed` changes execution order only and never skips the rest of the suite.
`--strict` makes quarantined failures block the exit code.

`check <capture.json>` alone re-evaluates the captured backend events offline.
`--exec` (preview) re-executes them instead: it boots the named command with
`REPROIT_REPLAY` pointed at the capture, the SDK serves every recorded
dependency exchange in process, and the verdict comes from the live response:
reproduced, fixed, diverged (the code no longer makes the captured calls), or
inconclusive. Diverged and inconclusive fail closed. This needs a version-2
capture with recorded exchanges, currently produced by the Node backend SDK
with `instrument.install()`, and an app that listens on `$PORT`.

`--exec` is optional when the project sets `backend.exec` in reproit.yaml;
`reproit init` records it whenever it can infer the boot command, and the flag
remains the override. A capture never supplies a command: only repo-local
config does.

`keep <guard> --refresh` re-records a guard whose code has DRIFTED (a diverged
verdict). It boots the guard's own stored recipe with recording on and replay
off, fires the guard's recorded inbound trigger, and prints the old-versus-new
exchange diff: added calls, removed calls, or the same calls reordered. Nothing
is rewritten without `--yes`, and an unconfirmed refresh exits 3 having changed
nothing. A refresh preserves the inbound trigger and the oracle, so it
re-records how the operation reaches its dependencies, never what was asked of
it or what counts as failure.

Exit classifications are stable:

- pass: the exact failure is absent;
- fail: the exact failure is present;
- flaky: repeated runs disagree;
- stale: the case cannot establish its required contract.

Infrastructure and different-failure results are reported separately in structured output.

## List current work

```sh
reproit internal list
reproit internal list --state candidates
reproit internal list --state bugs [--query TEXT]
```

The default lists local guards. Candidates include exact blockers. Bugs lists confirmed production
identities, not unverified telemetry.

## Account selection

```sh
reproit login
```

Login stores an account credential in the platform credential store and selects a project.
Local-only capture and checking do not require login.

## Global options

```text
--config PATH   select reproit.yaml
--json          machine-readable output
--quiet         suppress human output
--yes           disable prompts for CI
```
