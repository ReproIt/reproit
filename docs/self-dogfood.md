# Self-dogfood policy

Reproit reproduces, proves, and retains its own defects with Reproit. This
document is the enforceable version of that rule. The gate is
`validation/self-dogfood/check-fix-policy.py`, run by the `dogfood-policy` CI
job on every pull request and direct push to `main`.

## The rule

> Every confirmed Reproit defect enters the reproduction funnel. Every eligible
> defect must become an exact Reproit reproduction and a committed
> `reproit check` guard before it is considered closed.

"Eligible" is load-bearing. Some failures need a capability Reproit does not
implement yet. Those produce a typed record and a named capability gap. They
are backlog inputs, not passing guards, and they are never described as
reproduced.

## Controller and subject are separate

Every self-dogfood execution has two explicit versions:

- **controller**: a pinned known-good Reproit build that captures, executes,
  replays, and evaluates;
- **subject**: the checkout, binary, runner, SDK, service, or deployment being
  tested.

The controller must never silently resolve to the subject binary. A case whose
controller artifact digest equals either subject artifact digest is rejected.

When the defect is in the controller's own evaluation logic, the fixed
candidate must pass both the Reproit replay and an independent authority: an
existing failing test, an upstream specification, an externally observed exit
status or database state, a prior known-good release, or a small verifier that
does not reuse the affected evaluator.

## The declaration every fix carries

A commit that changes production source must carry exactly one
`Reproit-Dogfood:` trailer. Production source includes runtime and SDK source,
runners, scripts, build manifests, package locks, and GitHub Actions workflows.
Documentation and retained evidence do not need a declaration.

| Declaration | Meaning | What the gate checks |
| --- | --- | --- |
| `guard:rep_<12 hex>` | A committed guard proves affected versus fixed | `.reproit/repros/<id>/meta.json` exists in that commit's tree with `status: required` |
| `exception:<code>:<id>` | A typed eligibility exception | The exception record validates and every retained artifact exists with its declared SHA-256 digest |
| `no-repro:<id>` | No stable Reproit reproduction is practical | The record, test, affected evidence, command, and timeout validate; the test fails with the declared result on the parent and passes on the fix |
| `not-a-fix:<id>` | The change is not a bug fix | A changed typed record explains the change and binds at least one changed evidence artifact by SHA-256 |

A missing trailer fails the gate. Two trailers fail the gate. There is no
implicit exception.

Example:

```text
Reject a divergent migration ledger

Reproit-Dogfood: guard:rep_40f619ef4a2c
```

### Typed blocker codes

`incomplete-evidence`, `unsupported-capability`, `environment-unreachable`,
`unsafe-to-execute`, `authority-missing`, `flaky-within-budget`.

An exception record is:

```json
{
  "schemaVersion": 1,
  "id": "vendor-sdk-offline",
  "code": "unsupported-capability",
  "detail": "the vendor SDK installer requires network access at build time",
  "issue": "DOGFOOD-042",
  "missingCapability": "offline vendor SDK acquisition",
  "retainedEvidence": [
    {
      "path": "validation/self-dogfood/evidence/vendor-sdk-offline.log",
      "sha256": "sha256:<64 lowercase hex>"
    }
  ]
}
```

`detail`, `issue`, `missingCapability`, and at least one retained evidence
artifact are all required. Each evidence path must exist in the declared commit
and match its digest. An exception that names no missing capability is an
omission, not an exception.

### Independent-test records

`no-repro:<id>` resolves to
`validation/self-dogfood/no-repro/<id>.json`. The record declares a changed
test path, a shell-free command argument array, a bounded timeout, the exact
nonzero exit expected from the parent revision, and digest-bound affected
evidence. CI overlays the new regression test onto the parent checkout, proves
the declared failure, then proves the same command passes on the fixed commit.

### Non-fix records

`not-a-fix:<id>` resolves to
`validation/self-dogfood/not-a-fix/<id>.json`. It must classify the change as
`feature`, `maintenance`, `refactor`, or `tooling`, explain why it is not a bug
fix, and bind at least one artifact changed by the same commit. A reusable
blanket exemption is invalid.

## Guards may not be weakened to make CI green

The gate compares the required guard corpus between the base and the head
commit. A guard that disappears, or whose `trigger_sig` changes, is a weakening
and fails the gate.

The only way through is an explicit, recorded retirement:

1. add `Reproit-Guard-Retire: rep_<id>` to the commit message;
2. commit `validation/self-dogfood/retirements/rep_<id>.json` with
   `schemaVersion`, `guard`, `reason`, and a different required replacement
   guard.

Deleting a failing guard, quarantining it, or editing its trigger signature so
it stops matching are all the same act and are all blocked. Fix the code or
record the retirement. Arbitrary replacement prose is rejected; the named
replacement must be present in the required corpus and therefore replay in CI.

## Guard execution order in CI

Changed guards run first for fast feedback. The complete required corpus runs
before merge and again on direct pushes. The corpus runner validates every
committed guard directory, selects required guards, and replays each explicitly
under `--strict` with three runs.

```sh
target/debug/reproit --json --yes check self-dogfood-cli-backend-root --strict
python3 validation/self-dogfood/run-required-guards.py target/debug/reproit
```

A guard passes only under the `exact-observation-v1` contract: every run
reaches the named observation point, launches cleanly, and produces zero
exact-identity matches. A different crash, a missing observation, a setup
failure, or a controller failure is not a pass and is not a reproduction.

## Where guards live

Each repository owns the guards for its own defects, in the single
content-addressed store `.reproit/repros/<content-id>/`. A committed guard
carries `capsule-id`, `meta.json`, `package.json`, `plan.json`,
`providers.yaml`, and `replay.json`. Generated run evidence under `plan-runs/`
and every private capture input are machine-local and git-ignored: a guard must
replay from a fresh checkout without them.

`reproit-proof` owns sanitized public evidence, never the executable source of
truth.

## Current corpus

| Case | Identity | Guard | Repository |
| --- | --- | --- | --- |
| `dog_cli_backend_root` | `doctor:blank-backend-project-root` | `rep_b1ab0f0eb617` | reproit-cli |
| `dog_cli_required_corpus_dispatch` | `ci:required-guard-corpus-dispatch` | `rep_6bc2f97d73a7` | reproit-cli |
| `dog_cli_direct_push_policy` | `ci:direct-push-dogfood-policy` | `rep_cf7b6f962595` | reproit-cli |
| `dog_cloud_migration_history` | `cloud:migration-history-divergence` | `rep_40f619ef4a2c` | reproit-cloud |

The CLI CI-enforcement guards bind the affected and fixed commits, controller
and verifier digests, three affected reproductions, and three reached-observation
fixed controls in
`validation/self-dogfood/evidence/ci-enforcement-qualification.json`.

The original CLI and Cloud cases carry three affected reproductions, three
reached-observation fixed controls, and verified digests for every declared
artifact. Those digests are re-checked in this repository by:

```sh
python3 validation/self-dogfood/run-required-guards.py target/debug/reproit
```
