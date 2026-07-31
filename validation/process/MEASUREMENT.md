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

| program | entries | divergences | replay output correct | verdict |
| --- | ---: | ---: | --- | --- |
| C program, plain `open`/`read` | 3 | 0 | yes | replay correct |
| C program, `-D_FILE_OFFSET_BITS=64` | 3 | 0 | yes | replay correct |
| coreutils `cat` | 3 | 0 | yes | replay correct |
| `python3` script | 863 | 1 | no | fails closed, DIVERGED with the path named |
| `ruby` script | 2288 | 68 | no | fails closed, DIVERGED |
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

## What still does not work: the wall, with evidence

**`python3` does not replay correctly. One divergence remains and its cause is
identified, not guessed.**

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

**`ruby` fails harder, and generalizes the wall differently.** It diverges 68
times and segfaults, on `/proc/self/maps`, `/etc/ld.so.cache`, and
`libgcc_s.so.1`. Ruby dlopens native extensions at runtime, and a shared
object cannot be served from a memfd copy: the dynamic loader maps it
`PROT_EXEC` and relocates it, so a substituted descriptor breaks the mapping
rather than feeding it. So the generalization across both runtimes is:

> This boundary serves DATA files correctly. It does not serve objects the
> kernel and the loader interpret structurally, which today means shared
> libraries and glibc locale objects. Interpreted runtimes reach for both
> during startup, which is why neither replays correctly yet.

Both fail closed. At the product level the verdict is `DIVERGED`, exit 3. No
configuration reports a passing or reproducing verdict for a replay that did
not re-execute.

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
