# Trusted execution providers

Imported evidence may describe what must be reproduced, but it never grants permission to run a
command. Commands come only from `execution.providers` in the checkout-owned `reproit.yaml`.
Repro It validates the catalog before planning or execution and pins each selected provider into the
reproduction plan.

Run `reproit doctor` after editing the catalog. Doctor reports these states separately:

- no project catalog, so automatic local planning must abstain;
- an invalid catalog, with the exact field or boundary that failed;
- a valid catalog with no exact observation matcher;
- a valid catalog, including its phases, source pins, and cleanup coverage.

## Complete example

```yaml
execution:
  version: 1
  cells:
    backend:
      driver: docker-compose
      composeFile: compose.yaml
      applicationService: api
      dependencyServices: [postgres, redis]
      allowLocalBuild: false
      timeoutMs: 60000
      debug:
        debugger: node-inspector
        argv: ["node", "--inspect-brk=0.0.0.0:9229", "dist/server.js"]
        port: 9229
        localSourceRoot: .
        targetSourceRoot: /workspace
  providers:
    reset-postgres:
      authority: trusted-checkout
      phase: reset
      cell: backend
      argv: ["./repro/reset-postgres"]
      timeoutMs: 30000
      cleanExitCodes: [0]
      stateFingerprint:
        argv: ["./repro/fingerprint-postgres"]
        timeoutMs: 30000
        expectedSha256: sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

    launch-service:
      authority: trusted-checkout
      phase: launch
      cell: backend
      argv: ["./repro/launch-service"]
      timeoutMs: 30000
      cleanExitCodes: [0]
      cleanup:
        argv: ["./repro/stop-service"]
        timeoutMs: 30000

    reproduce-null-owner:
      authority: trusted-checkout
      phase: trigger
      cell: backend
      argv: ["./repro/reproduce-null-owner"]
      timeoutMs: 30000
      cleanExitCodes: [0]
      observation:
        identity: "panic:null-owner:src/orders.rs:184"
        kind: stderr-contains
        value: "null owner at src/orders.rs:184"
```

Provider IDs contain only ASCII letters, digits, `.`, `_`, or `-`. A catalog contains at most 256
providers. Each argv contains 1 to 128 entries, each environment contains at most 128 entries, and
each timeout is bounded to 1 through 600000 milliseconds. Working directories and pinned source
files must stay inside the checkout.

## Phases

The supported phases are `validate`, `reserve`, `reset`, `build`, `seed`, `launch`, `readiness`,
`debug`, `trigger`, `observe`, `retain`, and `cleanup`. A requirement binds to exactly one provider
in its required phase. Multiple compatible providers are ambiguous and cause abstention instead of
an arbitrary choice.

Every `reset` and `seed` provider must define `stateFingerprint`. The fingerprint command runs only
after the state-changing command exits cleanly. It must exit zero and write a deterministic,
canonical state representation to stdout. Repro It hashes those exact bytes and compares them with
`expectedSha256`. Timeout, nonzero exit, output truncation, and hash mismatch all fail as
infrastructure errors. A successful reset process without matching state evidence never counts as a
clean starting state.

Long-running services need a bounded launcher that starts the owned process, proves readiness,
records ownership, and exits. A provider command that remains in the foreground until its timeout
is an infrastructure failure. Put the corresponding bounded stop command under `cleanup`. Cleanup
runs in reverse provider order. A launch error, timeout, signal, or nonzero cleanup exit is recorded
and makes the overall result an infrastructure failure, even if the target observation matched.

## Exact observations

One selected provider must decide the verdict using the occurrence's exact identity. Supported
matchers are:

```yaml
observation: { identity: "...", kind: exit-code, code: 17 }
observation: { identity: "...", kind: signal, number: 11 }
observation: { identity: "...", kind: stdout-contains, value: "exact marker" }
observation: { identity: "...", kind: stderr-contains, value: "exact marker" }
observation: { identity: "...", kind: timeout }
```

An observation mismatch with a clean exit proves the failure absent under those conditions. A
different non-clean exit is a different failure. A timeout or provider failure is infrastructure
failure, not evidence that the bug reproduced or disappeared.

## Source pinning

For a checkout script, pin the content digest so a plan cannot silently execute changed code:

```yaml
source:
  path: repro/reproduce-null-owner
  sha256: sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

The path is normalized and checkout-relative. Repro It recomputes the digest before execution.
Changing a pinned mechanism makes the plan stale until it is explicitly refreshed.

Environment values support the same config interpolation as the rest of `reproit.yaml`. Do not
commit secrets. Use the encrypted vault or runtime environment references.

## Reproduction cells

A provider may name one checkout-owned cell. Every provider selected by a plan must either name the
same cell or run on the host. Mixed host and cell plans abstain because their isolation boundary is
ambiguous. A cell plan is compiled with `local-compose` as its execution destination.
The plan binding digest covers the provider, cell definition, and exact Compose file bytes, so a
cell or Compose edit makes an existing plan stale instead of silently changing its mechanism.

The Docker Compose driver requires the Compose file and all bind mounts to stay inside the
checkout. Every selected service must use a digest-pinned image, unless `allowLocalBuild` explicitly
permits a checkout build. Published ports must bind only to loopback. Privileged containers, host
networking, host PID or IPC namespaces, and device mappings are rejected. Repro It adds ownership
labels and an internal network, starts dependencies before reset and seed, starts the application
before readiness, waits for declared Compose health checks within the cell timeout, and tears down
containers, networks, and volumes on every exit path. Cleanup is
successful only when independent Docker label queries find no owned resources.

Every cell run writes a versioned receipt under `.reproit/cells`. It includes the effective Compose
configuration digest, selected services, verified reset and seed fingerprints, and cleanup status.
It does not include the effective environment or other secret-bearing Compose fields.

## Debugging inside the cell

Add a `debug` profile to the cell, then run:

```text
reproit debug occ_0123456789abcdef
```

Repro It starts a fresh cell with the debugger command override, maps its debugger port to a dynamic
loopback port, prints the source mapping, and pauses before the recorded trigger. Attach the
debugger and press Enter to fire the trigger. A VS Code `launch.json` or generic JSON descriptor is
written beside the cell receipt.

Debugger sessions are always diagnostic. Their receipt records the command override, forwarded
port, and attach pause as perturbations, and the run is marked `authoritative: false`. A diagnostic
session cannot claim reproduced, clean, fixed, or flaky. Run `reproit occ_...` normally after a code
change to obtain an authoritative verification result.

## Debugging through any trusted provider

The public command is never selected by framework:

```text
reproit debug occ_0123456789abcdef
```

For a local process, simulator, physical device, or local VM plan, put the debug capability on the
bound trigger provider instead of a Compose cell:

```yaml
execution:
  version: 1
  providers:
    recorded-trigger:
      authority: trusted-checkout
      phase: trigger
      argv: ["./repro/run-trigger"]
      timeoutMs: 60000
      cleanExitCodes: [0]
      debug:
        debugger: language-specific
        argv: ["./repro/run-under-debugger"]
        port: 5678
        localSourceRoot: .
        targetSourceRoot: /workspace
```

The trusted debug command owns the trigger in diagnostic mode. Repro It starts it, requires its
debug endpoint on `127.0.0.1`, opens the IDE session, and releases that same process after
attachment. The authoritative run continues to use the provider's ordinary `argv`. Device and VM
providers may perform their port forwarding inside the trusted debug command. Repro It does not
infer a public endpoint or execute debugger commands supplied by captured evidence.

The provider mechanism is source-neutral. Node Inspector, Chrome DevTools, GDB, LLDB, JDWP, .NET,
and language-specific descriptors are debugger capabilities, not separate Repro It commands. When
an occurrence has no trusted debug capability, `reproit debug explain` identifies that exact gap.
