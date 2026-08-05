# Production to local customer journey

The customer should start from the bug they already have. Repro It should not ask
them to recreate the report, copy stack traces, invent test data, or manually
translate a production event into a test.

## Target journey

The Repro It SDK is the production capture authority. It records the trigger,
dependency exchanges, deterministic inputs, observation point, and exact
failure identity needed for replay. Imported diagnostics may add optional
context for a person or agent, but cannot replace SDK capture and can never make
an occurrence executable.

1. The customer installs the Repro It SDK and deploys it with a project-scoped,
   write-only capture key.
2. A failing captured operation is normalized into an immutable Repro It
   occurrence. Repro It groups occurrences for prioritization, but keeps the
   exact occurrence as the reproduction identity.
3. Repro It assesses the SDK occurrence automatically:
   - `eligible`: enough source-neutral evidence exists to reproduce it.
   - `incomplete`: one or more named facts are missing.
   - `environment-bound`: the failure needs controlled remote infrastructure.
   - `unsupported`: Repro It preserves the evidence without pretending it can run.
4. The issue page presents one primary action:

   ```text
   reproit occ_0123456789abcdef
   ```

5. In a trusted checkout, the CLI downloads and verifies that occurrence. The
   package contains facts, fixtures, observations, and replay actions. It cannot
   introduce a process command from captured production evidence.
6. The local checkout selects the trusted app adapter and launch policy. Repro It
   synthesizes safe fixture values, performs the replay, checks the same failure
   identity, and writes the result beside the occurrence.
7. After the fix, the developer runs the same command. A clean result can be kept
   as a named regression guard and run in CI.
8. Cloud receives the reproduction and verification result, links it to the
   aggregate bug, and can update the source issue without making the source tool
   the execution authority.

## Trust boundary

Production evidence describes what happened. The checkout decides how code may
run. This separation is the core safety rule:

- The Repro It SDK may provide occurrence facts and content-addressed artifacts.
- Imported diagnostics may provide non-authoritative context only.
- Repro It Cloud may store, assess, group, and distribute those facts.
- Only a trusted checkout or approved remote adapter may supply executable
  commands, credentials, infrastructure, or destructive reset behavior.

The CLI may import with `--no-run` for inspection. A normal occurrence command
must show why execution is blocked when no exact trusted adapter is available.

## Different paths for different evidence

| Evidence state | Customer path |
| --- | --- |
| Eligible UI replay | Run the occurrence in the trusted app adapter. |
| Eligible process plan | Run the plan selected by the checkout's provider catalog. |
| Backend capture with recorded exchanges | Re-execute hermetically: `reproit check <capture.json>` boots the service with the SDK serving every recorded dependency exchange, and verdicts reproduced, fixed, diverged, or inconclusive from the live response. The boot command comes from `backend.exec` in reproit.yaml (recorded by `reproit init`), or from `--exec` as the override. Node, Python, Go, Rust, Java, .NET, PHP, and Ruby share the same replay contract. |
| Backend capture without exchanges | Re-evaluate the recorded events offline; honest about not re-running code. |
| CI test capture (flaky-CI wedge) | Same capsule, captured in CI instead of production: the test job records with `REPROIT_CI_CAPTURE=1`, a failing test spools the capsule, and the red job ships it as a job artifact with the repro command in the summary. `reproit check <capsule.json> --exec "<test command>"` re-executes the exact failing run locally with the recorded exchanges and envelope; a plain rerun passing outside the capsule stays flaky evidence, never Fixed. The eight SDKs integrate with node:test, pytest, Go test, cargo test, JUnit 5, .NET test runners, PHPUnit-style suites, and RSpec-style suites. File-based and local-first; cloud ingest optional, never required. Races the boundary cannot see are Inconclusive, never faked. |
| Incomplete | Show the smallest missing fact and the action that can collect it. |
| Environment-bound | Offer an approved disposable worker, simulator, or VM path. |
| Unsupported | Preserve and link the evidence, with no false reproduce button. |
| Reproduced after a fix | Keep it as a regression guard and verify it in CI. |

## How this differs from the traditional path

| Traditional issue flow | Repro It flow |
| --- | --- |
| Alert is a stack trace and prose ticket. | Alert becomes a typed occurrence package. |
| Developer guesses which user state mattered. | Fixture classes and path evidence are retained. |
| Developer manually recreates steps. | The exact occurrence ID is the replay entry point. |
| Production data is copied into local notes or scripts. | Safe synthetic values are derived from bounded fingerprints. |
| A new test is written only after diagnosis. | The reproduction can become the regression guard. |
| Closing the ticket is mostly a human claim. | Local replay, CI, and later production evidence form one chain. |

## Product rule

The bucket is the unit of prioritization. The occurrence is the unit of
reproduction. Every reproduce button, API response, copied command, and retained
verification record should preserve that distinction.
