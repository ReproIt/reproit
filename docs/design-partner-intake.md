# Design-partner intake for source-neutral reproduction

The first conversations should test the failure-to-execution workflow, not broad monitoring demand.
Reproit does not need an installed SDK or a UI-heavy application for this evaluation.

## Qualifying failure

Ask the partner for one recurring failure with:

- a current source checkout and a deterministic build or test command;
- at least one concrete observation, such as an exit code, exception identity, dump signature,
  authored invariant, or stable diagnostic marker;
- bounded evidence they are authorized to analyze;
- a safe non-production execution environment;
- a known cleanup path;
- one engineer other than the founder who will rerun the result.

Good initial cases include service startup, CLI crashes, installers, migrations, scheduled jobs,
background workers, and ordinary backend failures. A mixed Windows client and service remains a
strong second case after the host command vertical.

## Evaluation script

1. Collect or receive an encrypted `.rpb`.
2. Inspect its manifest before decrypting.
3. Import it and review the exact missing requirements.
4. Define one checkout-owned provider and cleanup action.
5. Compile the occurrence with an exact observation identity.
6. Reproduce on the affected revision.
7. Change only application code, not the plan, provider, or observation.
8. Show clean non-reproduction on the candidate revision.
9. Run a positive control.
10. Run `reproit check` and retain the guard.
11. Have the second engineer repeat the run without founder operation.

## Evidence to retain

- original bundle digest and manifest;
- artifact redaction and consent policy;
- imported occurrence, assessment, and capture defects;
- provider digest and checkout revision;
- typed plan and capsule;
- affected-revision exact reproduction;
- candidate-revision non-reproduction;
- positive-control result;
- cleanup result;
- guard result and recurrence history.

## Claims allowed after the evaluation

Allowed:

- the exact occurrence reproduced through the named provider and environment;
- the exact identity did not reproduce on the candidate revision when controls passed;
- the case is retained as a source-neutral guard.

Not allowed:

- arbitrary logs can reconstruct any failure;
- an ephemeral self-signed bundle proves collector identity;
- a host command result proves device, VM, queue, database, or timing hermeticity;
- a different failure is proof of the original failure;
- a missing capability is a pass.
