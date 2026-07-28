# Universal local reproduction contract

Reproit's product objective is to turn every observable software failure into
either an exact local reproduction or a typed, actionable statement of the
missing capability.

Local means developer-controlled and isolated. Execution may use a host process,
container, Compose stack, simulator, emulator, native VM, or dedicated hardware.
It never requires mutating production state.

## Precision and recall

Candidate acquisition maximizes recall. Promotion remains exact.

- A `reproduced` verdict means the original failure identity appeared after a
  successful reset, launch, trigger, and observation.
- `not_reproduced` means the same observation point was reached and the exact
  identity was absent.
- A similar symptom with another identity is `different_failure`.
- Missing evidence, setup failure, infrastructure failure, and unsupported
  capability never become a reproduction or fix.
- Every abstention is retained as a recall blocker rather than discarded.

The measured funnel is:

```text
observed occurrence
  -> sufficiently captured
  -> eligible
  -> locally executed
  -> exactly reproduced
  -> minimized
  -> fixed control verified
  -> retained guard
```

## Source-neutral occurrence

Production telemetry, support bundles, commands, CI, crashes, UI sessions,
requests, messages, migrations, concurrency schedules, and performance
workloads compile into the same immutable occurrence model.

An occurrence contains facts:

- observation and component identity;
- build and environment provenance;
- bounded artifacts and causal events;
- input properties with privacy classification;
- capture defects and missing evidence; and
- typed reproduction requirements.

Evidence cannot supply commands, working directories, environment variables,
timeouts, or cleanup actions.

## Trusted reproduction compiler

The reproduction compiler assesses requirements and binds them to checkout-owned
or built-in trusted providers. Provider digests cover every executable
mechanism. A changed provider invalidates the plan until it is reviewed again.
Automatic compilation succeeds only when the occurrence has one exact identity
and every required phase has exactly one compatible trusted provider. It never
uses names or evidence content to choose between candidates.

Every executable capsule uses the same bounded lifecycle:

```text
validate -> reserve -> reset -> build -> seed -> launch -> readiness
         -> apply environment -> trigger -> observe -> retain -> cleanup
```

Cleanup runs after success, timeout, a different failure, or infrastructure
failure. Output is drained, retained within bounds, and classified separately
from the application verdict.

## Failure identities

Each failure family defines the smallest stable identity that distinguishes the
original defect from a similar symptom:

- crash: process, exception or signal, and relevant stack location;
- backend: operation, actor, resource, response, and persistent effect;
- UI: starting state, trigger, transition, and violated invariant;
- startup: component, lifecycle phase, configuration, and exit identity;
- migration: input schema, starting data, step, and corruption;
- concurrency: actors, ordering constraints, state, and violated invariant;
- performance: workload, environment, metric, and authoritative threshold;
- distributed: topology, messages, dependency state, and injected fault;
- security: principal, authorization state, operation, and prohibited effect;
  and
- device: runtime, permissions, device properties, and trigger.

## Exact minimization and permanent guards

Actions, requests, actors, services, rows, input fields, files, environment
dimensions, timing constraints, and network conditions may be removed only
after a clean replay preserves the exact identity.

The minimized capsule becomes a saved guard. `reproit check` must continue to
reach its observation point and prove that the exact failure remains absent.

## Capability closure

Unsupported occurrences produce typed blockers such as missing reset,
database-effect capture, external protocol emulation, actor scheduling, device
state, or hardware evidence. Reproit aggregates those blockers by affected
occurrence and impact. New adapters reassess retained occurrences so capability
work converts existing recall gaps into local reproductions.
