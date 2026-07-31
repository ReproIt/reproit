# The invariant ledger

What this codebase has learned, and what executes each lesson.

Thirty percent of the CLI (33,319 of 110,607 lines) is already test code. This
ledger makes that knowledge addressable: one row per invariant, naming the
behavior protected, the thing that proves it, and where the invariant came
from. `check.py` fails closed when a row names a proof that does not exist.

Two rules for adding a row:

1. Harvest, do not invent. A row belongs here when something already executes
   it, not when someone believes it.
2. If you cannot name the origin, the row is probably a guess. Say so in the
   gap section instead.

Proof format is `path` or `path::name`. Proof kinds: `cargo`, `script`,
`python`, `node`, `sdk`, `manual`.

## Verdict honesty: absence is never a pass

The most expensive class this project has paid for. Every row here exists
because a run that could not conclude was once reported as clean.

| id | invariant | proof | kind | origin |
| --- | --- | --- | --- | --- |
| absence-not-negative | A state meaning "could not evaluate" never merges with one meaning "evaluated clean" | `crates/reproit/tests/architecture.rs::absence_never_merges_with_a_negative_result` | cargo | Generalized from the verify false-open and the gate-blindness fixes, where an unevaluable run rendered as clean |
| verify-proof-of-fix | A live reproducing finding is never reported as held; only an evaluable, authenticated 2xx with the violation gone certifies a fix | `crates/reproit/tests/backend_verify.rs::a_live_bug_blocks_and_a_real_server_fix_is_held` | cargo | The verify false-open (839d725): an auth-gated endpoint 401'd, the contract was unevaluable, and absence read as fixed |
| retraction-needs-evidence | A withdrawn contract claim retracts only on an evaluable non-reproduction; a flaky or unreachable run cannot retract a live bug | `crates/reproit/tests/backend_verify.rs::withdrawing_a_false_claim_retracts_instead_of_blocking_forever` | cargo | Scan evaluated the current schema while verify replayed the recorded contract, so a wrong claim could never be closed |
| retraction-without-contact | An operation the schema no longer declares retracts without sending a request | `crates/reproit/tests/backend_verify.rs::an_operation_dropped_from_the_schema_retracts_without_calling_the_target` | cargo | Same retraction work; a dropped operation must not be probed to be judged |
| verify-scope | Naming one finding id replays only that finding, not the whole suite at the live target | `crates/reproit/tests/backend_verify.rs::naming_an_id_does_not_replay_the_rest_of_the_suite` | cargo | Verify hardening: `verify <id>` replayed every artifact and filtered afterwards |
| hermetic-four-verdicts | Reproduced, fixed, reproduced-again, and diverged are all distinguishable from a real re-execution, and a verdict line is asserted alongside the exit code | `validation/backend/hermetic-e2e/run.sh` | script | The exit-code-only check: an unresolvable capture also exits 1, so a resolution error read as a reproduction |
| drift-quarantines | A guard whose code drifted is quarantined and reported, never silently red or silently green | `validation/backend/gate-quarantine-e2e/run.sh` | script | Phase 4 of the capsule plan; drift is not a regression and must not gate merges |
| refresh-needs-confirmation | An unconfirmed guard refresh leaves the capture byte-identical; only explicit confirmation rewrites it | `validation/backend/refresh-e2e/run.sh` | script | Drifted guards previously had no path forward but re-capture, so the diff-then-confirm flow was added |

## Capture and replay contract

| id | invariant | proof | kind | origin |
| --- | --- | --- | --- | --- |
| capture-batch-conformance | Every backend SDK emits a batch that passes the protocol's own semantic validator, tagged and redacted | `sdk/test/backend_batch_test.js` | node | The cross-SDK contract test; the shape must be identical across languages |
| hermetic-parity-rust | The Rust SDK re-executes a captured failure with no live dependencies, holding all four verdicts | `sdk/reproit-backend-rs/validation/hermetic-e2e.sh` | sdk | Backend parity work; one acceptance per language so a port cannot claim parity it lacks |
| artifact-portability | A version 3 artifact replays from a moved checkout, and versions 1 and 2 still replay through the legacy path | `validation/backend/artifact-portability-e2e/run.sh` | script | Artifacts stored absolute schema paths and ephemeral-port URLs, so they only worked beside the checkout that wrote them |
| contract-null-optional | A kept guard's contract loads when an optional field serialized as an explicit null | `crates/reproit/src/domain/backend/tests/query.rs::an_explicit_null_optional_field_reads_as_absent` | cargo | Found building the live demo: a guard with a query-semantics invariant could not load, so it silently stopped guarding |
| backend-contract-gate | The real binary scans a real service and its backend contract gate holds end to end | `validation/backend/cli-e2e/run.sh` | script | The backend pillar's own acceptance |

## Process capsule, the general-program boundary

| id | invariant | proof | kind | origin |
| --- | --- | --- | --- | --- |
| process-hermetic | A native program is captured once and reproduced with its input file deleted and its upstream down | `validation/process/run.sh` | script | Phase 1 of the general-program plan |
| process-python-byte-identical | A replayed interpreter's stdout is byte identical to the recording, not merely verdict-equal | `validation/process/run.sh` | script | The double-stdout caveat was stale and unmeasured; a per-PID write table settled it, and the claim is now pinned |
| process-ruby-fails-closed | Ruby does not replay correctly and reports INCONCLUSIVE rather than a reproduction | `validation/process/run.sh` | script | A change traded a loud signal for a quieter one, so the fail-closed property needed its own guard |
| static-binary-refused | A statically linked program is refused before capture rather than producing an empty capsule that would replay as a false success | `validation/process/MEASUREMENT.md` | manual | Measured: a static binary performs no dynamic symbol resolution, so the boundary observes nothing |
| completeness-oracle | A recorded-but-empty file or socket is a named divergence, not a silent empty read | `validation/process/MEASUREMENT.md` | manual | coreutils `cat` and CPython replayed WRONG with zero divergences until absence became loud |

## Self-dogfood policy

| id | invariant | proof | kind | origin |
| --- | --- | --- | --- | --- |
| declaration-required | Every commit touching production source declares how its defect is proven; a missing declaration is a failure, never an implicit exception | `validation/self-dogfood/check-fix-policy.py` | python | Gate D4, the policy's founding rule |
| exception-binding | An exception record must be touched by the range citing it, so a commit cannot satisfy the gate by naming any record in history | `validation/self-dogfood/test_check_fix_policy.py` | python | Found by committing one: an unrelated exception was cited and the gate accepted it (96c7007) |
| policy-survives-force-push | An unreachable base sha gates the head commit alone instead of erroring on an unevaluable range | `validation/self-dogfood/test_check_fix_policy.py` | python | CI failed on a force-pushed range whose before-sha was orphaned |
| no-dead-check-flags | No workflow or self-dogfood runner passes a flag `check` no longer accepts | `validation/self-dogfood/test_check_flag_callers.py` | python | The vocabulary purge deleted flags four callers still passed, so guard replay exited 2 with a usage error |
| harness-case-accounting | An acceptance script asserts how many cases actually ran, so an early exit cannot look like a pass | `validation/self-dogfood/check-harness-integrity.py` | python | Four separate instances in one day of a harness that stopped early and looked exactly like one that passed |
| required-guards-replay | Every required guard replays and holds, and a skipped guard fails loudly rather than passing | `validation/self-dogfood/run-required-guards.py` | python | The corpus gate; a guard that does not run is not a guard |
| ci-enforcement-contract | The CI workflow and the guard runner keep their agreed shape, and drift between them is caught | `validation/self-dogfood/ci-enforcement.mjs` | node | The required-corpus dispatch contract |

## Architecture ratchets

| id | invariant | proof | kind | origin |
| --- | --- | --- | --- | --- |
| layering | Inner layers never depend on outer ones: domain and adapters never reach into interface or workflows | `crates/reproit/tests/architecture.rs::inner_layers_do_not_depend_on_outer_layers` | cargo | The layering the whole codebase rests on |
| domain-determinism | Domain code stays deterministic, with one documented exception that may shrink but never grow | `crates/reproit/tests/architecture.rs::domain_code_stays_deterministic` | cargo | Oracles must not read the clock or the environment behind the caller's back |
| file-size | No source file exceeds 1000 lines | `crates/reproit/tests/architecture.rs::source_files_stay_reviewable` | cargo | The founder's reviewability rule, enforced rather than requested |
| no-glob-parent | A new module imports what it uses by name rather than glob-importing its parent | `crates/reproit/tests/architecture.rs::new_modules_do_not_glob_import_their_parent` | cargo | Caught the hermetic module on its first commit |
| canonical-artifact-layout | Production code composes artifact paths from the layout helpers instead of hard-coding them | `crates/reproit/tests/architecture.rs::production_code_uses_canonical_artifact_layout` | cargo | Caught a hard-coded `.reproit/findings` in an error message during the refresh work |
| honest-gap-phrasing | Every gap message names the exact next input rather than a dead-end phrase | `crates/reproit/tests/architecture.rs::a_gap_is_always_phrased_as_the_next_input` | cargo | The init overhaul's honesty invariant |
| multi-schema | A project declaring several schemas is never narrowed to the first one | `crates/reproit/tests/architecture.rs::a_multi_schema_project_is_never_narrowed_to_one` | cargo | Resolving only `.schemas.first()` silently dropped every operation past the first |
| runner-shared-core | Native runners compose the shared signature core rather than reimplementing it | `crates/reproit/tests/architecture.rs::native_runners_compose_the_shared_signature_core` | cargo | Signature parity across platforms depends on one implementation |
| runner-module-resolution | A runner's import graph resolves; a source-relative specifier cannot ship broken | `runners/signature_test.mjs` | node | The tauri snapshot import shipped a specifier that never resolved |

## Tooling that the product depends on

| id | invariant | proof | kind | origin |
| --- | --- | --- | --- | --- |
| prune-portability | The target prune behaves identically on GNU and BSD find | `scripts/test_prune_target.py` | python | GNU findutils rejects `-prune` with `-delete`, so the prune exited 1 on every Linux runner |
| prune-preserves-out-dirs | The prune never deletes build-script OUT_DIR files whose fingerprints tell cargo not to re-run | `scripts/test_prune_target.py` | python | A half-missing build broke tree-sitter's `include_str!` on the next compile |

## Device and hardware behavior, measured not re-run

These are proven on real hardware and retained as evidence. CI does not re-run
them, which is stated rather than implied.

| id | invariant | proof | kind | origin |
| --- | --- | --- | --- | --- |
| android-capsule-delivery | A capsule is spooled to disk during a crash and uploaded on the next launch, rather than racing process death | `sdk/reproit-android/validation/capsule-delivery.sh` | manual | Measured on a Pixel emulator: the crash-to-death window is 168 to 768 ms and a cold POST takes 40 to 316 ms, so delivery was 4 of 6 on loopback and 0 of 2 with realistic latency |
| android-capsule-fidelity | A captured null in a payload is recorded and served as null, so replay reproduces the same error as production | `sdk/reproit-android/validation/EMULATOR-PROOF.md` | manual | The emulator caught an encoder dropping null map values; replay had reproduced a DIFFERENT error than production |
| ios-secret-never-leaves | The literal secret is absent from the bytes that leave the device, asserted by grep over the posted batch | `sdk/reproit-ios/validation/simulator-e2e.sh` | manual | Proven on an iPhone 16 Pro simulator with a real URLSession call |
| schedule-fuzz-bounded | Schedule fuzzing raises a race's reproduction rate only when the window crosses a hookable boundary, and instrumentation itself widens the window | `validation/process-parallel/MEASUREMENT.md` | manual | 200 runs per cell: 2 to 10 percent natural becomes 46 to 48 percent fuzzed for a libc-crossing window, and nil for a pure memory race; at fire rate zero the rate still rose from 2 to 17 percent |
| gpu-determinism-must-be-measured | A requested determinism flag is not evidence of determinism; MPS accepts `use_deterministic_algorithms(True)`, reports enabled, and changes nothing | `validation/process-parallel/MEASUREMENT.md` | manual | `scatter_add_` gave 8 distinct results across 8 processes while the flag reported enabled, so recording the flag would record a false assurance |

## Invariants with no executable proof

The honest gap list. Each of these is defended in production code, and nothing
names it. They are ranked, most dangerous first, by what a silent regression
would cost.

1. **The `Diverged` verdict has no test anywhere in the crate.** `Diverged` appears
   in three production files and zero Rust test files. Its only executable
   proof is at the script level (`validation/process/run.sh` and seven SDK
   hermetic scripts assert exit 3), which means the verdict's *internal*
   semantics, that a divergence is neither reproduced nor fixed and can never
   certify a fix, rest entirely on end-to-end shell assertions. A refactor that
   collapsed `Diverged` into `Inconclusive` would keep every script green while
   destroying the distinction. This is the single most dangerous gap, because
   `Diverged` is the newest verdict and the one the drift-quarantine behavior
   depends on.

2. **`Inconclusive` is named in seven production files and no unit test.** The
   architecture ratchet `absence_never_merges_with_a_negative_result` checks
   that the *token* appears in four named files, which is a spelling check, not
   a behavioral one. `backend_verify.rs` exercises the fix and retraction paths
   but never asserts that an inconclusive run fails closed. The rule this
   project paid the most for is therefore enforced by a grep.

3. **The mobile divergence marker split has no cross-platform proof.** Android,
   iOS and React Native now emit `REPROIT:DIVERGENCE` alongside the frozen
   `CAPSULE:MISS`, so the CLI's verdict path can read a mobile replay. Nothing
   asserts that both markers are emitted together on all three platforms; the
   simulator and emulator scripts are per-platform and manual. A platform that
   silently dropped the structured marker would report a mobile divergence as
   something else, which is exactly the class of bug this addition existed to
   fix.

4. Redaction keyword folding is proven per SDK but has no shared vector, so a
   language could quietly diverge on which keys count as secret. This is the
   gap that `plan-simplification.md` step 1.1 exists to close.

5. The capture bounds (8 KiB inline, 32 headers, sha256 beyond the budget) are
   asserted in several SDK suites independently, with no single source of
   truth. Same origin, same fix, same plan step.

6. `ReplayVerdict`, `ArtifactVerdict`, `HermeticVerdict`, `ReproVerdict`,
   `ProviderVerdict`, `ExecutionVerdict`, `RunVerdict` and `Outcome` are eight
   verdict vocabularies. No test asserts how they map onto one another, so a
   translation error between two of them would be invisible until it produced a
   wrong verdict in the field. `plan-structural-reduction.md` step B proposes
   converging them; until then the mapping is undefended.

Closing gaps 1 through 3 is cheap and should happen before any refactor that
touches verdict handling. Gaps 4 and 5 are the conformance-vector work already
planned. Gap 6 is a design decision, not a test.
