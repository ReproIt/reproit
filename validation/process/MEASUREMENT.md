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
| `ruby` script | 1780 | 0 | no | fails closed, INCONCLUSIVE |
| C program, `gcc -static` | 0 | 0 | n/a | capture REFUSED before it runs |

Every replay above ran with the input file DELETED.

For comparison, the same `python3` case with the completeness layer disabled
(`REPROIT_SECCOMP=0`) reports ZERO divergences and still replays wrong. That
contrast is the whole argument for the layer: the failure did not become
correct, but it stopped being silent.

Three rounds of fixes moved python from 2 divergences to 1 and closed two real
bug classes on the way (below). It did NOT reach a correct replay. The
remaining cause is named, with evidence, rather than guessed.

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

What remains is NOT a missing entry. Ruby's replay evaluates `rbconfig.rb`
twice, which it reports itself:

```
rbconfig.rb:369: warning: already initialized constant RbConfig::TOPDIR
rbconfig.rb:16:  warning: previous definition of TOPDIR was here
```

`strace` on the replay shows `/usr/lib/aarch64-linux-gnu/ruby/3.1.0/rbconfig.rb`
opened twice, while the recorded run searched the `site_ruby` prefixes first.
So the library search resolves in a different ORDER under replay, `require`
dedupes on the resolved path, and the same file is loaded under two names.
Closing it means pinning the interpreter's search order, not adding another
syscall to the boundary.

It fails closed. At the product level the verdict is `INCONCLUSIVE`, exit 3:

```
INCONCLUSIVE recorded exit 3, observed exit 1; failing closed
boundary: 91 served, 0 diverged, 0 clock overrun, 0 rng overrun, 1 env fallthrough
```

That distinction matters and is now pinned by a case in
`validation/process/run.sh`: zero divergences at the boundary is NOT a
reproduction, and the product must never report one for a replay whose program
did not do what it did when recorded.

## Static binaries: measured, and now refused

Previously unmeasured because the test image could not link one. It can:
`gcc -static` produces a working binary, and the measurement shows the
boundary observes **0 entries**, exactly as predicted. A statically linked
program resolves no dynamic symbols, so nothing is interposed.

A capsule of nothing would replay as a false success, so capture now refuses
in two independent places:

- before the program runs, by reading its ELF program headers: no `PT_INTERP`
  means no dynamic loader, and capture stops with a named reason;
- after it runs, if the boundary observed nothing at all, which also catches a
  loader that dropped the preload for other reasons.

The seccomp layer would see a static binary's syscalls, since it filters the
kernel boundary rather than the symbol table. Wiring capture to run without
the libc shim is a real option and is NOT implemented; until it is, refusing
is the honest answer.

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
an empty file, which is a silent wrong replay. The open entry now carries the
file's size, and:

- recorded size > 0 with zero recorded bytes emits `incomplete-file` and fails
  the open;
- a recorded dial with zero recorded stream bytes emits `incomplete-socket`;
- a PARTIAL capture does not diverge at open, because a program that
  legitimately reads only a prefix would be punished for nothing. It diverges
  at the moment the program reads PAST what the capsule carries, reported as
  `truncated-file`.

## Bounds

Per file inline content is capped at 4 MiB, deliberately larger than the SDKs'
8 KiB body rule because a process input is a whole file (a locale archive is
350 KiB), and a file past the cap records its size but not all its bytes,
which the completeness oracle turns into a loud `truncated-file` when the
program reads past what the capsule holds. The capsule keeps its 8192 entry
bound with the dropped count stated.

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
