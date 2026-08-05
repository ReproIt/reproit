# Self-dogfood status: what Repro It actually guards about itself

Written 2026-07-31. This is a scoreboard, not marketing. It is meant to be
usable as evidence against us, so every number here is the honest one and the
gaps are stated in more detail than the wins.

## The measured starting point

Roughly fifteen defects were found and fixed in this project on 2026-07-30 and
2026-07-31. **Repro It guards caught zero of them.** What caught them was
acceptance scripts, CI, the protocol validator, and running on real devices and
emulators. The declaration ledger showed it plainly: six `not-a-fix`, five
`exception`, three `no-repro`, and **zero `guard:`**.

That is not laziness. It is structural. A guard could only express a
request-shaped API contract violation, and almost none of this project's own
defects are that shape.

## Guard inventory

Every entry uses the `exact-occurrence` oracle.

| guard | alias |
| --- | --- |
| `rep_b6f7c439ee73` | self-dogfood-required-corpus-dispatch |
| `rep_bc3102f3a330` | self-dogfood-cli-backend-root |
| `rep_adb0d03115cf` | self-dogfood-direct-push-policy |
| `rep_a0c5982cf686` | self-dogfood-contract-null-optional |
| `rep_5e6e817c4d5c` | self-dogfood-check-flag-callers |

Five required guards, up from three. The two new ones are the first guards in
this project that assert **behaviour of the reproit binary and its callers**
rather than CI wiring.

Both new guards are proven in both directions, end to end through
`reproit check`, not merely at the verifier:

```
HOLD   run-required-guards.py            all 5: pass, not_reproduced x3 each
RED 1  pre-fix binary swapped in         FAIL rep_a0c5982cf686  exit 1
RED 2  deleted flag reintroduced         FAIL rep_5e6e817c4d5c  exit 1
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

## Can a process capsule be kept as a guard? Yes, but none is required yet

The directive asked. Measured answer:

- `reproit check <capsule> --exec "<command>"` **works**: `check.rs` routes a
  file whose `format` is a process capsule to `process_capsule::check_exec`,
  reusing the four-way verdict vocabulary.
- `reproit keep <capsule> --exec <command> --strict` now recognizes the process
  capsule, proves its current four-way verdict, and persists `capsule.json`, a
  checkout-authorized `hermetic.json` recipe, and required guard metadata.
- `reproit check <guard>` detects that retained format and replays it without a
  capsule path or repeated command. Unit and Linux process gates cover this
  path.

**What is missing, precisely:** the required self-dogfood corpus still has no
process-capsule guard. The current corpus is platform-neutral. The `LD_PRELOAD`
capture and replay mechanism is Linux-only. Guard metadata needs a typed
environment requirement. The corpus runner must require Linux for this guard.
It must report the guard as not applicable on other hosts. It must not report a
pass on those hosts.

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
3. **Promote a retained process capsule into the required Linux corpus.** The
   keep and check routes now exist. The remaining work is typed environment
   selection plus a committed affected-versus-fixed process guard.

## Update 2026-08-05: the corpus dispatch is the product path

The corpus replay step in CI is now plain `target/debug/reproit check`. That
is the exact command a customer copies. `run-required-guards.py` is deleted.
Its four honesty checks moved into the product, where every user gets them:

- Suite enumeration fails closed (`domain/repro/corpus.rs`). A store
  directory that is not content-addressed fails the run. So does a missing or
  mismatched meta.json, and so does a guard with no replay route. No guard
  can drop out of the run.
- Kept process-capsule and hermetic-capture guards replay as suite cases. No
  guard format can vanish from `reproit check`.
- Guards now carry a typed environment requirement (`requires` in meta.json,
  closed vocabulary, currently `os`). When the requirement does not hold,
  `check` reports the guard as NOT APPLICABLE. It never counts it as a pass.
  Group 2 above named this capability as the prerequisite for a required
  process-capsule guard. The corpus can now hold a linux-only guard honestly.
- The app (map refresh, device selection) boots only for work that drives it.
  A suite of source-neutral guards checks without an app build in CI.

The flag vocabulary of `check` shrank to match. `--strict`, `--junit`,
`--service`, `--changed`, and `--auto` are gone. Each had zero real callers.
The `required-corpus-dispatch` and `direct-push-policy` guards were re-pinned
over the updated verifier. Identity, alias, and history survived.

Still open from the list above: wrap SDK acceptance scripts as guards
(group 1), and promote a retained Linux process capsule into the required
corpus (group 4). Environment gating unblocks both.
