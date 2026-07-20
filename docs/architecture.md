# Reproit CLI architecture

The CLI is organized so correctness-sensitive logic can be tested without a device, network,
process-global arguments, or terminal output.

## Dependency direction

```text
process entry point -> CLI parsing -> command dispatch -> workflow modes
                                       |               -> domain model
                                       +---------------> backend adapters -> infrastructure
```

- `src/main.rs` only delegates to `startup.rs`; startup owns the explicit bounded-stack thread and
  Tokio runtime used on every platform.
- `cli/` owns process exit codes, argument parsing, target classification, and compatibility
  rewrites.
- `commands/` translates parsed commands into calls to workflows, models, and adapters. It is the
  only application-level dispatcher.
- `modes/` owns cohesive user-facing workflows such as fuzzing, journeys, triage, and headless
  backend exploration.
- `model/` owns deterministic data and analysis. Model code must not read the environment, start
  processes, perform network requests, or print.
- `backends/` owns runtime and device adapters.
- `infra/` owns explicit operating-system and external-system mechanisms.
- `layout.rs` is the sole authority for canonical project artifact paths.

Dependencies point inward: models do not depend on commands or process state, and workflow modes do
not parse process arguments. Backends may translate external state into model types, but model code
does not call a backend.

Temporary crate-root re-exports preserve compatibility while callers migrate from the historical
flat namespace. New code must use the real namespace.

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
  changes whenever graph content changes and remains serialized as legacy-compatible `version`.
- `GraphIndex` derives signature, incoming, and outgoing indexes in memory. It is never persisted.
- Map and visit JSON is streamed to same-directory temporary files and atomically replaced under a
  workspace lock. A recovery journal rolls an interrupted multi-file commit forward before reads.
- Corrupt maps, corrupt visits, unsupported schemas, and dangling transitions are errors. They are
  never interpreted as an empty graph.
- Typed input values are runtime evidence, not graph identity, and are discarded at map ingestion.
- Unknown or malformed actions abstain. They are never converted into another action.

Runner output crosses one lexical boundary in `model/runner.rs`. Platform control markers remain a
capture-adapter concern. Verdict-bearing evidence crosses the core boundary only as a strict
`REPROIT/1 <domain> <subject> <sequence> <run-id> <event-json>` frame defined by the
`reproit-protocol` package. CLI and cloud compile the same package source and reject unknown fields,
unknown versions, invalid ordering, malformed scopes, and values outside explicit bounds.

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

## Finding-preservation rule

Performance and storage changes must not broaden a finding predicate. A refactor is acceptable only
when the same authoritative evidence produces the same finding identity and incomplete, malformed,
or unsupported evidence still abstains. In particular:

- inferred contracts do not produce confirmed findings;
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
