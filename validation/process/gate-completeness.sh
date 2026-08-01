#!/usr/bin/env bash
# The completeness oracle gate (umbrella plan track 4a).
#
# The failure mode this gate exists to kill: a replay that EXITS SUCCESSFULLY
# with wrong (empty or shortened) output because a data path bypassed the
# boundary and the serve fell short of what the recording observed. Phase 1
# measured exactly that for coreutils cat and python3, and it recurred on the
# seccomp layer for hand-truncated capsules (a short scratch file is read by
# the KERNEL, so no interposed call ever sees the shortfall).
#
# The rule under test: every recorded open stamps the file's observed size,
# and a serve that cannot cover that size DIVERGES with a named reason and
# both byte counts (incomplete-file for zero held bytes, truncated-file for a
# prefix), instead of replaying short in silence. cat and python3 replaying
# CORRECTLY here is the shipped serving; the oracle rows prove the loud
# failure when the bytes are gone.
#
# Run it from anywhere: off Linux it re-executes itself inside Docker, once
# per platform in REPROIT_GATE_PLATFORMS. Inside a container it needs gcc and
# python3 (the gcc:13 image has both).
set -u

if [[ "$(uname -s)" != "Linux" ]]; then
  ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
  IMAGE="${REPROIT_GATE_IMAGE:-gcc:13}"
  for platform in ${REPROIT_GATE_PLATFORMS:-linux/amd64 linux/arm64}; do
    echo "=== gate-completeness on $platform ($IMAGE) ==="
    docker run --rm --platform "$platform" -v "$ROOT:/work" "$IMAGE" \
      bash /work/validation/process/gate-completeness.sh || exit 1
  done
  exit 0
fi

# Case accounting, same shape as run.sh: a gate that stops early prints only
# PASS lines and looks exactly like one that passed everything it printed.
CASES_RUN=0

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$(mktemp -d /tmp/reproit-oracle-gate.XXXXXX)"
SUBJECT_DIR=/tmp/reproit-oracle
cleanup() { rm -rf "$OUT" "$SUBJECT_DIR"; }
trap cleanup EXIT

# Is the seccomp completeness layer available HERE? Docker's x86_64 emulation
# on an arm64 host answers EINVAL to SECCOMP_FILTER_FLAG_NEW_LISTENER (an
# emulator limit, measured, not a kernel fact), and the shim then falls back
# to the libc layer. The interpreted-runtime rows need the completeness layer
# (that is plan item 4c's own statement), so they are SKIPPED by name when it
# cannot install, never silently varied.
cat > "$OUT/layer.c" <<'C_LAYER'
#define _GNU_SOURCE
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>
int main(void) {
    struct sock_filter f[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog p = {.len = 2, .filter = f};
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) return 1;
    return syscall(__NR_seccomp, SECCOMP_SET_MODE_FILTER, 1UL << 3, &p) >= 0 ? 0 : 1;
}
C_LAYER
gcc -O1 -o "$OUT/layer" "$OUT/layer.c" || { echo "FAIL layer probe build" >&2; exit 1; }
if "$OUT/layer"; then
  SECCOMP_LIVE=1
else
  SECCOMP_LIVE=0
fi
EXPECTED_CASES=$((7 + 2 * SECCOMP_LIVE))

echo "platform: $(uname -m), glibc $(ldd --version | awk 'NR==1{print $NF}'),\
 completeness layer: $([[ $SECCOMP_LIVE -eq 1 ]] && echo seccomp || echo 'libc only')"

gcc -shared -fPIC -O1 -o "$OUT/shim.so" \
  "$ROOT/runners/process-shim/reproit_shim.c" \
  "$ROOT/runners/process-shim/reproit_shim_capsule.c" \
  "$ROOT/runners/process-shim/reproit_shim_movers.c" \
  "$ROOT/runners/process-shim/reproit_shim_time.c" \
  "$ROOT/runners/process-shim/reproit_seccomp.c" \
  "$ROOT/runners/process-shim/reproit_seccomp_scratch.c" \
  "$ROOT/runners/process-shim/reproit_elf.c" -ldl \
  || { echo "FAIL shim build" >&2; exit 1; }

INPUT="$SUBJECT_DIR/input.txt"
CONTENT='twenty-bytes-of-data'

record() { # record <log> <out> -- cmd...
  local log="$1" out="$2"; shift 3
  mkdir -p "$SUBJECT_DIR"
  printf '%s' "$CONTENT" > "$INPUT"
  env "${EXTRA_ENV[@]}" LD_PRELOAD="$OUT/shim.so" REPROIT_RECORD="$log" \
    "$@" > "$out" 2> "$OUT/rec.err"
  RECORD_STATUS=$?
  rm -rf "$SUBJECT_DIR" # every replay below runs with the input DELETED
}

replay() { # replay <log> <out> <err> -- cmd...
  local log="$1" out="$2" err="$3"; shift 4
  env "${EXTRA_ENV[@]}" LD_PRELOAD="$OUT/shim.so" REPROIT_REPLAY_LOG="$log" \
    REPROIT_REPLAY_SEED=c0ffee00c0ffee00 "$@" > "$out" 2> "$err"
  REPLAY_STATUS=$?
}

assert_correct() { # assert_correct <label> <recorded-out> <replay-out> <replay-err>
  local label="$1" rec="$2" rep="$3" err="$4"
  local diverged
  diverged="$(grep -c 'REPROIT:DIVERGENCE' "$err")"
  if ! cmp -s "$rec" "$rep" || [[ "$diverged" -ne 0 ]]; then
    echo "FAIL $label: expected a byte-identical replay with 0 divergences" >&2
    echo "  divergences: $diverged" >&2
    echo "  recorded: [$(cat "$rec")]" >&2
    echo "  replayed: [$(cat "$rep")]" >&2
    grep 'REPROIT:DIVERGENCE' "$err" | head -5 >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label: byte-identical replay, 0 divergences"
}

assert_loud() { # assert_loud <label> <kind> <counts> <recorded-out> <replay-out> <replay-err>
  local label="$1" kind="$2" counts="$3" rec="$4" rep="$5" err="$6"
  local marker
  marker="$(grep -o "REPROIT:DIVERGENCE {\"layer\":\"process\",\"kind\":\"$kind\"[^}]*}" "$err" \
    | head -1)"
  if [[ -z "$marker" ]]; then
    echo "FAIL $label: no $kind divergence; this is the silent wrong replay" >&2
    echo "  replayed stdout: [$(cat "$rep")] (exit $REPLAY_STATUS)" >&2
    sed 's/^/  /' "$err" >&2
    exit 1
  fi
  if [[ "$marker" != *"$counts"* ]]; then
    echo "FAIL $label: the divergence does not name the byte counts '$counts'" >&2
    echo "  marker: $marker" >&2
    exit 1
  fi
  if cmp -s "$rec" "$rep"; then
    echo "FAIL $label: replay produced the recorded output AND diverged; the capsule" >&2
    echo "  was gutted, so identical output means the replay read something live" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label: loud $kind, $counts"
}

strip_reads() { # strip_reads <log> <path> <out-log>: remove the path's bytes
  grep -v "^read	$2	" "$1" > "$3"
}

truncate_reads() { # truncate_reads <log> <path> <out-log>: cut the bytes to 1
  python3 - "$1" "$2" "$3" <<'PY'
import base64, sys
log, path, out = sys.argv[1:4]
lines = []
cut = False
for line in open(log):
    f = line.rstrip('\n').split('\t')
    if f[0] == 'read' and f[1] == path and f[2] != '-':
        if cut:
            continue  # one prefix chunk stands for the whole stream
        f[2] = base64.b64encode(base64.b64decode(f[2])[:1]).decode()
        cut = True
    lines.append('\t'.join(f))
open(out, 'w').write('\n'.join(lines) + '\n')
PY
}

# Extra environment for record and replay; the stdio rows force the libc
# layer through it. Bash 4.4+ expands an empty array cleanly under set -u,
# and inside the container bash is 5.x.
EXTRA_ENV=()

# --- the phase 1 rows, re-run: these must replay CORRECTLY ---------------

cat > "$OUT/plain.c" <<'C_SUBJECT'
/* The phase 1 "C program, plain open/read" row. */
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
int main(void) {
    char buf[128];
    int fd = open("/tmp/reproit-oracle/input.txt", O_RDONLY);
    if (fd < 0) { perror("open"); return 2; }
    ssize_t n = read(fd, buf, sizeof(buf));
    if (n <= 0) { perror("read"); return 2; }
    close(fd);
    printf("read:%.*s\n", (int)n, buf);
    return 0;
}
C_SUBJECT
gcc -O1 -o "$OUT/plain" "$OUT/plain.c" || { echo "FAIL plain build" >&2; exit 1; }
record "$OUT/plain.log" "$OUT/plain.rec" -- "$OUT/plain"
replay "$OUT/plain.log" "$OUT/plain.rep" "$OUT/plain.err" -- "$OUT/plain"
assert_correct "C program, plain open/read" "$OUT/plain.rec" "$OUT/plain.rep" "$OUT/plain.err"

record "$OUT/cat.log" "$OUT/cat.rec" -- cat "$INPUT"
replay "$OUT/cat.log" "$OUT/cat.rep" "$OUT/cat.err" -- cat "$INPUT"
assert_correct "coreutils cat" "$OUT/cat.rec" "$OUT/cat.rep" "$OUT/cat.err"

if [[ "$SECCOMP_LIVE" -eq 1 ]]; then
  cat > "$OUT/script.py" <<'PY_SUBJECT'
data = open('/tmp/reproit-oracle/input.txt').read()
print("read:" + data)
PY_SUBJECT
  record "$OUT/py.log" "$OUT/py.rec" -- python3 "$OUT/script.py"
  replay "$OUT/py.log" "$OUT/py.rep" "$OUT/py.err" -- python3 "$OUT/script.py"
  assert_correct "python3 script" "$OUT/py.rec" "$OUT/py.rep" "$OUT/py.err"
else
  echo "SKIP python3 rows: seccomp user-notify unavailable here (interpreted" \
    "runtimes need the completeness layer; see plan item 4c)"
fi

# --- the oracle rows: gutted capsules must fail LOUDLY, with counts ------

strip_reads "$OUT/cat.log" "$INPUT" "$OUT/cat-empty.log"
replay "$OUT/cat-empty.log" "$OUT/e1.rep" "$OUT/e1.err" -- cat "$INPUT"
assert_loud "cat, capsule emptied of its bytes" "incomplete-file" \
  "recorded=${#CONTENT} served=0" "$OUT/cat.rec" "$OUT/e1.rep" "$OUT/e1.err"

# The row that replayed SHORT with zero divergences on the seccomp layer
# before this oracle: the kernel serves the scratch file, so no interposed
# read exists to catch the shortfall, and the check must fire at the serve.
truncate_reads "$OUT/cat.log" "$INPUT" "$OUT/cat-trunc.log"
replay "$OUT/cat-trunc.log" "$OUT/e2.rep" "$OUT/e2.err" -- cat "$INPUT"
assert_loud "cat, capsule truncated to a prefix" "truncated-file" \
  "recorded=${#CONTENT} served=1" "$OUT/cat.rec" "$OUT/e2.rep" "$OUT/e2.err"

if [[ "$SECCOMP_LIVE" -eq 1 ]]; then
  strip_reads "$OUT/py.log" "$INPUT" "$OUT/py-empty.log"
  replay "$OUT/py-empty.log" "$OUT/e3.rep" "$OUT/e3.err" -- python3 "$OUT/script.py"
  assert_loud "python3, capsule emptied of the input's bytes" "incomplete-file" \
    "recorded=${#CONTENT} served=0" "$OUT/py.rec" "$OUT/e3.rep" "$OUT/e3.err"
fi

# --- the stdio serving path (fmemopen), libc layer forced ----------------
# glibc stdio internals (fgets, fscanf, getline) bypass the fread interposer,
# so a short fmemopen stream cannot defer its check to the reads; the serve
# itself must refuse. Forced to the libc layer because that is the only mode
# this path serves in (with the supervisor live, fopen is passthrough).

cat > "$OUT/stdioread.c" <<'C_STDIO'
/* fread on purpose: it is the one stdio read the record boundary sees, so
 * the capsule carries the bytes and the gutted-capsule rows below test the
 * SERVE, not a recording gap. */
#include <stdio.h>
int main(void) {
    char buf[128];
    FILE *f = fopen("/tmp/reproit-oracle/input.txt", "r");
    if (!f) { perror("fopen"); return 2; }
    size_t n = fread(buf, 1, sizeof(buf), f);
    if (n == 0) { perror("fread"); return 2; }
    fclose(f);
    printf("read:%.*s\n", (int)n, buf);
    return 0;
}
C_STDIO
gcc -O1 -o "$OUT/stdioread" "$OUT/stdioread.c" || { echo "FAIL stdio build" >&2; exit 1; }
EXTRA_ENV=(REPROIT_SECCOMP=0)
record "$OUT/stdio.log" "$OUT/stdio.rec" -- "$OUT/stdioread"
replay "$OUT/stdio.log" "$OUT/stdio.rep" "$OUT/stdio.err" -- "$OUT/stdioread"
assert_correct "C program, fopen/fread (libc layer)" \
  "$OUT/stdio.rec" "$OUT/stdio.rep" "$OUT/stdio.err"

strip_reads "$OUT/stdio.log" "$INPUT" "$OUT/stdio-empty.log"
replay "$OUT/stdio-empty.log" "$OUT/e4.rep" "$OUT/e4.err" -- "$OUT/stdioread"
assert_loud "fopen, capsule emptied of its bytes" "incomplete-file" \
  "recorded=${#CONTENT} served=0" "$OUT/stdio.rec" "$OUT/e4.rep" "$OUT/e4.err"

truncate_reads "$OUT/stdio.log" "$INPUT" "$OUT/stdio-trunc.log"
replay "$OUT/stdio-trunc.log" "$OUT/e5.rep" "$OUT/e5.err" -- "$OUT/stdioread"
assert_loud "fopen, capsule truncated to a prefix" "truncated-file" \
  "recorded=${#CONTENT} served=1" "$OUT/stdio.rec" "$OUT/e5.rep" "$OUT/e5.err"

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL gate accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "gate-completeness: no silent short serve ($CASES_RUN/$EXPECTED_CASES cases)"
