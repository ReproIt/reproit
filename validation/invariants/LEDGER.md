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
| verdict-lattice | All eight verdict vocabularies map onto one canonical axis, every translation between them preserves the axis, and only a proven fix or a withdrawn claim exits zero | `crates/reproit/src/workflows/verdict_lattice.rs` | cargo | Gap 6: eight vocabularies with translations at six points and no test of the translations; each mapping table is an exhaustive match, so adding a variant anywhere fails to compile until its meaning is stated |

## Capture and replay contract

| id | invariant | proof | kind | origin |
| --- | --- | --- | --- | --- |
| capture-batch-conformance | Every backend SDK emits a batch that passes the protocol's own semantic validator, tagged and redacted | `sdk/test/backend_batch_test.js` | node | The cross-SDK contract test; the shape must be identical across languages |
| hermetic-parity-rust | The Rust SDK re-executes a captured failure with no live dependencies, holding all four verdicts | `sdk/reproit-backend-rs/validation/hermetic-e2e.sh` | sdk | Backend parity work; one acceptance per language so a port cannot claim parity it lacks |
| artifact-portability | A version 3 artifact replays from a moved checkout, and versions 1 and 2 still replay through the legacy path | `validation/backend/artifact-portability-e2e/run.sh` | script | Artifacts stored absolute schema paths and ephemeral-port URLs, so they only worked beside the checkout that wrote them |
| contract-null-optional | A kept guard's contract loads when an optional field serialized as an explicit null | `crates/reproit/src/domain/backend/tests/query.rs::an_explicit_null_optional_field_reads_as_absent` | cargo | Found building the live demo: a guard with a query-semantics invariant could not load, so it silently stopped guarding |
| backend-contract-gate | The real binary scans a real service and its backend contract gate holds end to end | `validation/backend/cli-e2e/run.sh` | script | The backend pillar's own acceptance |
| behavior-vector-coverage | Every directory under `sdk/` either executes the shared behavioral vectors or records why they are meaningless for it; no SDK can be added unwired | `sdk/check-behavior-coverage.py` | python | Gaps 4 and 5: the vectors existed and most SDKs did not run them. Bounds was executed by five of ten wired SDKs and the header cap by exactly one, so nine SDKs shipped the same cap-before-sort defect independently while a vector describing sorted order sat unread in the same repository |
| capture-bounds-shared | The 8 KiB inline budget is measured in encoded BYTES, the 32 header cap is taken over name-sorted order, and an over-budget body keeps only its byte count and sha256, identically in eleven languages | `sdk/capture-behavior-v1.json` | sdk | Gap 5. The bounds were asserted in several SDK suites independently with no single source of truth; wiring the shared vector to all eleven found the header cap applied in arrival order in nine of them |
| redaction-folding-shared | One secret-key folding rule across every SDK: `api-key`, `Access Key` and `X-Authorization` fold to secret and `username` does not, and redaction is structure preserving so a scrubbed body still replays | `sdk/capture-behavior-v1.json` | sdk | Gap 4. Proven per SDK with no shared vector, so a language could quietly diverge on which keys count as secret. The frozen runner wire gets its own group, `causalRedaction`, because it carries thirteen parts where capture carries fourteen |

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
| internal-command-reachable | Every harness invokes an internal command as `reproit internal __name`, the only spelling the CLI accepts | `validation/self-dogfood/test_check_internal_invocations.py` | python | The vocabulary purge moved `__atspi`/`__tui` under `internal` and re-pointed the Rust tests but not three shell harnesses; clap exited 2 before the runner started, and the two AT-SPI gates plus tui-pty were red for 17 commits |
| gate-environment-independence | A gate needs neither an ambient environment variable nor an optional dependency to load code that does not use it | `validation/self-dogfood/test_gate_environment_independence.py` | python | Two jobs red on one push, both green locally: the rn bundle imported webdriverio at top level so the pure signature exports would not load in a job with no npm install, and the Python suite pinned an exact deployment shape while the SDK reads GITHUB_SHA, which only a runner sets |
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
| mobile-divergence-parity | Android, iOS and React Native each emit the structured `REPROIT:DIVERGENCE` marker at the start of a stderr line ALONGSIDE the frozen `CAPSULE:MISS` contract, and all three produce byte-identical payloads for one shared capsule | `validation/mobile/divergence-parity/run.sh` | manual | Gap 3. Mobile had shipped emitting `CAPSULE:MISS` alone, so a mobile capsule replayed through `check` could never report Diverged; the fix was additive on all three SDKs and had no cross-platform test. Run on a Pixel_9a emulator under ART, an iPhone 16 Pro simulator, and node |
| schedule-fuzz-bounded | Schedule fuzzing raises a race's reproduction rate only when the window crosses a hookable boundary, and instrumentation itself widens the window | `validation/process-parallel/MEASUREMENT.md` | manual | 200 runs per cell: 2 to 10 percent natural becomes 46 to 48 percent fuzzed for a libc-crossing window, and nil for a pure memory race; at fire rate zero the rate still rose from 2 to 17 percent |
| gpu-determinism-must-be-measured | A requested determinism flag is not evidence of determinism, and the SAME flag is load bearing on one backend and cosmetic on another | `validation/process-parallel/MEASUREMENT.md` | manual | Measured on both: `use_deterministic_algorithms(True)` is accepted and reports enabled on MPS and CUDA alike, but `scatter_add_` across 8 fresh processes gives 8 distinct results on MPS (flag ineffective) and 1 on CUDA GB10 (flag effective). Nothing at the API level distinguishes them, so a capsule must record a measured probe AND the backend identity, never the request alone |

## Invariants with no executable proof

The honest gap list. Each of these is defended in production code, and nothing
names it. They are ranked, most dangerous first, by what a silent regression
would cost.

1. CLOSED 2026-07-31 by `crates/reproit/src/workflows/verdict_lattice.rs` and
   `hermetic.rs::diverged_is_its_own_verdict_and_never_certifies`. `Diverged`
   had zero Rust tests and rested entirely on shell assertions of exit 3, so a
   refactor collapsing it into `Inconclusive` would have kept every script
   green while destroying the distinction. It now has unit proof that a
   divergence is neither reproduced nor fixed and can never certify, and the
   lattice's mappings are exhaustive `match`es, so that collapse no longer
   compiles.

2. CLOSED 2026-07-31 by `verdict_lattice.rs`, principally
   `nothing_unevaluable_or_drifted_ever_exits_zero` and
   `the_exit_code_contract_matches_the_axis_in_both_vocabularies`. The rule
   this project paid the most for was previously enforced by a grep: the
   ratchet `absence_never_merges_with_a_negative_result` checked only that the
   TOKEN appeared in four named files. `Inconclusive` now has behavioral proof
   that it fails closed and never exits zero, across the process hop.

3. CLOSED 2026-07-31 by `validation/mobile/divergence-parity/run.sh`
   (`mobile-divergence-parity` above). One capsule and one unmatched call, taken
   from `sdk/capture-behavior-v1.json`, put to all three platforms as they
   actually run: the real `CausalHttp` dexed and executed under ART on a Pixel_9a
   emulator, the real `ReproItCausalURLProtocol` on an iPhone 16 Pro simulator,
   and the real `installCausalFetch` under node. All three emitted
   `REPROIT:DIVERGENCE ` at the start of a stderr line, all three still threw the
   frozen `CAPSULE:MISS`, and both payloads were byte-identical across the three.
   Four negative controls were run and each failed exactly the assertion it
   should: the marker on stdout instead of stderr, the marker prefixed so it no
   longer starts the line (Ruby's `warn(uplevel:)` shape), the marker dropped
   entirely while `CAPSULE:MISS` still threw, and one platform disagreeing with
   the other two on the payload. The fourth is the one no per-platform script
   could catch, because every platform passed its own check and the run was still
   wrong. Measurement in that directory's `MEASUREMENT.md`; the device-free half
   is asserted in each mobile SDK's own behavior-vectors suite.

4. CLOSED 2026-07-31 by `sdk/capture-behavior-v1.json` and
   `sdk/check-behavior-coverage.py` (`redaction-folding-shared` and
   `behavior-vector-coverage` above). The folding rule is now one table executed
   by every SDK rather than fourteen restatements of it: eleven capture SDKs run
   `redaction.foldingCases`, `typeCases`, `nestingCases` and the new
   `structureCases`, and the eight replay-only SDKs run `causalRedaction`, the
   frozen runner wire's own thirteen-part list. The two lists differ by exactly
   `idempotencykey`, and that difference is now asserted in both directions so it
   cannot be closed by accident. `structureCases` pins the property the matcher
   depends on and nothing had stated: redaction is structure preserving, so a key
   is never dropped, an array never shortens, and an explicit null stays a null
   value. Negative controls in ten languages each made redaction drop a null or a
   redacted key and each failed naming the missing path.

5. CLOSED 2026-07-31 by `sdk/capture-behavior-v1.json` and
   `sdk/check-behavior-coverage.py` (`capture-bounds-shared` above). The bounds
   were not wrong, they were unread: the vectors already described them and
   `bounds` was executed by five of the ten wired SDKs while `headers` was
   executed by one. Wiring all eleven found the 32-header cap applied in arrival
   order rather than over sorted names in nine of them (node, python, rust, java,
   dotnet, ruby, php, react native, android; go sorted the wire spelling rather
   than the recorded lowercase name). That is the Go defect, shipped nine more
   times, each one a capsule whose recorded header subset could differ from the
   live call it was recorded from. A new case, `budgetIsBytesNotCharacters`, pins
   the budget in encoded bytes with a 4096-character body that is 12288 bytes.
   Negative controls per SDK broke the cap ordering and the byte budget and each
   failed naming the kept subset or the inline body.

6. CLOSED 2026-07-31 by `crates/reproit/src/workflows/verdict_lattice.rs`
   (`verdict-lattice` above). The eight vocabularies still exist, deliberately:
   each names a distinction its layer needs and a flattening would lose. What
   was missing was the statement of how they relate, and that is now one file.
   Every vocabulary maps onto one canonical axis (reproduced, certified, flaky,
   not-about-this-bug, unevaluable, withdrawn), each mapping is an exhaustive
   `match` so a new variant cannot be added silently, and the real translation
   functions are asserted against the axis rather than restated. Five negative
   controls were run and each failed exactly the assertion it should:
   `blocks()` letting `Inconclusive` through, an unevaluable re-check retracting
   a live bug, a reproduction outranking an infrastructure failure in the
   provider fold, `Diverged` exiting zero, and `check`'s exit-code 3 read as
   clean across the process hop.

All six gaps this list has ever named are now closed. That is a statement about
this list, not about the system: a closed list means every gap someone WROTE
DOWN has a proof, and the next real gap is by definition one nobody has thought
to name yet. The list earns its keep only if it keeps growing, so a sweep that
adds no new entry should be read as a sweep that did not look hard enough
rather than as a clean bill of health.

One rule for maintaining it, learned by this list going stale: gaps 1 and 2 sat
here described as open for some time after `verdict_lattice.rs` had actually
closed them, because the prose and the invariant table are maintained
separately. Understating coverage is the safe direction to drift, but it is
still drift. Mark a gap CLOSED in the same change that closes it.
