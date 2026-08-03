# CLI reference

Eight commands are listed in `--help`. They are the whole loop. Specialist commands exist and run
normally but are unlisted, so a first reader sees the loop instead of the inventory; the last
section names them.

```sh
reproit init            configure this project
reproit capture         preserve a failure you can already point at
reproit find            search for failures you have not seen yet
reproit <id>            reproduce one exact failure
reproit keep <id>       preserve it as a regression guard
reproit check           prove saved failures are still fixed
reproit list            show saved guards
reproit doctor          diagnose setup
reproit login           sign in to Cloud
```

Global options, valid on every command: `--config PATH`, `--json`, `--quiet`, `--yes`.

## init

```sh
reproit init [URL] [--platform flutter|web|rn|android|backend] [--target SERVICE_URL] [--force]
```

Detects the project and writes the smallest configuration that works. It never modifies application
source. A URL argument always selects the web workflow. `--target` points at a running service, and
init then sends one bounded GET per parameterless GET route and records what came back, so the
generated config describes the service as it actually behaves.

## capture

A failure you can already point at, from three sources.

```sh
reproit capture -- cargo test failing_test     # a command that fails
reproit capture --attach --title "menu closes" # a running application
reproit capture --bundle support.rpb           # a signed offline bundle
```

`--timeout-ms` bounds the command (default 300000). `--include-output` keeps bounded stdout and
stderr as local-only restricted evidence. `--local-only` prevents Cloud upload even when
credentials exist. Imported bundle evidence can never supply an executable mechanism.

## find

```sh
reproit find [TARGET] [--quick | --deep | --exhaustive] [--runs N] [--budget N]
```

A fast surface pass, then bounded deep exploration, then exact replay confirmation and
minimization. A finding is reported only when its oracle authority and exact identity survive all
of it. Incomplete coverage stays an explicit blocker rather than an implied clean result.

`--only` and `--no` restrict or exclude detector categories; the default is the stable set.

## Reproduce one failure

```sh
reproit fnd_...        # a local finding
reproit occ_...        # a production occurrence
reproit @saved-name    # a saved guard, by alias
```

`reproit` is the verb, so there is no `run` or `replay` command. The result is one of: the exact
failure reproduced; a clean result; a different failure; a flaky result; stale or unsupported
evidence; or an infrastructure failure. A different failure never counts as a reproduction.

On a TTY the replayed app is held for you to attach a debugger. `--auto` (and any non-TTY, `--json`,
or `--yes` run) reports the verdict and exits.

## keep

```sh
reproit keep fnd_... [--as NAME] [--strict]
reproit keep capture.json [--exec "node server.js"]
reproit keep GUARD --refresh [--yes]
```

Preserves a confirmed failure in the committed suite. The store directory is the repro's content
hash, so it is stable across machines and self-deduping; `--as` adds a human alias. A guard lands
quarantined until its first green run unless `--strict` makes it blocking immediately.

`--exec` is the boot command for a hermetic guard, and defaults to `backend.exec` in reproit.yaml
when set. A capture never supplies a command: only repo-local config does.

`--refresh` re-records a guard whose code has DRIFTED. It boots the guard's own stored recipe with
recording on, fires the recorded trigger, and prints the old-versus-new exchange diff: added calls,
removed calls, or the same calls reordered. Nothing is rewritten without `--yes`, and an
unconfirmed refresh exits 3 having changed nothing. The inbound trigger and the oracle are always
preserved, so a refresh re-records how the operation reaches its dependencies, never what was asked
of it or what counts as failure.

## check

```sh
reproit check                       # the whole saved suite
reproit check capture.json          # re-evaluate one capture offline
reproit check capture.json --exec "node server.js"   # re-execute it
reproit check --changed [BASE]      # changed-first ordering, never a subset
reproit check --junit report.xml    # CI report
reproit check --service a/reproit.yaml --service b/reproit.yaml
```

Exit codes are the contract:

| code | meaning |
| ---: | --- |
| 0 | pass: the exact failure is absent |
| 1 | fail: the exact failure is present |
| 2 | flaky: repeated runs disagree |
| 3 | stale: the case cannot establish its required contract |

Infrastructure failures and different-failure results are reported separately in `--json` output.
Read the `outcome` field, not the exit code alone, when you need to tell them apart.

`--changed` changes execution order only and never skips the rest of the suite. `--strict` makes a
quarantined failure block the exit code. Repeat count and the device matrix come from the `gate:`
section of reproit.yaml, not from flags.

`check <capture.json>` alone re-evaluates the captured events offline. `--exec` re-executes them:
it boots the named command with `REPROIT_REPLAY` pointed at the capture, the SDK serves every
recorded dependency exchange in process, and the verdict comes from the live response. Diverged
(the code no longer makes the captured calls) and inconclusive both fail closed. This needs a
version-2 capture with recorded exchanges, from any of the eight backend SDKs with
`instrument.install()`, and an app that listens on `$PORT`.

## list, doctor, login

```sh
reproit list [--state guards|candidates|bugs] [--query TEXT]
reproit doctor [--json]
reproit login [--cloud URL] [--key KEY]
```

`list` defaults to local guards. `candidates` includes exact blockers; `bugs` lists confirmed
production identities, never unverified telemetry.

`doctor` checks the platform, runner, target, credentials, and native toolchain, and every failed
check carries a repair when Reproit knows a safe one. `--json` carries the same `detail` and `fix`
fields.

`login` stores a credential in the platform credential store and selects a project. Local capture
and checking never require it.

## Unlisted commands

These are real commands, typed the same way; `--help` does not list them so the loop stays legible.
`reproit <name> --help` documents each.

`scan`, `fuzz`, `verify`, `accept`, `baseline`, `proof`, `repro simplify|why`, `journey`,
`screenshots`, `auth`, `import`, `collect`, `inspect`, `surface`, `push`, `create`, `triage`,
`timeline`, `resolution-events`, `skills`, `platforms`, `mcp`, `reset`, `update`, `debug`.

Names beginning `__` are not commands at all. They are process entry points reproit spawns on
itself (runner hosts, the direct-id routes, the update check) and are deliberately untypeable.
