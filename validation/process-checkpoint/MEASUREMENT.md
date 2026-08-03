# Checkpoint anchoring (Class C): what was measured

Two anchor kinds exist and must never be confused. The criu kind below
(everything from "The survey" down) checkpoints the REPLAYING process and is
an investigation accelerator that can never verify a fix. The APPLICATION
kind, measured 2026-08-01 in the next section, is the plan's preferred path:
the checkpoint the program itself wrote, which the new binary can load, so a
tail flipping to a clean exit is a real fix verification.

## The application anchor (measured 2026-08-01)

The anchor is an artifact the program already writes (a trainer's save file),
and the capture runs the program's own resume invocation, so the boundary log
covers the tail from the anchor forward BY CONSTRUCTION rather than by
splitting a longer log at an ordinal nobody measured. The capsule gains an
additive versioned `anchor` section: the checkpoint by digest plus embedded
bytes (16 MiB embed cap, the shim's REPROIT_FILE_CAP, one number for the
whole size story; past it a named over-cap marker), the ordinal position, and
the uncontrolled-sources statement. Anchor-less capsules load and replay
unchanged (gate case 2 pins that).

Reproduce with `validation/process-checkpoint/gate-anchor.sh` (self-wrapping
in Docker; image and cargo volume shared with gate-session.sh). Fixture:
`fixtures/trainer-checkpoint-fixture/trainer.c`, a scalar SGD loop over 400
one-line samples that checkpoints every 50 steps and dies on a declared
assertion at step 380 (poisoned label), so the last checkpoint sits at 350,
near the failure.

| case | linux/arm64 (seccomp layer) | linux/amd64 (emulated, libc only) |
| --- | --- | --- |
| docker image / glibc | rust:1.97.1-trixie, glibc 2.41 | rust:1.97.1-trixie, glibc 2.41 |
| baseline: failure at step 380, own checkpoint at 350 | PASS | PASS |
| additive: anchor-less capsule still replays | PASS | SKIP by name (below) |
| anchored capture: digest + position + statement stored | PASS | SKIP |
| portability: clean copy, other absolute path, ckpt deleted and materialized back | PASS reproduced | SKIP |
| head skipped: no head step line in the tail replay | PASS (350 of 400 skipped) | SKIP |
| fix: TRAINER_FIXED=1 flips the tail to exit 0 | PASS | SKIP |
| tampered checkpoint digest refuses by name, program never ran | PASS anchor-checkpoint-digest | SKIP |
| deleted boundary read diverges naming the file | PASS incomplete-file | SKIP |
| statement verbatim in output == statement in artifact | PASS | SKIP |
| gate total | 9/9 | 1/1 + named skip |
| criu availability (for the criu kind, unchanged) | criu 3.17.1 "Looks good" in privileged bookworm, re-probed 2026-08-01 | not re-probed; prior survey stands |

The amd64 SKIP, measured before it became a skip: Docker's x86_64 emulation
answers EINVAL to `SECCOMP_SET_MODE_FILTER` with `NEW_LISTENER` (the
emulator's limit, per validation/process/MEASUREMENT.md, where a real x86_64
kernel installs the layer and passes the full completeness gate), so the shim
falls back to the libc boundary as a named event. The trainer reads through
stdio (`fopen`/`fscanf`), whose internal reads bypass the interposed `read`,
so the libc-only capture held the data file's open (7196 bytes) and no
content, and the replay refused LOUDLY: `incomplete-file recorded=7196
served=0`, exit 3, divergence naming the file. Fail-closed held exactly as
designed; the gate now probes the layer first (the same probe
gate-completeness.sh uses) and skips rows 2-9 by name on a layer-less host.

The statement, stored in every anchored capsule and printed verbatim with the
verdict (`crates/reproit/src/workflows/process_capsule/anchor.rs`):

> UNCONTROLLED-SOURCES pinned: file reads, socket reads, clock reads, and RNG
> draws observed at the boundary; the environment block; the replay seed; the
> checkpoint artifact by digest. not pinned: in-process RNG state carried
> inside the checkpoint, thread scheduling, GPU kernel execution,
> floating-point reassociation across builds. The tail replays under the
> pinned sources only; this is never a bit-exact re-execution claim.

Named limits of the application kind, honestly:

- The position ordinal is DECLARED by the operator (`--anchor-position`), not
  measured out of the program; the trainer prints "resumed from checkpoint at
  step 350" and the gate cross-checks the two, but a program that lies about
  where its checkpoint sits is not caught.
- The fix flip is real only because an application checkpoint is data; it
  holds for a fixed BINARY loading the same checkpoint format. A fix that
  changes the checkpoint format itself invalidates the anchor (the resume
  refuses or diverges), which is the correct failure but is not a verdict.
- The over-cap arm (checkpoint past 16 MiB) is unit-tested
  (`process_capsule::anchor::tests`), not gate-exercised; the gate's
  checkpoint is bytes, not gigabytes, on purpose.

Platform for every number: Linux aarch64 in Docker (Docker Desktop VM, kernel
6.12), `criu` 3.17.1 from Debian bookworm, `--privileged`. Reproduce with
`validation/process-checkpoint/run.sh` inside such a container.

**The bookworm part is load bearing, not incidental.** Measured 2026-07-31:
`criu` 4.1.1, the version Debian trixie ships, HANGS in `criu restore` on this
host until it is killed, so the restored tail never resumes. criu 3.17.1
restores the same image fine. This was measured with a plain looping C program
and no reproit in the picture at all, with and without `--restore-detached`:

| criu | `--restore-detached` | plain `restore` |
| --- | --- | --- |
| 3.17.1 (bookworm) | exit 0, tail 87 -> 216 | tail 88 -> 1371 |
| 4.1.1 (trixie) | hangs, tail 88 -> 88 | hangs, tail 87 -> 87 |

The product behaves correctly under the hang: it bounds the wait and refuses
with `criu restore did not return within 120s`, verdict inconclusive, exit 3.
`run.sh` now recognizes that reason and fails naming criu and the environment,
because the earlier message ("the restored tail did not advance") read like a
product regression and was not one. Note also that the reproit binary must be
built against the SAME image it runs in: a trixie built binary needs GLIBC_2.39
and dies at exec on bookworm, which `run.sh` now reports as a loader error
rather than as a failed capture.

The question this work exists to answer: can a capsule skip the head of a long
run, so a failure at minute 340 of a six hour run is reachable without
replaying the first 339?

## The survey, before any design

CRIU was measured, not assumed, and three of its four answers shaped the design.

| case | result |
| --- | --- |
| `criu check` in privileged Docker | "Looks good" (veth and UFFD warnings only) |
| dump and restore a process with open FILES, 3 attempts | **3 of 3 succeeded** |
| dump a process with an established TCP connection | **REFUSED** |
| dump a process whose files live on a bind mounted host path | **REFUSED** |

The socket refusal is verbatim, and it is the one that decided where a
checkpoint may be taken:

```
Error (criu/sk-inet.c:189): inet: Connected TCP socket, consider using --tcp-established option.
Error (criu/sk-tcp.c:77):   tcp: Failed to lock TCP connection 10d51d13
```

Both spellings fail. A long-running server or trainer is exactly the program
that holds connections, so **the original run cannot be checkpointed**.

The bind-mount refusal is an artifact of a macOS host, and worth recording so
nobody loses an afternoon to it:

```
Error (criu/files-reg.c:1710): Can't lookup mount=56 for fd=1
  path=/run/host_virtiofs/private/tmp/.../out.log (deleted)
```

## The design that follows from the survey

The checkpoint is taken of the REPLAYING process, not of the original run.
Under the shim every socket is served from the capsule, so no live connection
exists and criu's refusal does not apply. The anchor is a by-product of one
slow replay, and every replay after it skips the head.

The boundary cursor is not a number this module tracks. The replaying process
holds the shim's cursor in its own memory, so a checkpoint of that process
carries the cursor with it. The anchor records the OBSERVABLE position (how far
the program's own output had got) for a human, plus two digests:

- `capsuleSha256` binds the anchor to the capsule it came from, computed over
  the capsule WITHOUT its anchor field so writing the anchor does not
  invalidate it;
- `imageSha256` covers the image contents, so an edited, truncated, or
  partially written image is refused rather than restored into an unknown
  state.

## A second refusal, found while building

`criu` cannot dump a process holding a seccomp notify descriptor, which is
exactly what the completeness layer installs:

```
Error (criu/files-ext.c:94): Can't dump file 3 of that type [600]
  (anon anon_inode:seccomp notify)
```

So anchoring runs on the libc-only boundary. That has a consequence measured
directly: the two boundaries key a file entry differently, the seccomp layer by
the resolved absolute path and the libc layer by the string the program passed.
A capsule recorded under one cannot be served by the other when the program
uses a relative path. The subject therefore takes an absolute config path, and
with that the same capsule serves both.

## The acceptance, measured

400 iterations, the config file DELETED before every replay, anchor requested
at 350 lines of the subject's own output.

| step | measured |
| --- | ---: |
| capture | 802 boundary entries |
| replay from zero | 2683 ms, verdict **reproduced** |
| anchor taken at | 353 lines |
| restored tail resumed | 356 to 400 lines, in about a second |
| head skipped by the anchor | **356 of 400 iterations** |

The tail resumes and runs to completion. The head is genuinely skipped, which
is the property an anchor exists for.

## What does NOT work, precisely

**The restored tail's outcome is not observable, so the verdict is
INCONCLUSIVE.** Two measured facts combine:

1. `criu restore` exits 0 even when the restored task dies on a fatal signal.
   Measured directly with a subject that aborts: restore returns 0 while the
   task's own output ends at `aborting`. So criu's exit code cannot be the
   oracle.
2. The workaround, a shell wrapper that publishes its child's status, does not
   survive. `sh -c "cmd; echo $? > status"` does fork (verified: sh and the
   subject are two processes), and the shell is in the image, but after restore
   the status file is never written while the subject's output reaches 400
   lines within a second. `--restore-detached` does not change it.

So the tail demonstrably runs, and nothing can currently name how it ended.
The product reports `inconclusive` and exits 3 rather than guessing, and
`run.sh` pins that as a case so it cannot silently become a false pass.

`--restore-sibling` plus a shell `wait` was also tried and returns 127, the
shell reporting the pid is not its child.

Closing this needs the restored tail's status observed some other way: reaping
the restored pid directly from the tool (which needs waitpid on a pid the tool
did not spawn), or a criu build that forwards the task's status. It is NOT
done, and no part of the product claims a reproduction from an anchor today.

## Fail closed, verified

Every one of these is refused with a named reason and exit 3, never restored:

- a capsule with no anchor;
- an anchor whose `kind` this build cannot restore (an application-level save
  survives a rebuild and a criu image does not, so the two must never be
  confused);
- a checkpoint image that is absent;
- a checkpoint image whose bytes changed, including one extra file;
- an anchor whose capsule changed, which is what an edited boundary log
  produces.

## The limit that matters most for how an anchor may be used

A criu image contains the program's memory, including its code. Restoring it
re-runs the OLD binary. **An anchor accelerates investigating a failure and
must never be used to verify a fix**; verification replays from zero against
the new binary. This is why the fix-verification path never consults an anchor,
and why an application-level anchor (a program's own save file, which engines
and trainers already have) is a different `kind`: that one survives a rebuild,
and conflating them would let a fix be "verified" against the code it replaced.
