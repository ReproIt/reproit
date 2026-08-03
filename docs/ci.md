# ReproIt in CI

Two separate jobs, and they are worth keeping separate: a **gate** that proves saved failures are
still fixed, and **capture** that turns a red build into something you can run on a laptop.

## The gate

```yaml
- run: reproit check --junit reports/reproit.xml
```

`check` runs every saved guard and exits with the suite's worst outcome:

| code | meaning | what CI should do |
| ---: | --- | --- |
| 0 | pass | proceed |
| 1 | fail: a guarded failure is back | block |
| 2 | flaky: repeated runs disagree | block, and treat as a real signal |
| 3 | stale: a case cannot establish its contract | block; the evidence needs re-recording |

Do not branch on the exit code alone when you need detail. `--json` carries `outcome`, and
infrastructure failures and different-failure results are reported there rather than folded into a
verdict.

Repeat count and the device matrix come from the `gate:` section of reproit.yaml, so the CI step
stays one line and the policy stays reviewable in the repo.

A new guard lands **quarantined**: it runs and reports but does not block until its first green
run. That is what stops a freshly recorded guard from breaking the build on the commit that adds
it. `--strict` makes quarantined failures block too. `keep --strict` skips quarantine for a guard
you want blocking immediately.

### Faster feedback without a smaller suite

```yaml
- run: reproit check --changed ${{ github.event.pull_request.base.sha }}
```

`--changed` runs guards connected to the changed files first, then the rest of the suite. It
changes order only. It never skips an unmapped guard, so a passing run still means the whole suite
passed.

### More than one service in a repo

```yaml
- run: reproit check --service api/reproit.yaml --service worker/reproit.yaml
```

One step, non-zero if any of them fails. Chaining N steps hides which one failed behind the first
red.

### Backend baseline

`check --update-baseline` records the current findings as accepted and exits 0, so later runs block
only on new or regressed findings. Use it once, deliberately, when adopting the gate on an existing
service. To accept one specific finding instead, `reproit accept <id> --reason "..." --until
YYYY-MM-DD` keeps everything else blocking and lapses on the date rather than staying silent
forever.

## Capture from a red build

A failing test can spool a replayable capsule as a job artifact, with no cloud involved:

```yaml
- uses: reproit/reproit@v1
  with:
    test-command: node tests/checkout.test.mjs
```

The developer downloads the artifact and runs it on their own machine:

```sh
reproit check capsule.json --exec "node tests/checkout.test.mjs"
```

This boots nothing, re-runs only the named test with the recorded exchanges served in process, and
verdicts reproduced, fixed, diverged, or inconclusive. Two limits worth stating before you rely on
it: a plain rerun that passes *outside* the capsule is flaky, envelope-dependent evidence and is
never reported as fixed, and a race the replay boundary cannot see (scheduling, shared memory)
reports inconclusive rather than a faked reproduction. Node's `node:test` runner today; jest is not
integrated.

The action's other mode fuzzes a pull request and comments with a minimized repro. Both modes,
with every input, are documented in [action.README.md](../action.README.md).

## Credentials

The gate needs none. `check` and `capture` are local operations, and a repo with committed guards
gates itself on a fork PR with no secrets available.

Cloud upload needs `REPROIT_CLOUD_KEY`. Keep it out of workflows that run untrusted pull requests.
