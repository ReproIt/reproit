# Self-dogfood policy

Reproit reproduces, proves, and retains its own defects with Reproit. This
document is the enforceable version of that rule. The gate is
`validation/self-dogfood/check-fix-policy.py`, run by the `dogfood-policy` CI
job on every pull request.

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

The controller must never silently resolve to the subject binary. The schema in
`reproit-lab/src/self-dogfood.mjs` rejects a case whose controller artifact
digest equals either subject artifact digest.

When the defect is in the controller's own evaluation logic, the fixed
candidate must pass both the Reproit replay and an independent authority: an
existing failing test, an upstream specification, an externally observed exit
status or database state, a prior known-good release, or a small verifier that
does not reuse the affected evaluator.

## The declaration every fix carries

A commit that changes production source must carry exactly one
`Reproit-Dogfood:` trailer. Production source means a file under `crates/`,
`src/`, `sdk/`, `runners/`, or `scripts/` with a source extension. Docs,
workflows, and retained evidence do not need a declaration.

| Declaration | Meaning | What the gate checks |
| --- | --- | --- |
| `guard:rep_<12 hex>` | A committed guard proves affected versus fixed | `.reproit/repros/<id>/meta.json` exists in that commit's tree with `status: required` |
| `exception:<code>:<id>` | A typed eligibility exception | `validation/self-dogfood/exceptions/<id>.json` exists and validates; `<code>` is one of the six typed blocker codes |
| `no-repro:<test path>` | No stable automated reproduction is practical | The named independent regression test exists and is changed by the same commit |
| `not-a-fix` | The change is not a bug fix | Nothing further; the point is that the answer is explicit |

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
  "retainedEvidence": ["validation/self-dogfood/evidence/vendor-sdk-offline.log"]
}
```

`detail`, `issue`, `missingCapability`, and at least one retained evidence path
are all required. An exception that names no missing capability is not an
exception, it is an omission.

## Guards may not be weakened to make CI green

The gate compares the required guard corpus between the base and the head
commit. A guard that disappears, or whose `trigger_sig` changes, is a weakening
and fails the gate.

The only way through is an explicit, recorded retirement:

1. add `Reproit-Guard-Retire: rep_<id>` to the commit message;
2. commit `validation/self-dogfood/retirements/rep_<id>.json` with
   `schemaVersion`, `guard`, `reason`, and `replacement`.

Deleting a failing guard, quarantining it, or editing its trigger signature so
it stops matching are all the same act and are all blocked. Fix the code or
record the retirement.

## Guard execution order in CI

Changed guards run first for fast feedback. The complete required corpus runs
before merge, under `--strict`, so a quarantined guard's failure blocks too.

```sh
reproit --json --yes check @self-dogfood-cli-backend-root --runs 3
reproit --json --yes check --strict --runs 3
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

`reproit-lab` owns multi-repository and native campaign orchestration.
`reproit-proof` owns sanitized public evidence, never the executable source of
truth.

## Current corpus

| Case | Identity | Guard | Repository |
| --- | --- | --- | --- |
| `dog_cli_backend_root` | `doctor:blank-backend-project-root` | `rep_b1ab0f0eb617` | reproit-cli |
| `dog_cloud_migration_history` | `cloud:migration-history-divergence` | `rep_40f619ef4a2c` | reproit-cloud |

Both cases validate under `reproit-lab`'s strict case validator with three
affected reproductions, three reached-observation fixed controls, and verified
digests for every declared artifact:

```sh
cd reproit-lab
npm run dogfood -- validate self-dogfood/cases
npm run dogfood:guard -- recheck cli-backend-root
npm run dogfood:guard -- recheck cloud-migration-history
```
