# Reproit CLI architecture

The CLI is organized so correctness-sensitive logic can be tested without a device, network,
process-global arguments, or terminal output.

## Dependency direction

```text
process entry point -> interface -> workflows -> domain
                                  |            -> adapters
                                  +------------> runtime
```

- `src/main.rs` delegates to `runtime/startup.rs`, which owns the bounded-stack thread and Tokio
  runtime used on every platform.
- `interface/` owns CLI parsing, process output and exit policy, JUnit rendering, and the MCP
  protocol boundary.
- `workflows/` owns command dispatch and cohesive use cases such as fuzzing, journeys, triage,
  accessibility reporting, and headless service exploration.
- `domain/` owns canonical data, protocol evaluation, evidence identity, graph analysis, and
  persistence rules. Deterministic evaluation stays separate from acquisition mechanisms.
- `adapters/` owns configuration loading, credentials, project scaffolding, platform acquisition,
  device control, update checks, and other external-system mechanisms.
- `runtime/` owns process execution, startup, and canonical project artifact paths.
- `assets/` owns data embedded into the binary. Flutter source under
  `assets/scaffolds/flutter/` is a generated-project asset, not Rust implementation code.

Dependencies point away from external interfaces. Domain and adapter production code cannot import
the interface or workflows, and the interface cannot import workflows. Workflows coordinate the
layers after parsing. A few domain persistence modules use configuration and project-layout types,
but deterministic evaluators do not get external evidence themselves. The exact remaining
domain-to-adapter and domain-to-runtime files are allowlisted by an architecture test. The list may
shrink as narrow input models move inward, but a new outward dependency fails the build. This keeps
future adapters additive without requiring a speculative plugin framework.

## Why `workflows/` groups by target class, not by lifecycle

`workflows/` is about half the crate, so it periodically attracts a proposal to regroup it by
bug lifecycle: discover, reproduce, retain, report. That split was measured against the tree on
2026-07-31 and rejected. The lifecycle is the axis each module is already organized by INTERNALLY,
so grouping by it would cut through the largest modules instead of grouping them.

- Only 15 of 50 top-level modules straddle two or more lifecycle phases, but those 15 are 51.6% of
  the lines. The three largest straddle all four: `backend_headless`, `fuzz`, `journey`. The files
  under `fuzz/` are already named `campaign`, `confirmation`, `findings`, and `reporting`, which is
  discover, reproduce, retain, and report.
- A further 39.3% of the lines sit in no phase at all: `backend_learn` is project onboarding, and
  auth, cloud, doctor, reset, device, init, dispatch, and `backend_target` are plumbing.
- What the reference graph does cluster by is target class, and the module names already say it.
  Intra-`workflows` references are 66 edges in a near-tree, and there is not one edge between the
  backend modules (`backend_learn`, `backend_headless`, `backend_target`, 42.0% of the lines) and
  the UI-driving modules (fuzz, journey, barrier, device, and their neighbors, 27.1%). They meet
  only at `backend_target` and at `internal_dispatch`. That boundary is real: the backend boots a
  process-global restartable server, the UI boots a per-run device torn down each time.

The move would also not be mechanical. 51 files here open with a top-level `use super::*`, 22 of
them under `backend_headless/` sharing that module's types, so splitting it across phase
directories means rewriting every one of those imports and hoisting the shared types into a fifth
directory larger than any phase. That is the "sharding satisfies the ratchet without decoupling
anything" failure that `new_modules_do_not_glob_import_their_parent` exists to catch.

Regroup this directory when a real dependency forces it, not to satisfy a vocabulary.

## Correctness rules

The project follows these correctness rules:

1. Bound work at trust boundaries: schema depth, response bytes, retries, generated values,
   endpoints, messages, and reductions have explicit limits.
2. Treat external input as fallible. Return an error or abstain; assertions are reserved for
   internal invariants.
3. Keep deterministic logic pure. Time, environment, filesystem, network, and subprocess access
   enter through narrow adapters.
4. Prefer explicit state machines and enums over correlated booleans or magic strings when they
   remove invalid states.
5. Preserve wire formats, ordering, fingerprints, exit codes, and abstention behavior with
   characterization tests before changing implementations.
6. Avoid generic `utils`, `common`, or `crosscut` modules. A module has one domain-specific reason
   to change.
7. Use one canonical implementation for artifact paths, hashes, contract paths, target resolution,
   and finding persistence.
8. Keep changes mechanically reviewable: structure first, behavior second.
9. Prefer a small named module over a generic helper collection. Visibility is private by default
   and widened only to the nearest parent that coordinates it.
10. Make limits executable: pair each important bound with a rejection or truncation path and a test
    at or beyond the boundary.
11. Keep owned source and prose within 100 columns. Wrap expressions and sentences instead of
    requiring horizontal scrolling; generated artifacts and third-party sources are excluded.

## Architecture ratchets

`tests/architecture.rs` keeps the process entry point and crate root small, rejects `#[path]`
shortcuts that bypass the module hierarchy, prevents new raw artifact paths, and sets a generous
upper bound on source-file size. The size limit is a last-resort tripwire rather than a target;
cohesion determines when a file should be split.

Formatting, warnings-as-errors Clippy, workspace tests, real CLI contract tests, and native platform
gates are required for framework-wide changes.

## App-map and runner pipeline

The reviewable app map deliberately uses `BTreeMap<StateId, State>` and `Vec<Transition>`. Expected
maps do not need a graph database. Deterministic JSON and a small data model are more valuable than
a general graph abstraction.

- A state key is an immutable structural id. An editable `State.name` supplies the human label.
- `schemaVersion` changes only for incompatible file-format changes. The in-memory `revision`
  changes whenever graph content changes and is serialized as `version`.
- `GraphIndex` derives signature, incoming, and outgoing indexes in memory. It is never persisted.
- Map and visit JSON is streamed to same-directory temporary files and atomically replaced under a
  workspace lock. A recovery journal rolls an interrupted multi-file commit forward before reads.
- Corrupt maps, corrupt visits, unsupported schemas, and dangling transitions are errors. They are
  never interpreted as an empty graph.
- Typed input values are runtime evidence, not graph identity, and are discarded at map ingestion.
- Unknown or malformed actions abstain. They are never converted into another action.

Runner output crosses one lexical boundary in `domain/runner.rs`. Platform control markers remain a
capture-adapter concern. Verdict-bearing evidence crosses the core boundary only as a strict
`REPROIT/1 <domain> <subject> <sequence> <run-id> <event-json>` frame defined by the
`reproit-protocol` package. The package is independently versioned from both products and publishes
a machine-readable wire ledger. CLI and Cloud consume the same reviewed package content and reject
unknown fields, unknown versions, invalid ordering, malformed scopes, and values outside explicit
bounds. Cloud may retain an exact vendored snapshot while a package release is being reviewed, but
content equality and the package version are gated. A CLI commit pin is provenance and controller
selection, not a claim of protocol compatibility.

The same shared protocol defines the causal graph and environment-minimization envelope used by
capsule schema version 2. A capsule is assembled through five layers: bounded capture,
normalization, pure tri-state evaluation, exact confirmation and minimization, then immutable
artifact persistence. The causal graph is data, not an execution framework. Platform adapters
capture facts and apply bounded environment mutations; the core owns graph construction,
dependency closure, proof rules, and content identity.

One frame is limited to 1 MiB. Its fixed header fits in the first 512 bytes, before the JSON event.
An oversized frame becomes a persisted stream defect with reason `frame-too-large`; its payload is
not parsed. An unattributed contract defect makes every configured temporal contract abstain. A
valid bounded contract hash limits that abstention to the exact matching contract. Malformed frames
and unsupported versions are shared defects. The removed `REPROIT:EVENT/1` envelope is never
decoded as protocol evidence.

Normalized traces, evaluations, confirmation results, minimized traces, and proof ledgers are
persisted as a topologically ordered evidence graph. Every node id is the SHA-256 digest of its
kind, parents, and payload. A proof ledger cannot serialize as `confirmed` unless every finding has
an authority source, evaluation is `VIOLATION`, clean replay reproduced the exact identity, and
minimization preserved it. The cloud validates every node again, stores nodes by `(app, digest)`,
and attaches graph roots to run ids. CLI/cloud handoff therefore transfers immutable identities
rather than mutable files with parallel interpretations.

Graph analysis is search guidance only. Bounded strongly connected component and dominator
analysis prioritizes rare frontiers that unlock more reachable state. It degrades to the existing
deterministic visit ordering above its analysis bounds and never enters evaluation, confirmation,
or finding identity.

Causal reduction is separate from search guidance. It operates on a validated acyclic event graph
whose edges distinguish ordering, data flow, state prerequisites, actor ownership, and contract
scope. Hard dependencies are removed atomically, each candidate receives a clean exact-identity
replay, and the accepted graph is regenerated from the executable capsule. Environment reduction
uses the same tri-state rule: only an exact reproduction permits relaxation, a reconfirmed baseline
permits a required-dimension claim, and uncertainty remains `ABSTAIN`.

## Source-neutral occurrences and trusted execution

Evidence ingestion is independent from reproduction execution:

```text
evidence source
  -> immutable occurrence
  -> capability assessment
  -> reproduction plan
  -> executable capsule
  -> trusted local or remote providers
  -> exact verdict
  -> regression guard
```

`reproit-protocol` owns the strict `OccurrenceEnvelope`, typed artifact inventory, capture defects,
requirements, assessment, plan, package, and support-bundle manifest. Cloud and CLI compile the
same definitions. Cloud retains its legacy bucket projection for compatibility, but every new error
also receives an occurrence id and a typed reproduction package.

The trust boundary is asymmetric by design. Evidence may describe facts, reference
content-addressed artifacts, and identify required capabilities. It may not supply an executable
mechanism. A plan binding accepts only these authorities: trusted checkout, built-in adapter,
organization policy, or explicit local approval. The first local executor resolves bindings
through `execution.providers` in the checkout-owned `reproit.yaml`, recomputes
the provider digest, verifies its phase and exact observation, and only then
launches it.

The execution state machine is shared by actionless and action-driven failures:

```text
validate -> reserve -> reset -> build -> seed -> launch -> readiness
         -> debug -> trigger -> observe -> retain -> cleanup
```

Every phase is explicit even when skipped. Commands have bounded argv, environment, timeout,
working directory, and retained output. Working directories must remain inside the checkout.
Output pipes are drained while only the first 1 MiB is retained. Cleanup is attempted in reverse
provider order after success, a different failure, timeout, or infrastructure failure.

Exact reproduction, clean non-reproduction, different failure, incomplete evidence, unsupported
capability, infrastructure failure, stale execution, and flaky execution remain distinct. Only the
exact observation is `reproduced`. Only a reached observation point without the exact identity is
`not_reproduced`.

Capsule schema version 2 remains wire-compatible. It now optionally carries the occurrence,
assessment, and reproduction plan as an all-or-none typed group. Legacy action capsules keep their
existing hashes and behavior because absent typed fields are not serialized. New typed capsules
derive their cause from process, command, message, timer, installer, migration, filesystem,
resource-pressure, concurrency, device, HTTP, or UI requirements.

## Finding-preservation rule

Performance and storage changes must not broaden a finding predicate. A refactor is acceptable only
when the same authoritative evidence produces the same finding identity and incomplete, malformed,
or unsupported evidence still abstains. In particular:

- contracts and route-access cells carry typed `declared`, `derived`, or `suggested` authority;
  `suggested` rules abstain before evaluation and cannot steer exploration;
- missing effects or lifecycle observations do not prove absence;
- sparse graph snapshots do not prove permission traps when an equivalent route has a forward exit;
- advisory timing or pixel signals do not become verdict-bearing reproductions; and
- framework failures do not become application failures.

The adversarial clean-corpus tests, invariant tests, replay tests, and native gates enforce this
boundary. Scaling work is performed behind these characterization tests.

## Performance shape

The main hot paths are designed around bounded, linear work:

- fuzz guidance is computed once from the pre-batch map snapshot and shared by every seed;
- frontier search stores a predecessor and depth per state, then reconstructs one winning path;
- permission analysis summarizes edges once instead of scanning all edges for every state;
- constant regular expressions are initialized once, and repeated schema patterns are cached for
  one recursive domain evaluation;
- evidence sidecar files remain open for the run, while state changes wake orchestration directly;
- source fingerprints and JSON persistence stream through bounded buffers; and
- disabled contract and backend pipelines do not parse their event types.

Parallel parsing and alternative graph containers require benchmark evidence. They are not default
optimizations because deterministic ordering and a simple failure model are part of correctness.
