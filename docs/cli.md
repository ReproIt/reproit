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
reproit capture [OPTIONS] [-- COMMAND...]
```

Important forms:

```sh
reproit capture --include-output -- cargo test failing_test
reproit capture --attach --title "menu closes" --record-video
reproit capture --bundle support.rpb
```

`--timeout-ms` bounds command execution. `--include-output` stores bounded stdout and stderr as
restricted local evidence. `--local-only` prevents Cloud upload. Imported evidence cannot supply
an executable mechanism.

## Find unknown failures

```sh
reproit find [TARGET] [--record-video]
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
reproit check [CAPTURE]
reproit check --runs N
reproit check --changed [BASE]
reproit check --junit report.xml
```

`keep` preserves a confirmed case. `check` runs all saved guards unless a single capture reference
is supplied. `--changed` changes execution order only and never skips the rest of the suite.
`--strict` makes quarantined failures block the exit code.

Exit classifications are stable:

- pass: the exact failure is absent;
- fail: the exact failure is present;
- flaky: repeated runs disagree;
- stale: the case cannot establish its required contract.

Infrastructure and different-failure results are reported separately in structured output.

## List current work

```sh
reproit list
reproit list --state candidates
reproit list --state bugs [--query TEXT]
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

## Compatibility surface

Older commands remain hidden compatibility routes while scripts migrate to `capture`, `find`,
`check`, and `list`. They are covered by parser and workflow tests and are not removed until their
observable contracts have equivalents in the core workflows. They are intentionally omitted from
this product reference.
