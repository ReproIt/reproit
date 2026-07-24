# Oracle operating guide

Use an oracle to explain what evidence made a ReproIt result authoritative. Never describe a
repeated observation as a confirmed bug unless its oracle has an authoritative truth source and
exact replay predicate.

The repository's complete catalog is [`docs/oracles.md`](../../../docs/oracles.md). This reference
contains the rules an agent needs while diagnosing a finding.

## Verdicts

| Verdict   | Interpretation                                                     |
| --------- | ------------------------------------------------------------------ |
| `VIOLATION` | Exact authoritative evidence contradicts the oracle contract.      |
| `SATISFIED` | The same evidence channel proves the contract currently holds.     |
| `ABSTAIN`   | Evidence is missing, ambiguous, unsupported, or not authoritative. |

`ABSTAIN` is not clean, broken, fixed, or flaky.

## Default confirmed categories

| Oracle               | Authority                                                               |
| -------------------- | ----------------------------------------------------------------------- |
| `crash`              | Uncaught exception, fatal assertion, signal, or native crash            |
| `overflow`           | Declared container plus two stable exact-layout samples                 |
| `detached-indicator` | Application-declared owner, container, gap, and settled geometry        |
| `contract`           | Application-declared structural or temporal rule with a frozen identity |

The top-level `invariant` and `visual` categories are also contract-dependent. They require an
application predicate or an approved pinned baseline.

## Specialist UI categories

These may be useful observations, but selecting them with `--only` does not upgrade their
confidence:

- Environment-dependent: `jank`, `leak`, `flicker`, `divergence`, `hang`, `stuck-keyboard`,
  `rotation`, `background-restore`, `scroll-round-trip`, `wakelock`, `safe-area`, and
  `permission-walk`.
- Heuristic or policy-dependent: `content-bug`, `occlusion`, `choice-anomaly`, `broken-route`,
  `security`, `duplicate-submit`, `focus-loss`, corroborated `blank-screen`, `broken-asset`,
  `zoom-reflow`, `zero-contrast` (an emphasized glyph run whose resolved foreground exactly
  equals its resolved background, so selected or highlighted content renders invisible; exact
  colorimetric equality only, TUI first), and `dead-input` (a runner-injected keystroke or
  wheel that provably vanished: no event, no value or scroll delta, and no handler claimed it
  with preventDefault; intentional filters, masks, and custom editors abstain; web first).
  Visual emptiness alone is diagnostic and abstains.
- Experimental semantic parity: `accessibility-state`. It remains defaults-off until historical
  red/green cases establish the proof boundary; explicit ARIA overrides and disabled-state
  differences currently abstain.
- `unclassified` is registry-drift telemetry and can never become a confirmed bug.

Useful exact contract identities include:

- `focused-input-obscured:<field>`
- `state-preservation:<boundary>:<id>`
- `action-effect:<id>:route`
- `action-effect:<id>:state`
- `detached-indicator:<id>`
- `overflow:<subject>:<container>`
- `accessibility-state:<identity>:<property>`
- `route-access:<route>:<principal>`

When one of these returns `ABSTAIN`, report which required signal was absent. Do not replace the
missing signal with a screenshot or language-dependent text guess.

## Backend categories

Backend support is experimental. A result needs a schema-owned or authored contract and a runtime
event correlated to the exact operation. Each backend check is a first-class oracle category whose
finding carries the per-check `backend-*` id below; legacy artifacts stamped with the umbrella id
`backend-contract` remain readable.

- Request and response: `backend-server-error`, `backend-response-status`,
  `backend-accepted-invalid-input`, `backend-response-shape`, and `backend-response-selection`.
- Effects and tenancy: `backend-read-only-mutation`, `backend-missing-effect`,
  `backend-excess-effect`, and `backend-tenant-isolation`.
- Data loss and resource lifecycle: `backend-data-loss`, `backend-resource-create-missing`,
  `backend-resource-delete-visible`, `backend-resource-identity`, `backend-resource-state`,
  `backend-resource-round-trip`, and `backend-codec-round-trip`.
- Query and application rules: `backend-authored-invariant`, `backend-query-pagination`,
  `backend-query-pagination-reference`, and `backend-idempotency`.
- Deployment and multi-actor proofs: `backend-fleet-consistency`, `backend-authorization-matrix`,
  `backend-transaction-atomicity`, `backend-concurrent-update`, and
  `backend-concurrent-conservation`.
- Scoped protocol evidence still reports under the `backend-contract` umbrella id:
  `http-byte-range`, `http-redirect-transition`, `http-response-media-type`,
  `http-conditional-cache`, `lifecycle-precedence`, `lifecycle-forbid-after`, and
  `lifecycle-cardinality`.

Backend semantics must not be inferred from operation, field, route, framework, or function names.
Missing strong consistency, snapshot identity, complete effects, or an explicit behavioral contract
means `ABSTAIN`.

Browser document access is a separate authored matrix. Run
`reproit scan --only route-access`. A non-anonymous cell first proves its configured principal,
then directly navigates to the exact route in an isolated browser context. A violation is confirmed
only when a second clean context produces the identical observation. Missing auth authority,
incomplete navigation, external redirects, and unstable evidence return `ABSTAIN`.

HTTP media-type and cache proofs require exact captured headers and body bytes. Lifecycle proofs
require one complete, stably identified, totally ordered scope. Codec proofs require a declared
operation plus complete unredacted input/output projections. Matching exact evidence is
`SATISFIED`; missing, malformed, inferred, redacted, mixed-scope, or incomplete evidence is
`ABSTAIN`. These rules are framework-neutral.

## A2UI categories

A2UI runs validate v0.9 streams and render them through the official React and Lit integrations:

- `protocol-invalid`
- `renderer-error`
- `unlabeled-input`
- `unlabeled-button`
- `stream-convergence`
- `default-conformance`
- `bound-action-coherence`

Each A2UI finding must retain a minimized message stream, renderer identity, structural signature,
repair context, and exact replay predicate.

## Commands

```sh
reproit scan                 # one coverage walk
reproit fuzz                 # deeper sequences and structural inputs
reproit fuzz --only crash    # narrow observation to one category
reproit <finding-id>         # replay the saved minimized proof
```

Shrinking may shorten a sequence only while the same oracle identity and fingerprint reproduce. A
different failure is a different bug.

## How to explain a result

Give four facts:

1. The oracle and exact structural identity.
2. The authoritative evidence it consumed.
3. The minimal sequence that reproduces it.
4. What `SATISFIED` would look like after a fix.

If any of those facts are unavailable, state the gap instead of upgrading the claim.
