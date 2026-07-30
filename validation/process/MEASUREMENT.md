# Process capsule, phase 1: what was measured

Platform for every number below: Linux x86_64 (aarch64 host via Docker),
glibc 2.36, `gcc:13` image. Reproduce with `validation/process/measure.sh`
(shim only) and `validation/process/run.sh` (full CLI, needs a Linux CLI
binary via `REPROIT_BINARY`).

The question phase 1 exists to answer: can an LD_PRELOAD boundary record
enough of a real native program that replay does not diverge spuriously, and
never replays WRONG in silence?

## The table

| program | entries | divergences | replay output correct | verdict |
| --- | ---: | ---: | --- | --- |
| C program, plain `open`/`read` | 2 | 0 | yes | replay correct |
| C program, `-D_FILE_OFFSET_BITS=64` | 2 | 0 | yes | replay correct |
| coreutils `cat` | 2 | 0 | yes | replay correct |
| `python3` script | 274 | 0 | no | fails closed as INCONCLUSIVE |

Every replay above ran with the input file DELETED and, for the acceptance
subject, the upstream server DOWN.

## What works

Ordinary compiled programs replay correctly and hermetically. The full CLI
acceptance (`run.sh`) holds all four verdicts against a native subject that
reads a config file, dials a socket, reads the clock, and draws from
`getrandom`:

```
PASS captured the planted abort into a process capsule
PASS bug reproduces with the file deleted and the upstream down (exit 1)
PASS fix certifies (exit 0)
PASS revert reproduces again (exit 1)
PASS missing socket bytes diverges (exit 3)
```

## Three defects the measurement caught, and the fixes

1. **Large file offset aliases were invisible.** With only `open`/`openat`
   interposed, a python3 run recorded 256 entries and ZERO file entries, and a
   `cat` replay produced empty output. glibc compiles callers that define
   `_FILE_OFFSET_BITS=64` (CPython, coreutils, most of userspace) against
   `open64`, `openat64`, `pread64`, `fopen64`. Fixed by interposing the
   aliases. This was found by measuring, not by reading code.
2. **`openat` fell through to the live filesystem** for relative paths, so a
   deleted input produced an empty result with zero divergences: a false
   negative. Fixed by resolving against the cwd and treating any path the
   capsule does not carry as a divergence, even when that makes an interpreted
   runtime noisy. Fail closed is the contract.
3. **glibc stdio bypasses the POSIX symbols.** `fopen`/`fread` call libc's
   internal `__open`/`__read`, so the boundary saw nothing. Fixed by
   interposing stdio and serving replay through `fmemopen`.

## The completeness oracle

A capsule that recorded an `open` but none of the file's bytes used to serve
an empty file, which is a silent wrong replay. Now the open entry carries the
file's size from `fstat`, and:

- recorded size > 0 with zero recorded bytes emits
  `REPROIT:DIVERGENCE {"kind":"incomplete-file"}` and fails the open;
- a recorded dial with zero recorded stream bytes emits `incomplete-socket`
  when the program reads;
- a PARTIAL capture (fewer bytes recorded than the file held) does not
  diverge at open, because a program that legitimately reads only a prefix
  would be punished for nothing. It diverges at the moment the program reads
  PAST what the capsule carries, reported as `truncated-file`.

## Data movers

`read`/`pread` are not the only ways bytes leave a file. Added, because the
measured failures exercised them: `mmap` (file backed), `copy_file_range`,
`sendfile`, `splice`, `readv`, `preadv`. `cat` moved from "replay wrong, 0
divergences" to "replay correct" purely from `copy_file_range` coverage.

## What still does not work, stated plainly

**Interpreted runtimes.** `python3` still replays wrong. The cause is
measured, not guessed: the capsule records the script's data file correctly
(`open` + `read` with the right bytes), but CPython dies during interpreter
startup with

```
Fatal Python error: error evaluating path
OSError: [Errno 9] Bad file descriptor
```

CPython's path evaluation probes the filesystem with `stat`, `access`,
`readlink`, and `getcwd`, none of which this boundary covers, so replay cannot
reach the script at all.

Crucially, this is NOT a silent wrong replay at the product level. The capsule
verdict is:

```
  INCONCLUSIVE recorded exit 0, observed exit 1; failing closed
  boundary: 5 served, 0 diverged, 0 clock overrun, 0 rng overrun, 38 env fallthrough
```

The shim emits no divergence marker (nothing it serves diverged), but the
verdict refuses to claim a reproduction or a fix and exits 3. No configuration
of this feature reports a passing or reproducing verdict for a replay that did
not actually re-execute.

**Unmeasured: static binaries.** `gcc -static` could not link in the test
image (no static glibc), so the static case has no number. It is unreachable
by construction: a statically linked program performs no dynamic symbol
resolution, so nothing is interposed and a capsule would be empty. Treat
"empty capsule" as a refusal to capture rather than a successful capture of
nothing; that guard is not yet implemented.

## Recommendation, with the evidence for it

Extending the libc boundary further has diminishing returns and a growing
tail: after four rounds of additions it still cannot start CPython, because
the misses are no longer data movers but metadata calls (`stat`, `access`,
`readlink`) whose complete coverage is a moving target per libc version.

**seccomp user-notify (Linux) is required, not optional, for interpreted
runtimes and static binaries.** All of the above converge at the syscall
layer, where one supervisor sees `openat`, `statx`, `readlink`, `mmap`, and
`copy_file_range` regardless of how the program was linked or which libc
alias it called. The libc shim remains the right cheap path for compiled
programs, which is the phase 2 target (a game engine is a compiled binary that
looks like the passing rows, not the python row).

Suggested scope: keep the libc boundary as the fast path for compiled
programs, add the empty-capsule refusal, and put seccomp behind phase 2 rather
than phase 3, because an engine's asset loading will lean on `mmap` and
`statx` heavily enough that the fast path alone will not hold.
