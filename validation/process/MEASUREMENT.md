# Process capsule: what was measured

Platform for every number below: Linux (aarch64 host via Docker), glibc 2.36,
`gcc:13` image. Reproduce the shim numbers with the measurement harness and
the end to end verdicts with `validation/process/run.sh` (needs a Linux CLI
binary via `REPROIT_BINARY`).

The question this work exists to answer: can a boundary record enough of a
real program that replay does not diverge spuriously, and never replays WRONG
in silence?

## The table

Measured with the seccomp completeness layer active (the default on Linux;
set `REPROIT_SECCOMP=0` to force the libc-only boundary).

| program | entries | divergences | stdout byte identical | verdict |
| --- | ---: | ---: | --- | --- |
| C program, plain `open`/`read` | 3 | 0 | yes | replay correct |
| C program, `-D_FILE_OFFSET_BITS=64` | 3 | 0 | yes | replay correct |
| coreutils `cat` | 3 | 0 | yes | replay correct |
| `python3` script | 782 | 0 | **yes** | REPRODUCES hermetically |
| `ruby` script | 1779 | 0 | **yes** | REPRODUCES hermetically |
| C program opening one relative name twice | 5 | 0 | **yes** | REPRODUCES hermetically |
| C program, `gcc -static` | 0 | 0 | n/a | capture REFUSED before it runs |
| the same static image behind `/bin/sh` | 6 | n/a | n/a | capture REFUSED as INCOMPLETE |

Every replay above ran with the input file DELETED.

For comparison, the same `python3` case with the completeness layer disabled
(`REPROIT_SECCOMP=0`) reports ZERO divergences and still replays wrong. That
contrast is the whole argument for the layer: the failure did not become
correct, but it stopped being silent.

Three rounds of fixes moved python from 2 divergences to 1 and closed two real
bug classes on the way (below). It did NOT reach a correct replay. The
remaining cause is named, with evidence, rather than guessed.

## Phase 2: a timed input stream (measured)

A session shaped program's trigger is input arriving OVER TIME, not a single
request. The capsule now stamps every input read with the TICK it arrived on,
and replay holds an input back until the program reaches that tick again.

The tick is the ordinal of the program's clock reads. That choice matters:
replay serves clock reads from the capsule IN ORDER, so the Nth clock read at
replay is the Nth clock read of the recording, which makes the ordinal aligned
between the two runs without the program having to expose a frame counter. A
fixed timestep loop reads the clock once per frame, so the ordinal counts
frames.

**What a program must do to be replayable this way.** It has to take its time
from the clock (a fixed timestep loop already does) and poll its input rather
than block on it. A program that blocks on input without ever reading its clock
cannot be scheduled, and is served early with `inputEarly` counted rather than
being quietly reordered. Frame perfect replay is not free and this says so.

### The acceptance, and why it is discriminating

`validation/process/engine.c` is a fixed timestep loop on SDL2: SDL's timer,
SDL's event pump, input on stdin because a container has no evdev or X11 and an
engine that cannot run headless cannot be tested. Its planted defect is a STALE
COMBO that fires only when presses arrive FAR APART.

That direction is deliberate. The same bytes back to back are SAFE, so a replay
that delivered the recorded input immediately would NOT reproduce the crash.
The test therefore fails if the schedule is ignored, which is what makes it a
test of timing rather than of bytes. `run.sh` asserts the premise first.

| program | entries | input events | divergences | record | replay | stdout identical |
| --- | ---: | ---: | ---: | --- | --- | --- |
| SDL2 engine, presses 0.25s apart | 1479 | 2 | 0 | exit 134 | exit 134 | yes |
| SDL2 engine, same bytes back to back | n/a | n/a | n/a | exit 0 | n/a | n/a |

A surviving run of the same engine reports `inputServed=3, inputEarly=0,
ticks=714, clockOverrun=0` from the target process. The crashing run reports
nothing, because a program that dies on a fatal signal never runs the reporting
destructor; the divergence LINES are the authority there, as before.

The track 5 acceptance built on top of this machinery (a third-party engine
sample, the portability bar, the fix flip, and a tick tamper that refuses by
name) is measured in `validation/session/MEASUREMENT.md`, gated by
`validation/session/gate-session.sh`.

### Two defects this phase exposed

**A replay that OUTLIVES its recording used to hang.** Once the capsule's clock
entries ran out, the served clock advanced by one nanosecond per call, so a
frame loop waiting for five milliseconds of wall clock spun forever. That is
exactly the shape of a FIXED program: it no longer crashes, so it runs longer
than the recording did. Past the end of the recording the served clock now
continues from the last recorded instant at the REAL elapsed rate, which keeps
the program live; `clockOverrun` already reports that the run went past what
the capsule describes.

**The record log appended instead of truncating.** A capsule describes ONE
session, and a stale log silently merged two runs into a capsule that never
happened. The CLI always passes a fresh temp path, so this only bit a hand run,
which is exactly when a confusing capsule is hardest to spot.

**Usage note found the same way:** a shell redirect inside `--exec`, such as
`< /dev/null`, is itself an open the recording never made, so the boundary
correctly diverges on it. Replay serves stdin from the capsule, so the redirect
is unnecessary as well as wrong.

## Phase 3: the oracle vocabulary for programs (measured)

Class A oracles are HTTP shaped. A process capsule judges how a program DIED,
so it needs its own. Three are now first class registry ids rather than free
strings, which they had been: `process-signal`, `process-exit`, and
`process-assertion`. Phase 1 had been stamping `process-signal` and
`process-exit` onto findings while the registry's own note says every emitted
`oracle` value is one of its ids, so that was a live contract violation.

### The false proof this closed

Every failed assertion dies with `SIGABRT`. The verdict compared only the
signal and the exit code, so a replay that aborted for a COMPLETELY UNRELATED
reason was reported as a reproduction. That is a false proof in the one
direction this product must never get wrong.

A capsule now records the program's own failure text, normalized, and a replay
must produce the same one. Measured on two different assertions in one binary,
both dying with signal 6:

```
recorded oracle:  process-assertion
recorded failure: two: twoasserts.c:16: main: Assertion `n < 8 && "thrust budget exceeded"' failed.

WITH identity, different assertion -> exit 3   (INCONCLUSIVE, correct)
WITHOUT it (the old behaviour)     -> exit 1   (FALSE reproduction)
```

The second line is a negative control: the identity was stripped from the same
capsule to confirm the check is what closes the gap, rather than assuming it.

**Only hexadecimal addresses are folded** when comparing failure text. Folding
decimal digits as well was tried and REJECTED because it made
``Assertion `n < 8'`` and ``Assertion `n < 9'`` compare equal, which is the
same false proof arriving by a different route. A record and its replay run the
same binary, so file names, line numbers, and the predicate are all stable and
comparing them literally is safe. When a signature must be loosened the cost is
always paid in the direction of calling two different failures one, so the bias
is to fold as little as possible.

## What the syscall layer changed

The libc boundary only sees calls that cross the dynamic linking boundary. A
libc that calls its own `open` or `stat` internally never crosses it, so a
python3 replay was measured serving 5 files from the capsule while the loader,
locale, and gconv paths fell through to the LIVE filesystem with zero
divergences. That silently violated the fail closed contract: replay was not
hermetic and did not say so. `strace` confirmed real descriptors coming back
from `openat` during a supposedly hermetic replay.

`runners/process-shim/reproit_seccomp.c` supervises those calls where they all
converge. The division of labour is now:

- libc shim, the fast path: clock, randomness, environment, sockets.
- seccomp user notify, the completeness layer: files and path metadata
  (`openat`, `openat2`, `stat`, `lstat`, `newfstatat`, `statx`, `access`,
  `faccessat`, `faccessat2`, `readlink`, `readlinkat`, `getcwd`,
  `getdents64`), whoever called them.

When the supervisor is live the libc file interposition steps aside entirely,
so each class has exactly one source of truth. That mattered: with both layers
recording, a file's bytes were stored twice and replayed concatenated, which
the C subject exposed as `read:boomboom` instead of `read:boom`.

Replay serves a file by writing its recorded bytes into a `memfd` and
injecting that descriptor with `SECCOMP_IOCTL_NOTIF_ADDFD`, so the program's
later `read`, `lseek`, and `fstat` are answered by the kernel and cannot
diverge on chunk size.

## Serving real files, not memfd copies

The wall below was diagnosed as the memfd itself, and replacing it moved both
runtimes. Replay now materializes recorded content as REAL files in a scratch
tree and injects descriptors to those, because two things a copy cannot fake
depend on it: glibc validates a locale object structurally, and the dynamic
loader maps a shared object PROT_EXEC and relocates it, which kernels that
default memfds to noexec refuse outright. The scratch tree is torn down when
the target exits, and an unrecorded path still DIVERGES rather than falling
through to the host.

Effect, measured:

- `python3`: 1 divergence to **0**, and the CLI acceptance now asserts a real
  hermetic reproduction with the input file deleted, not a fail closed.
- `ruby`: 68 divergences and a segfault to **1** divergence, no crash.

The change also exposed a real bug in the supervisor: `LEAVE()` clears the
re-entrancy guard, so after its first `open` the supervisor began serving
ITSELF from the capsule and diverged on its own scratch paths. The memfd path
never called `open`, which is why it had stayed hidden. The supervisor now
latches a flag `LEAVE()` cannot clear.

**The python3 double stdout caveat is CLOSED, and it was stale.** It was
recorded before the real file serving change and never re-measured after it.
The decisive measurement is per writer: tracing `write(2)` on fd 1 through a
replay shows ONE pid performing ONE write of `read:boom\n`, and six
consecutive replays each produced exactly one line. `cmp` of the recorded and
replayed stdout is now an assertion in `validation/process/run.sh`, so the
claim is pinned rather than remembered.

The lesson is the one this file exists for: a caveat carried forward without
re-measurement is indistinguishable from a live defect, and this one had
already been fixed by a change made for an unrelated reason.

## What still does not work: the wall, with evidence

**HISTORICAL, now closed by real file serving.** `python3` used to fail with
one divergence, diagnosed as follows and fixed by the change described above.

```
REPROIT:DIVERGENCE {"kind":"file","detail":"/usr/lib/locale/UTF-8/LC_CTYPE"}
```

`strace` of the RECORDED run shows glibc's locale search resolving in this
order, ending in a successful load:

```
openat("/usr/lib/locale/locale-archive")   = -1 ENOENT
openat("/usr/share/locale/locale.alias")   = -1 ENOENT
openat("/usr/lib/locale/C.UTF-8/LC_CTYPE") = -1 ENOENT
openat("/usr/lib/locale/C.utf8/LC_CTYPE")  = 3        <- succeeds
```

The capsule captured that whole sequence, including the successful
`C.utf8/LC_CTYPE` with its content. At replay the interpreter instead reaches
for `/usr/lib/locale/UTF-8/LC_CTYPE`, a path the recorded run never touched.

That name is the tell. CPython's PEP 538 locale coercion tries its targets in
order, `C.UTF-8`, then `C.utf8`, then `UTF-8`. The recorded run stopped at the
second because `setlocale` succeeded. The replayed run reaches the third,
which means `setlocale` FAILED on the served copy: glibc's locale loader
validates an `LC_CTYPE` file through its mmap and its metadata, and the copy
this layer serves out of a memfd does not satisfy that validation. The capsule
has the bytes; the loader rejects the object.

Closing it needs the locale object served as a real file whose mapping and
metadata match the recording, not a memfd copy. That is the next honest
increment and it is NOT done.

**`ruby` reaches zero divergences and still does not replay.** The last
divergence was `/var/lib/gems/3.1.0/specifications/default`, and the capsule
DID hold the answer: the recording opened the parent
`/var/lib/gems/3.1.0/specifications` and got ENOENT (`a = -2`). A directory
observed as absent cannot contain anything, so replay now answers ENOENT from
that recorded fact instead of reporting drift that never happened. See the
section below on answering from the recording.

**HISTORICAL. The "different search ORDER" diagnosis above was WRONG, and
measuring it instead of trusting it is what closed the case.** It said ruby's
replay resolved libraries in a different order, so `require` loaded one file
under two names, and that closing it meant pinning the interpreter's search
order. `strace` of the replay says otherwise: the second open is at the SAME
path as the first,
`/usr/lib/ruby/vendor_ruby/rubygems/defaults/operating_system.rb`, so nothing
about the order moved.

The capsule was the problem. Counting bytes by key in the ruby capsule:

```
/usr/lib/ruby/vendor_ruby/rubygems.rb   opens 2   recorded bytes 74,490
file on disk                                      37,245
```

Exactly twice. The syscall layer re-reads a whole file on every `openat`, and
replay gathered every read of a key across the WHOLE log, so a file opened
twice was served containing its own text twice. Rubygems duly warned
`already initialized constant Gem::MARSHAL_SPEC_DIR` at line 2644 with the
previous definition at 1295, exactly 1,349 lines apart, which is the length of
the file. Debian's `alias upstream_default_path default_path` then ran a
second time, aliased itself, and recursed:

```
rubygems/defaults/operating_system.rb:83:in `default_path':
  stack level too deep (SystemStackError) ... 9347 levels...
```

A file opened twice is TWO streams. Replay now serves the reads that followed
THIS open and stops at the next open of the same key, and ruby reproduces with
its input deleted and byte-identical stdout, like python. Both properties are
pinned as cases in `validation/process/run.sh`.

The lesson is the same one this file keeps recording, in the sharper
direction: an abstention with a plausible named cause is still only as good as
its last measurement, and this one had been carried forward for three rounds
while the real defect sat one byte count away.

The four-way distinction the abstention used to pin is not lost. The
`twoasserts` case still asserts that a replay dying the same way for a
DIFFERENT reason is `INCONCLUSIVE` and not a reproduction, and the tampered
capsule case still asserts `DIVERGED`.

## Relative paths: one file with two keys, two files with one

Boundary entries were keyed by the path AS WRITTEN in the libc layer, which
broke in both directions at once. Measured on a subject that reads `data.txt`
from a directory, chdirs, and reads a different `data.txt`:

```
recorded log:   open data.txt / read data.txt(OUTER) / open data.txt / read data.txt(INNER)
live run:       A=OUTER   B=INNER
replayed run:   A=<ERR>   B=OUTERINNER
                REPROIT:DIVERGENCE {"kind":"file","detail":"/work/case/data.txt"}
```

- `A` is ONE file with TWO keys: record stored `data.txt`, replay resolved it
  against the cwd, and the lookup missed a file whose bytes the capsule held.
  A spurious divergence, which is the safe direction but still wrong.
- `B` is TWO files with ONE key, and it is the unsafe direction: the two files
  were concatenated and served as one, with ZERO divergences. A silent wrong
  replay.

Both close with one key. `reproit_path_key` resolves against the cwd or the
dirfd (through `/proc`) in BOTH modes, normalizes the identity-preserving
rewrites (`//`, `/./`, a trailing `/`), and folds an over-long path to a hash
plus its tail rather than truncating, because truncation is one more way for
two files to share a key.

`a/b/..` is deliberately NOT folded. The kernel resolves `..` after following
symlinks, so folding it lexically would make `/a/link/../b` key as `/a/b`, a
DIFFERENT file. That would convert a normalization meant to merge two keys for
one file into two files sharing one key, which is the exact failure being
fixed. A symlink and its target therefore still key apart, and under replay
both spellings simply diverge, which is the safe direction.

The capsule already recorded the working directory and replay ignored it.
Replay now runs in it, so a guard kept in a repo and checked from the repo root
still resolves relative names the way the recording did. A recorded working
directory that no longer exists is `INCONCLUSIVE` with that named cause, never
a pass and never a reproduction.

## Static binaries: measured, and now refused in three places

`gcc -static` produces a working binary and the boundary observes **0
entries**, exactly as predicted: a statically linked program resolves no
dynamic symbols, so the libc half of the boundary is never called.

The seccomp half is a different story, and measuring it changed the answer.
A seccomp filter SURVIVES `execve`, so a static image launched by a dynamic
parent is still supervised for files and path metadata. Measured, `/bin/sh`
exec'ing a static subject produced a capsule of six entries that carried the
subject's input file and replayed as a clean `reproduced` with that file
deleted:

```
open   /tmp/reproit-subject/input.txt
read   /tmp/reproit-subject/input.txt   boom
```

That capsule is not wrong about what it holds. It is wrong about what it does
NOT hold: the libc classes, clock, randomness, environment, and sockets, are
unobserved inside a static image and nothing in the capsule says so. The same
program with one socket dial would have replayed against the LIVE network with
zero divergences.

So capture refuses in three independent places:

- before the program runs, by reading its ELF program headers: no `PT_INTERP`
  means no dynamic loader, and capture stops with a named reason;
- after it runs, if the boundary observed nothing at all, which also catches a
  loader that dropped the preload for other reasons;
- when the supervisor sees the target EXEC into a statically linked image, read
  from `/proc/<pid>/exe` on image change rather than by trapping `execve`, so
  the filter and its hot path are untouched. Capture then names the capture
  INCOMPLETE in the classes it cannot report, rather than shipping it.

A syscall-only capture path is still a real option, and it is still NOT
implemented. What changed is that the gap can no longer be reached by accident
through a wrapper.

## Keeping a capsule: the loop was open at the middle

The capsule could FIND a failure and REPRODUCE it, and there was no route from
one into `reproit keep`, so it could never become a regression test. That is
the product's whole loop broken for exactly the programs this format exists to
serve. Routing was the entire gap: `keep` sniffed `reproit-backend-capture`
and a process capsule fell through to the finding lookup, which failed with
"unknown finding" on a file that was sitting right there.

A capsule now keeps exactly as a backend capture does, and for the same
reasons, with `--exec` required because a capsule may never supply its own
command:

```
$ reproit keep capsule.json --exec ./subject
Kept process capsule guard 56f8bf52b0d6
  verdict now: reproduced
  reproit check rep_56f8bf52b0d6 replays it hermetically, ...

$ reproit check rep_56f8bf52b0d6
  FAIL reproduced by re-execution (fatal signal 6 on process-assertion)
```

Three things this made explicit rather than assumed:

- The guard is PROVEN LIVE at keep time. A capsule whose current verdict is
  diverged or inconclusive is refused with the verdict named, because a guard
  that cannot replay is dead on arrival in CI.
- The guard file is `capsule.json`, not the backend guard's `capture.json`, so
  `reproit check <id>` routes on a lookup rather than a sniff and the two
  formats cannot be confused for one another.
- `check` resolves a repro by its PREFIXED id, so keep prints `rep_<id>`.
  Printing the bare directory name would have handed the operator a string
  that does not resolve, which is how this was found.

## The environment block and directory listings

Two of the three fixes this round were aimed squarely at the interpreter case
and both landed, even though the case still does not pass:

- **The environment is pinned as a whole block.** Capture records the full
  environment minus secret shaped names, and replay CLEARS the inherited
  environment before restoring it, so an interpreter cannot resolve a
  different prefix or locale from a variable the recording never had. It also
  stops serving `getenv` from the capsule when the block is pinned, because a
  stale snapshot hid the program's OWN `setenv` writes; CPython coerces
  `LC_CTYPE` at startup and the snapshot was replaying the value from before
  that write.
- **Directory listings are served, not refused.** A recorded directory is
  rebuilt in a scratch tree from the names the capsule carries, and the
  program's `getdents64` is then answered by the kernel from a real directory.
  Writing dirent structures by hand would duplicate the kernel's layout rules
  for no gain.

Two real bugs surfaced while doing it, both fixed:

- **A recorded FAILURE was treated as a missing entry.** A `readlink` on a
  regular file returns `EINVAL`, which the capsule recorded with no payload,
  and replay then diverged instead of replaying the failure. That alone was
  one of python's two divergences and most of ruby's early ones.
- **Entries were consumed once.** A program that stats or opens the same path
  twice during startup, which every interpreter does, diverged on the second
  lookup. Lookups now fall back to an already consumed entry rather than
  reporting drift that did not happen.

## Earlier defects this measurement caught

1. **Large file offset aliases were invisible.** With only `open`/`openat`
   interposed, a python3 run recorded zero file entries and a `cat` replay
   produced empty output, because glibc compiles `_FILE_OFFSET_BITS=64`
   callers against `open64`, `openat64`, `pread64`, `fopen64`.
2. **`openat` fell through to the live filesystem** for relative paths, so a
   deleted input produced an empty result with zero divergences.
3. **glibc stdio bypasses the POSIX symbols**, so `fopen`/`fread` were unseen.
4. **Both layers recorded the same bytes** once the syscall layer landed,
   replaying them concatenated.

## The completeness oracle

A capsule that recorded an `open` but none of the file's bytes used to serve
an empty file, which is a silent wrong replay. The open entry carries the
file's observed size, and every serve is judged against it:

- recorded size > 0 with zero recorded bytes emits `incomplete-file` and fails
  the open;
- recorded size > 0 with FEWER bytes than recorded emits `truncated-file` and
  fails the open;
- a recorded dial with zero recorded stream bytes emits `incomplete-socket`;
- both file markers name the byte counts in the detail
  (`<path> recorded=N served=M`), because a shortfall without its size is not
  actionable.

**The check fires AT THE SERVE, not at the reads, and the earlier deferral
was measured wrong.** The first version of this oracle flagged a partial
capture on the fd and diverged only when the program read PAST what the
capsule held, on the theory that a program legitimately reading a prefix
should not be punished. That deferral only exists on the libc `read` path.
The seccomp layer injects a descriptor the KERNEL answers, so no interposed
call ever sees the reads: a capsule truncated by hand replayed `cat` as
exit 0 with shortened output and ZERO divergences, on the default boundary.
An fmemopen stream has the same blindness, because glibc stdio internals
(fgets, fscanf, getline) bypass the fread interposer. mmap of a short memfd
is worse still: the bytes between the held length and the end of the last
page read back as zeros. So a serve that cannot cover the recorded size now
refuses up front on all three serving paths, and the libc read-EOF check
stays only as a backstop (with counts) for any future path that reintroduces
a partial fd. The cost is honest: a program that wanted only a prefix of a
source past the inline cap fails loudly instead of maybe-working, which is
the fail-closed direction.

### Re-measured for umbrella track 4a (2026-08-01)

Gate: `validation/process/gate-completeness.sh`, self-driving through Docker
(`gcc:13`, glibc 2.36), run on BOTH `linux/amd64` and `linux/arm64`. Every
replay row runs with the input file DELETED; the oracle rows replay capsules
gutted by hand (read entries removed, or their bytes cut to a 1 byte prefix
with the open's recorded size kept).

| row | linux/amd64 (libc layer) | linux/arm64 (seccomp layer) |
| --- | --- | --- |
| C program, plain `open`/`read` | byte identical, 0 divergences | byte identical, 0 divergences |
| coreutils `cat` | byte identical, 0 divergences | byte identical, 0 divergences |
| `python3` script | SKIP, named (below) | byte identical, 0 divergences |
| `cat`, capsule emptied | loud `incomplete-file recorded=20 served=0` | same |
| `cat`, capsule truncated | loud `truncated-file recorded=20 served=1` | same (was SILENT: exit 0, 0 div) |
| `python3`, capsule emptied | n/a | loud `incomplete-file recorded=20 served=0` |
| C program, `fopen`/`fread`, libc forced | byte identical, 0 divergences | same |
| `fopen`, capsule emptied | loud `incomplete-file` | same |
| `fopen`, capsule truncated | loud `truncated-file` | same |

`ruby` (ruby:3.1 image, arm64) was probed for regression after the at-serve
change and still replays byte identical with 0 divergences, as does the
existing `run.sh` python3 case; nothing those runs open falls short of its
recorded size, so the stricter rule costs them nothing.

Named limit, measured: Docker's x86_64 emulation on this arm64 host answers
EINVAL to `seccomp(SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_NEW_LISTENER)`,
so the completeness layer cannot install there and the shim falls back to the
libc boundary. An interpreted runtime does not survive that fallback (the
python3 libc-only replay dies `OSError: [Errno 9] Bad file descriptor` inside
`<frozen getpath>` with zero divergence lines), which is the already-stated
case for track 4c: seccomp user-notify REQUIRED for interpreted runtimes. The
gate SKIPS those rows with that named reason rather than letting the row
silently measure a different boundary. On a real x86_64 kernel the layer
installs; the EINVAL is the emulator's (measured below, not assumed).

### Re-measured for umbrella track 4b/4c (2026-08-01)

Track 4a made short serves loud; 4b records the movers FULLY so the loud
divergence becomes a correct replay for in-bound files, and 4c makes the
seccomp layer a recorded, enforced property of the capsule instead of a
silent runtime accident.

What changed, each measured before and after:

- The libc movers (`mmap`, `copy_file_range`, `sendfile`, `splice`) recorded
  at most 8 KiB PER CALL; they now record the whole moved range through a
  bounded pread loop, up to `REPROIT_FILE_CAP` per file, and a range past the
  cap records a `trunc` marker so the refusal names the bound. The movers
  record what MOVED, after the real call, because recording the requested
  length re-recorded the overlap when the kernel moved less than asked, and
  doubled content serves doubled. A high-water offset per fd stops a
  re-mapped range from being recorded twice for the same reason.
- Single-entry recording sites (`read`, `pread`, `readv`, `preadv`, `fread`)
  inlined at most 8 KiB per call; they now chunk through `record_content`, so
  one large read no longer loses everything past its first chunk. `preadv64`
  is now aliased; `readv` on a served FILE returned a false EOF (it asked the
  socket stream) and now reads the real injected descriptor.
- A serve that gathers MORE than the recorded size (`overlong-file`) now
  refuses too: a doubled serve is as silent and as wrong as a short one.
- The capsule records WHICH layer captured it (`layer seccomp` or
  `layer libc`, stamped by the shim at init). A layer-less capture or replay
  emits one named `REPROIT:PROCESS-LAYER` line, never silence; and a replay
  of a seccomp-captured capsule on a host that cannot install the layer
  REFUSES by name (`seccomp-required`, exit 3) before the program runs.
  Measured before the fix: that pairing died mid-run as
  `OSError: [Errno 9] Bad file descriptor` inside CPython's getpath with
  ZERO divergence lines, a silent wrong replay wearing a stack trace.

Gate: `validation/process/gate-completeness.sh` (`gcc:13`, glibc 2.36),
run on linux/amd64 (Docker emulation, libc layer), linux/arm64 (seccomp
layer), and on a REAL x86_64 kernel (strix, Fedora 7.1.4, via Docker
`gcc:13`), where the seccomp layer installs and the full row set passes,
proving the amd64 SKIP is the emulator's limit and not the code's.

| row | amd64 emulated (libc) | arm64 (seccomp) | x86_64 real (seccomp) |
| --- | --- | --- | --- |
| cat, 100 KiB through the movers | byte identical, 0 div (was loud truncated-file) | same | same |
| mmap, 100 KiB mapping | byte identical, 0 div (was loud truncated-file) | same | same |
| mmap, 16.5 MiB file past the cap | loud `truncated-file recorded=17301504 served=16777216 cap=16777216` | same | same |
| python3 capsule replayed layer-less | n/a (simulated seccomp capsule: refused by name) | `seccomp-required`, refused before the program ran (was OSError Errno 9 mid-run) | same as arm64 |
| static binary (`gcc -static`) | 0 entries recorded, 0 markers at replay: unsupported by construction, and measurably so | same | same |
| full gate | 12/12 | 14/14 | 14/14 |

`run.sh` still passes 20/20 (arm64, `reproit-phase2-rust` image: python3,
ruby, SDL2 engine, static refusals, keep/check by id).

Honest limit found on the REAL x86_64 host and kept: Fedora 43 (glibc 2.42)
ships `/usr/lib/locale/locale-archive` at 233,242,544 bytes, which no sane
per-file cap covers, so a native (non-container) python3 capture there hits
the cap and its replay refuses loudly:
`truncated-file /usr/lib/locale/locale-archive recorded=233242544
served=16777216 cap=16777216`. The stdout of that replay was still byte
identical (glibc fell back past the refused archive), but the run is
reported diverged, which is the fail-closed direction. A giant memory-mapped
system archive is a real cost of the per-file bound and it is NAMED, not
special-cased.

## Bounds

Every bound is explicit, and hitting one records a named `trunc` marker so
the replay divergence carries the bound instead of an anonymous gap:

- Per FILE: `REPROIT_FILE_CAP` 16 MiB, shared by the seccomp layer's
  `record_file` and the libc data movers so a file bounds the same way
  whichever layer records it. Larger than the SDKs' 8 KiB body rule because
  a process input is a whole file (a locale archive is 350 KiB); larger than
  the earlier 4 MiB because engine assets are the next consumer. A file past
  it replays as a loud `truncated-file ... cap=16777216`.
- Per CAPSULE content: 48 MiB of inline bytes, under the structural ceiling
  (8192 entries x 8 KiB = 64 MiB) so the `capsule-content` marker fires
  before the entry bound starts dropping lines.
- Per CAPSULE entries: 8192, with the LAST slot reserved for the
  `capsule-entries` marker, because a marker past the bound would never be
  loaded back.
- Replay load: 96 MiB of raw log (48 MiB of content base64s to 64 MiB). The
  earlier fixed 4 MiB read buffer silently parsed a PREFIX of an oversized
  log; past the bound the load now refuses loudly (`capsule-oversize`).

A capsule that carries any `capsule-*` marker also diverges by name at load
(`capsule-bound`), because a bounded recording cannot replay completely and
should say so up front rather than one short file at a time.

## Answering from the recording, not only from what it read

A branchy startup does not take the same path twice, so replay asks questions
the recording never asked. Two of those are answerable from what the capsule
already holds, and answering them is faithful rather than permissive:

- **The parent was enumerated and the name was not in it.** The recording
  listed the directory, so it knows the name did not exist.
- **An ancestor was observed as absent.** A directory recorded with ENOENT or
  ENOTDIR cannot contain anything beneath it.

Both are evidence, not inference about the host. Without either, the caller
still DIVERGES, and a name that IS in a recorded listing but has no entry of
its own also still diverges, because the capsule knows it existed and not what
it held. This is what took ruby from one divergence to zero without loosening
the contract, and it is exactly the same lesson as recording FAILURES
faithfully: what the recording observed includes the negatives.

## A latent bug in this script, found while extending it

`run_case` enabled `errexit` and never restored it, in a script that
deliberately runs subjects which exit non-zero. The first such command after a
verdict case therefore killed the run silently, with the harness reporting the
subject's exit status as its own. It is fixed by capturing the status without
touching shell flags. Worth stating because the failure mode is the one this
project keeps finding: a harness that stops early looks exactly like a harness
that passed everything it printed.
