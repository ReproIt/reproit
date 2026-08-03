#!/usr/bin/env bash
# The application-level anchor gate (Class C, umbrella plan track 6).
#
# The claim under test, the plan's honesty bound verbatim: same inputs, same
# data order, same seeds, from a checkpoint NEAR the failure, with the
# uncontrolled nondeterminism named IN THE ARTIFACT. Never bit-exact GPU
# replay, never "we reproduce the race".
#
# The anchor is the checkpoint the program itself wrote (a trainer's save),
# so the capture runs the program's own resume invocation and the boundary
# log covers the tail only, from the anchor forward. This is a different
# kind from the criu anchor run.sh measures: an application checkpoint is
# data the NEW binary loads, so a fix flipping the tail to a clean exit is a
# real fix verification, which a criu image can never give.
#
# Rows, the acceptance verbatim:
#   1 baseline: the planted failure fires at step 380; the trainer's own
#     checkpoint sits at step 350, near the failure
#   2 additive: a capsule WITHOUT an anchor still captures and replays, so
#     the anchor section is additive and old capsules keep working
#   3 anchored capture: the resume run is captured with the checkpoint
#     artifact bound by digest, the position, and the statement in the file
#   4 portability: clean copy of the trainer at a different absolute path,
#     the original build deleted, the checkpoint file DELETED (replay puts
#     the recorded bytes back), data present as recorded; tail reproduces
#   5 head skipped: the replay starts at the anchor, no head step appears
#   6 fix: TRAINER_FIXED=1 flips the tail replay to a clean exit
#   7 tamper: a tampered checkpoint digest refuses BY NAME before the
#     program runs
#   8 tamper: a deleted boundary read diverges naming the file
#   9 the statement stored in the artifact is printed verbatim with the
#     verdict
#
# Run it from anywhere: off Linux it re-executes itself inside Docker. The
# image and the cargo volume are SHARED with gate-session.sh on purpose (same
# Dockerfile, same name), so a machine that ran either gate builds nothing
# twice. REPROIT_GATE_ARCH=amd64 measures the other arch.
set -u

if [[ "$(uname -s)" != "Linux" ]]; then
  ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
  ARCH="${REPROIT_GATE_ARCH:-arm64}"
  # One cargo volume per arch: an emulated amd64 build clobbering the arm64
  # cache would make every later arm64 gate rebuild the world.
  if [[ "$ARCH" == "arm64" ]]; then
    VOLUME=reproit-session-cargo
  else
    VOLUME="reproit-anchor-cargo-$ARCH"
  fi
  # The shared image is built for the host arch only; a cross-arch run gets
  # its own tag so docker does not try to pull a nonexistent remote image.
  if [[ "$ARCH" == "arm64" ]]; then
    IMAGE="${REPROIT_GATE_IMAGE:-reproit-session-gate:latest}"
  else
    IMAGE="${REPROIT_GATE_IMAGE:-reproit-anchor-gate:$ARCH}"
  fi
  if ! docker image inspect "$IMAGE" > /dev/null 2>&1; then
    echo "=== building $IMAGE (one time, shared with gate-session) ==="
    docker build --platform "linux/$ARCH" -t "$IMAGE" - <<'DOCKERFILE' || exit 1
FROM rust:1.97.1-trixie
# libatspi2.0-dev because the CLI links -latspi; sdl2 for the C engine.
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends \
    libsdl2-dev python3 libatspi2.0-dev && rm -rf /var/lib/apt/lists/*
DOCKERFILE
  fi
  echo "=== gate-anchor on linux/$ARCH ($IMAGE) ==="
  docker run --rm --platform "linux/$ARCH" -v "$ROOT:/work" \
    -v "$VOLUME:/cargo-cache" -e CARGO_HOME=/cargo-cache/home \
    -e CARGO_TARGET_DIR=/cargo-cache/target "$IMAGE" \
    bash /work/validation/process-checkpoint/gate-anchor.sh
  exit $?
fi

CASES_RUN=0
EXPECTED_CASES=9

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$(mktemp -d /tmp/reproit-anchor-gate.XXXXXX)"

# Is the seccomp completeness layer available HERE? Docker's x86_64 emulation
# on an arm64 host answers EINVAL to SECCOMP_FILTER_FLAG_NEW_LISTENER (an
# emulator limit, measured by gate-completeness.sh, not a kernel fact). The
# trainer reads through stdio (fopen/fscanf), whose internal reads only the
# syscall layer sees; MEASURED on emulated amd64: the libc-only capture
# records the data file's open (7196 bytes) and can serve none of it, so the
# replay refuses LOUDLY with incomplete-file recorded=7196 served=0. That is
# the correct fail-closed behavior, and it means every shim row here needs
# the completeness layer, so they are SKIPPED by name without it.
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
if "$OUT/layer"; then SECCOMP_LIVE=1; else SECCOMP_LIVE=0; fi
# The capture's working directory is part of the capsule and must still exist
# at replay, so it lives OUTSIDE the build directory the portability row
# deletes (the same stated requirement gate-session.sh carries).
HOME_DIR="$OUT/anchor-home"
mkdir -p "$HOME_DIR"
cleanup() { rm -rf "$OUT"; }
trap cleanup EXIT

echo "platform: $(uname -m), glibc $(ldd --version | awk 'NR==1{print $NF}'),\
 completeness layer: $([[ $SECCOMP_LIVE -eq 1 ]] && echo seccomp || echo 'libc only')"

# --- toolchain: shim, CLI, trainer ----------------------------------------

gcc -shared -fPIC -O1 -o "$OUT/shim.so" \
  "$ROOT/runners/process-shim/reproit_shim.c" \
  "$ROOT/runners/process-shim/reproit_shim_capsule.c" \
  "$ROOT/runners/process-shim/reproit_shim_movers.c" \
  "$ROOT/runners/process-shim/reproit_shim_time.c" \
  "$ROOT/runners/process-shim/reproit_seccomp.c" \
  "$ROOT/runners/process-shim/reproit_seccomp_scratch.c" \
  "$ROOT/runners/process-shim/reproit_elf.c" -ldl \
  || { echo "FAIL shim build" >&2; exit 1; }
export REPROIT_PROCESS_SHIM="$OUT/shim.so"

REPROIT="${REPROIT_BINARY:-}"
if [[ -z "$REPROIT" ]]; then
  echo "=== building the CLI (cached in CARGO_TARGET_DIR) ==="
  (cd "$ROOT" && cargo build -q -p reproit --bin reproit) \
    || { echo "FAIL CLI build" >&2; exit 1; }
  REPROIT="${CARGO_TARGET_DIR:-$ROOT/target}/debug/reproit"
fi
[[ -x "$REPROIT" ]] || { echo "FAIL no CLI binary at $REPROIT" >&2; exit 1; }

BUILD="$OUT/trainer-build"
mkdir -p "$BUILD"
gcc -O1 -o "$BUILD/trainer" \
  "$ROOT/examples/trainer-checkpoint-fixture/trainer.c" -lm \
  || { echo "FAIL trainer build" >&2; exit 1; }

# The training data: 400 samples of y = 3x, one line per step, with the
# poisoned label at step 380: past the last checkpoint at 350, so the anchor
# sits NEAR the failure and the tail from it still reaches the defect.
STEPS=400
POISON_STEP=380
ANCHOR_STEP=350
python3 - "$HOME_DIR/data.txt" <<PY
import sys
lines = []
for i in range(1, $STEPS + 1):
    x = 0.1 + (i % 9) * 0.1
    if i == $POISON_STEP:
        lines.append("0.500000 1e12")
    else:
        lines.append("%.6f %.6f" % (x, 3.0 * x))
open(sys.argv[1], "w").write("\n".join(lines) + "\n")
PY

check_case() { # check_case <capsule> <command> <expected-exit> <label> <marker>
  local capsule="$1" command="$2" expected="$3" label="$4" marker="$5"
  (cd "$HOME_DIR" && "$REPROIT" --yes check "$capsule" --exec "$command") \
    > "$OUT/check.txt" 2>&1
  local status=$?
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$OUT/check.txt" >&2
    exit 1
  fi
  if ! grep -q "$marker" "$OUT/check.txt"; then
    echo "FAIL $label: output lacks the marker '$marker'" >&2
    cat "$OUT/check.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label (exit $status)"
}

# 1 baseline: the failure fires at step 380 and the last checkpoint the
# trainer wrote itself sits at step 350.
(cd "$HOME_DIR" && "$BUILD/trainer" data.txt "$STEPS" ckpt.txt) \
  > "$OUT/full.out" 2>&1
FULL_STATUS=$?
if [[ "$FULL_STATUS" -ne 7 ]] \
  || ! grep -q "assertion failed: weight left its bound at step $POISON_STEP" "$OUT/full.out"; then
  echo "FAIL baseline: the planted failure did not fire (exit $FULL_STATUS)" >&2
  cat "$OUT/full.out" >&2
  exit 1
fi
CKPT_STEP="$(awk '{print $1}' "$HOME_DIR/ckpt.txt")"
if [[ "$CKPT_STEP" -ne "$ANCHOR_STEP" ]]; then
  echo "FAIL baseline: expected the checkpoint at step $ANCHOR_STEP, found $CKPT_STEP" >&2
  exit 1
fi
CASES_RUN=$((CASES_RUN + 1))
echo "PASS baseline: failure at step $POISON_STEP, the trainer's own checkpoint at $CKPT_STEP"

# Scope gate, the same shape run.sh and gate-completeness.sh use: without the
# completeness layer every remaining row would measure a different boundary,
# so they are SKIPPED by name rather than silently varied or left red.
if [[ "$SECCOMP_LIVE" -ne 1 ]]; then
  if [[ "$CASES_RUN" -ne 1 ]]; then
    echo "FAIL harness accounting: $CASES_RUN of 1 cases ran before the scope gate" >&2
    exit 1
  fi
  echo "SKIP anchor rows 2-9: seccomp user-notify unavailable here (Docker's"
  echo "  x86_64 emulation answers EINVAL to SET_MODE_FILTER|NEW_LISTENER; a"
  echo "  real x86_64 kernel installs the layer, see validation/process/"
  echo "  MEASUREMENT.md). The trainer reads through stdio, which the libc"
  echo "  boundary cannot see; measured here: its capture refuses loudly at"
  echo "  replay (incomplete-file recorded=7196 served=0), never silently."
  echo "gate-anchor: baseline only; anchoring NOT EVALUATED on this host"
  exit 0
fi

# 2 additive: a plain capture (no anchor) of the FULL run still works, so
# the anchor section is additive and an anchor-less capsule stays valid.
(cd "$HOME_DIR" && "$REPROIT" --yes process-capture \
  --out "$OUT/plain.json" -- "$BUILD/trainer" data.txt "$STEPS" ckpt.txt) \
  > "$OUT/plain-cap.txt" 2>&1
grep -q "assertion failed" "$OUT/plain-cap.txt" \
  || { echo "FAIL plain capture did not see the failure" >&2; cat "$OUT/plain-cap.txt" >&2; exit 1; }
python3 - "$OUT/plain.json" <<'PY'
import json, sys
capsule = json.load(open(sys.argv[1]))
assert "anchor" not in capsule, "a plain capture must not invent an anchor"
PY
check_case "$OUT/plain.json" "$BUILD/trainer data.txt $STEPS ckpt.txt" 1 \
  "additive: an anchor-less capsule still replays the full run" \
  "reproduced by re-execution"

# 3 anchored capture: the trainer's own resume invocation, the checkpoint
# bound by digest, the position, and the statement stored in the artifact.
(cd "$HOME_DIR" && "$REPROIT" --yes process-capture \
  --out "$OUT/anchored.json" \
  --anchor-checkpoint ckpt.txt --anchor-position "$ANCHOR_STEP" \
  -- "$BUILD/trainer" data.txt "$STEPS" ckpt.txt resume) \
  > "$OUT/anchored-cap.txt" 2>&1
grep -q "resumed from checkpoint at step $ANCHOR_STEP" "$OUT/anchored-cap.txt" \
  || { echo "FAIL anchored capture did not resume" >&2; cat "$OUT/anchored-cap.txt" >&2; exit 1; }
grep -q "assertion failed: weight left its bound at step $POISON_STEP" "$OUT/anchored-cap.txt" \
  || { echo "FAIL anchored capture did not reach the failure" >&2; cat "$OUT/anchored-cap.txt" >&2; exit 1; }
python3 - "$OUT/anchored.json" "$HOME_DIR/ckpt.txt" "$ANCHOR_STEP" <<'PY'
import base64, hashlib, json, sys
capsule = json.load(open(sys.argv[1]))
anchor = capsule["anchor"]
assert anchor["kind"] == "application", anchor["kind"]
assert anchor["version"] == 1
assert anchor["position"] == {"ordinal": int(sys.argv[3]), "unit": "step"}
embedded = base64.b64decode(anchor["checkpointBase64"])
digest = "sha256:" + hashlib.sha256(embedded).hexdigest()
assert digest == anchor["checkpointSha256"], "embedded bytes must match the digest"
on_disk = open(sys.argv[2], "rb").read()
assert embedded == on_disk, "the embedded checkpoint must be the file the trainer wrote"
statement = anchor["uncontrolledSources"]
assert statement.startswith("UNCONTROLLED-SOURCES pinned:"), statement
assert "never a bit-exact" in statement
assert "thread scheduling" in statement and "GPU kernel" in statement
PY
CASES_RUN=$((CASES_RUN + 1))
echo "PASS anchored capture: checkpoint bound by digest, position $ANCHOR_STEP, statement stored"

# 4 portability: clean copy at a different absolute path, original build
# deleted, the checkpoint file DELETED so replay must put the recorded bytes
# back, data present as recorded. The trainer dials nothing, so the network
# leg of the bar is vacuous here and said so rather than implied.
MOVED="$OUT/moved-$RANDOM"
mkdir -p "$MOVED"
cp "$BUILD/trainer" "$MOVED/trainer"
rm -rf "$BUILD"
rm -f "$HOME_DIR/ckpt.txt"
check_case "$OUT/anchored.json" "$MOVED/trainer data.txt $STEPS ckpt.txt resume" 1 \
  "portability: the tail reproduces from a clean copy at $MOVED, checkpoint materialized" \
  "reproduced by re-execution"
grep -q "resumed from checkpoint at step $ANCHOR_STEP" "$OUT/check.txt" \
  || { echo "FAIL portability: the tail did not resume from the anchor" >&2
       cat "$OUT/check.txt" >&2; exit 1; }
cp "$OUT/check.txt" "$OUT/portability-check.txt"
cmp -s "$HOME_DIR/ckpt.txt" <(python3 -c "
import base64, json
print(base64.b64decode(json.load(open('$OUT/anchored.json'))['anchor']['checkpointBase64']).decode(), end='')") \
  || { echo "FAIL portability: the materialized checkpoint is not the recorded bytes" >&2; exit 1; }

# 5 head skipped: the replay contains no head progress line; the whole point
# of an anchor is that the first 350 steps never run.
if grep -q "trainer: step " "$OUT/portability-check.txt"; then
  echo "FAIL head skip: the replay printed head step lines, so the head ran" >&2
  cat "$OUT/portability-check.txt" >&2
  exit 1
fi
CASES_RUN=$((CASES_RUN + 1))
echo "PASS head skipped: the tail starts at step $ANCHOR_STEP, no head step ever ran"

# 6 fix: the fixed trainer replays the SAME tail to a clean exit. An
# application checkpoint is data the fixed binary loads, so this is a real
# fix verification, which a criu image can never give.
check_case "$OUT/anchored.json" \
  "TRAINER_FIXED=1 $MOVED/trainer data.txt $STEPS ckpt.txt resume" 0 \
  "fix: TRAINER_FIXED=1 flips the tail replay to a clean exit" \
  "the program now exits cleanly"

# 7 tamper: flip one byte of the embedded checkpoint; the replay must refuse
# BY NAME before the program runs.
python3 - "$OUT/anchored.json" "$OUT/tampered-ckpt.json" <<'PY'
import base64, json, sys
capsule = json.load(open(sys.argv[1]))
raw = bytearray(base64.b64decode(capsule["anchor"]["checkpointBase64"]))
raw[0] ^= 0xFF
capsule["anchor"]["checkpointBase64"] = base64.b64encode(bytes(raw)).decode()
json.dump(capsule, open(sys.argv[2], "w"))
PY
check_case "$OUT/tampered-ckpt.json" "$MOVED/trainer data.txt $STEPS ckpt.txt resume" 3 \
  "tamper: a tampered checkpoint digest refuses by name" \
  "anchor-checkpoint-digest"
if grep -q "resumed from checkpoint" "$OUT/check.txt"; then
  echo "FAIL tamper: the program ran before the refusal" >&2
  cat "$OUT/check.txt" >&2
  exit 1
fi

# 8 tamper: delete one recorded read of the data file; the tail replay must
# diverge naming the file, never serve a silent prefix.
python3 - "$OUT/anchored.json" "$OUT/tampered-tail.json" <<'PY'
import json, sys
capsule = json.load(open(sys.argv[1]))
# The whole data file fits one 8 KiB read entry, so deleting it leaves the
# open recording 7 KiB and the capsule able to serve none of it: exactly the
# short serve the completeness oracle refuses at the serve.
reads = [i for i, line in enumerate(capsule["entries"])
         if line.startswith("read\t") and "data.txt" in line]
assert reads, "expected a recorded read of the data file"
del capsule["entries"][reads[-1]]
json.dump(capsule, open(sys.argv[2], "w"))
PY
check_case "$OUT/tampered-tail.json" "$MOVED/trainer data.txt $STEPS ckpt.txt resume" 3 \
  "tamper: a deleted boundary read diverges naming the file" \
  "data.txt"
grep -qi "diverg" "$OUT/check.txt" \
  || { echo "FAIL tail tamper: no divergence was named" >&2; cat "$OUT/check.txt" >&2; exit 1; }

# 9 the statement in the artifact IS the statement in the output, verbatim.
python3 - "$OUT/anchored.json" "$OUT/portability-check.txt" <<'PY'
import json, sys
statement = json.load(open(sys.argv[1]))["anchor"]["uncontrolledSources"]
output = open(sys.argv[2]).read()
assert statement in output, "the replay output must carry the stored statement verbatim"
PY
CASES_RUN=$((CASES_RUN + 1))
echo "PASS statement: the artifact's uncontrolled-sources text appears verbatim with the verdict"

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL gate accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "gate-anchor: the application anchor skips the head honestly ($CASES_RUN/$EXPECTED_CASES cases)"
