# Self-dogfood status: what ReproIt actually guards about itself

Written 2026-07-31. This is a scoreboard, not marketing. It is meant to be
usable as evidence against us, so every number here is the honest one and the
gaps are stated in more detail than the wins.

## The measured starting point

Roughly fifteen defects were found and fixed in this project on 2026-07-30 and
2026-07-31. **ReproIt guards caught zero of them.** What caught them was
acceptance scripts, CI, the protocol validator, and running on real devices and
emulators. The declaration ledger showed it plainly: six `not-a-fix`, five
`exception`, three `no-repro`, and **zero `guard:`**.

That is not laziness. It is structural. A guard could only express a
request-shaped API contract violation, and almost none of this project's own
defects are that shape.

## Guard inventory

| guard | alias | what it protects | oracle |
| --- | --- | --- | --- |
| `rep_6bc2f97d73a7` | self-dogfood-required-corpus-dispatch | CI still dispatches the full required guard corpus | exact-occurrence |
| `rep_b1ab0f0eb617` | self-dogfood-cli-backend-root | `doctor` on a blank backend project root | exact-occurrence |
| `rep_cf7b6f962595` | self-dogfood-direct-push-policy | CI still enforces the direct-push dogfood policy | exact-occurrence |
| `rep_b9fee273bb4a` | self-dogfood-contract-null-optional | a kept guard's contract with explicit nulls still LOADS | exact-occurrence |
| `rep_77fe5b41f678` | self-dogfood-check-flag-callers | no repository caller passes a deleted `check` flag | exact-occurrence |

Five required guards, up from three. The two new ones are the first guards in
this project that assert **behaviour of the reproit binary and its callers**
rather than CI wiring.

Both new guards are proven in both directions, end to end through
`reproit check`, not merely at the verifier:

```
HOLD   run-required-guards.py            all 5: pass, not_reproduced x3 each
RED 1  pre-fix binary swapped in         FAIL rep_b9fee273bb4a  exit 1
RED 2  deleted flag reintroduced         FAIL rep_77fe5b41f678  exit 1
```

## The honest count

**Two of roughly fifteen.** If every defect from those two days were
reintroduced today, the guard corpus would catch two:

- `contract-null-optional` (a kept guard's contract failed to load, so the
  guard silently stopped guarding, a fail-open in the artifact the CI gate
  depends on)
- `check-flag-callers` (repository callers passed a flag the vocabulary purge
  deleted, so guard replay exited 2 and read as flake)

The other thirteen would not be caught, and the reasons are below. Two of
fifteen is a 13 percent capture rate on this project's own defects. It is
progress from zero, and it is not a good number.

## What remains unguardable, and why

Grouped by the capability that is actually missing, not by symptom.

**1. Defects in SDKs for other languages** (Ruby's swallowed divergence marker,
Ruby replay resolving DNS, PHP's seeded-stream overflow, .NET's UTF-8 BOM, Go's
unsorted header cap, iOS and React Native's wrong trigger token). Each is
caught today by that SDK's own acceptance script. A guard could wrap those
scripts the same way the two new guards wrap a verifier, and that is the
cheapest available expansion. The reason it has not been done is that each
needs its language toolchain present wherever the corpus runs, which the guard
corpus currently does not assume.

**2. Defects that only appear on a real device or emulator** (Android's
null-payload capsule infidelity, Android's capsule delivery race). These need a
booted emulator, an installed app, and controllable ingest latency. The guard
corpus runs on a developer machine and in CI with neither. Closing this needs
the corpus to be able to declare a guard as environment-gated, so it can be
required where the environment exists and honestly skipped, loudly, where it
does not. That capability does not exist: today a guard is either required
everywhere or not required at all.

**3. Defects in CI configuration itself** (missing `libatspi2.0-dev` in the
cloud test job, the dogfood-policy job lacking a Rust toolchain, the React
Native jest resolution that passed locally and failed on CI). A guard runs
inside one environment and cannot observe another. The `exception:
environment-unreachable` declaration exists precisely because this class is
unreproducible from here.

**4. Defects in the process capsule's own boundary** (missing LFS aliases,
`openat` falling through to the live filesystem, glibc stdio bypass, the silent
empty replay). These are measured by `validation/process/run.sh`, which needs
Linux and a built shim. Same shape as group 1: wrappable once the corpus can
carry an environment-gated guard.

**5. Test-infrastructure defects** (two temp-directory races, the `errexit`
leak in an acceptance harness, two acceptance scripts that could false-pass on
exit code alone). A guard asserting "this harness cannot silently stop early"
is expressible in principle, and is the most valuable unclaimed guard in this
list, because this failure class has now appeared **four separate times** in
this project. Not built yet.

**6. Defects in the honesty gate itself** (the exception declaration bound to
nothing). Guarded today by the policy script's own unit suite, which is the
right place for it. A guard would be a second copy of the same assertion.

## Can a process capsule be kept as a guard? Not yet

The directive asked. Measured answer:

- `reproit check <capsule> --exec "<command>"` **works**: `check.rs` routes a
  file whose `format` is a process capsule to `process_capsule::check_exec`,
  reusing the four-way verdict vocabulary.
- `reproit keep` has **no process-capsule route**. It handles a backend capture
  file, an `occ_` id, and a finding id. A process capsule matches none of them,
  so a captured process session cannot be persisted into `.reproit/repros/` and
  therefore cannot join the required corpus.

**What is missing, precisely:** a branch in `keep_command.rs` that recognises a
process capsule (`process_capsule::is_process_capsule`) and lands it as a guard
carrying its `--exec` recipe, exactly as the hermetic backend capture path
already does. That file is owned by another workstream, so this is reported
rather than edited.

**A second constraint, worth knowing before that is built:** process capture
and replay both need the LD_PRELOAD shim, which is Linux only. Today
`validation/process/run.sh` skips on macOS with "LD_PRELOAD injection through
sh is Linux only". So a process-capsule guard would be red or skipped on a
developer Mac and green in CI, which is exactly the environment-gated guard
capability group 2 above also needs. Building that gating is a prerequisite,
not an afterthought.

The two guards added here therefore use the existing trusted-provider and
exit-code observation mechanism, which is portable, already proven in CI, and
does not depend on the shim.

## A defect found while building these guards

The first version of `cli-defect-verifier.mjs` decided the contract check with
a single regex over the subject's output: absence of `invalid type: null` meant
clean. That is a fail-open. A binary that never ran at all also fails to print
that string, so a missing binary, or the `SIGKILL` macOS delivers to a
code-signed binary overwritten in place, would have been reported as a healthy
subject. Measured: exit 137 from the swapped binary, and the verifier said
`reproduced: false`.

The verifier now demands positive evidence that the replay was actually reached
and exits 70 when it cannot decide. This is the fourth appearance of the same
failure class in this project: **a harness that stops early looks exactly like
one that passed.** It is worth a standing rule rather than another fix.

## What would move the number

In rough order of value per unit of work:

1. **Environment-gated guards.** One capability unlocks groups 1, 2 and 4,
   which is most of the thirteen. A guard needs to declare what it requires
   (a toolchain, an emulator, Linux plus the shim) and be required where that
   holds and loudly skipped elsewhere, never silently.
2. **A guard over harness honesty**, asserting every acceptance script pins a
   case count and cannot report success after stopping early. Four occurrences
   justify it on its own.
3. **`keep` accepting a process capsule**, which makes ReproIt guardable by the
   same general-program machinery it sells, and is the path to guarding the
   binary's behaviour rather than its callers' text.
