#!/usr/bin/env bash
# The Class B session capsule gate (umbrella plan track 5).
#
# What a session capsule must prove that Class A never needed: the trigger is
# a TIMED INPUT STREAM. Each subject's planted defect is a stale combo that
# fires only when presses arrive FAR APART, so the same bytes back to back
# are SAFE and a replay that ignored the recorded tick schedule would NOT
# reproduce the crash. Every subject therefore pins the premise first.
#
# Rows per subject, the acceptance verbatim:
#   1 premise: the same bytes back to back exit cleanly (timing is the bug)
#   2 capture: a scripted spread session crashes and is captured
#   3 portability: the capsule replays the crash from a CLEAN COPY of the
#     binary at a DIFFERENT absolute path, the original deleted, no input
#     attached, network never dialed (the subjects have no sockets)
#   4 fix: the guarded build replays to a clean exit (verdict flips)
#   5 tamper: moving one input event's tick refuses by name (input-tick),
#     naming the event and both ticks, BEFORE the program runs. Measured
#     before that check existed: the same tamper reported the bug FIXED with
#     exit 0 and zero divergences, a false certificate.
#
# Subjects:
#   sdl    validation/process/engine.c, a fixed timestep loop on SDL2 with
#          SDL_VIDEODRIVER=dummy (third-party platform layer, own loop)
#   bevy   examples/engine-session-bevy, bevy_app's ScheduleRunnerPlugin at a
#          fixed timestep (a real third-party engine runner), headless by
#          construction
#
# Run it from anywhere: off Linux it re-executes itself inside Docker on
# linux/arm64 (the seccomp completeness layer does not install under Docker's
# x86_64 emulation; a real x86_64 kernel runs it fine, see the process gate).
# The image is built once from the inline Dockerfile below (rust + SDL2 +
# python3); cargo state persists in the named volume so the CLI and the bevy
# sample build once, not per run.
set -u

VOLUME=reproit-session-cargo
if [[ "$(uname -s)" != "Linux" ]]; then
  ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
  IMAGE="${REPROIT_GATE_IMAGE:-reproit-session-gate:latest}"
  if ! docker image inspect "$IMAGE" > /dev/null 2>&1; then
    echo "=== building $IMAGE (one time) ==="
    docker build -t "$IMAGE" - <<'DOCKERFILE' || exit 1
FROM rust:1.97.1-trixie
# libatspi2.0-dev because the CLI links -latspi; sdl2 for the C engine.
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends \
    libsdl2-dev python3 libatspi2.0-dev && rm -rf /var/lib/apt/lists/*
DOCKERFILE
  fi
  echo "=== gate-session on linux/arm64 ($IMAGE) ==="
  docker run --rm --platform linux/arm64 -v "$ROOT:/work" \
    -v "$VOLUME:/cargo-cache" -e CARGO_HOME=/cargo-cache/home \
    -e CARGO_TARGET_DIR=/cargo-cache/target "$IMAGE" \
    bash /work/validation/session/gate-session.sh
  exit $?
fi

# Case accounting, same shape as run.sh: a gate that stops early prints only
# PASS lines and looks exactly like one that passed everything it printed.
CASES_RUN=0
EXPECTED_CASES=10

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$(mktemp -d /tmp/reproit-session-gate.XXXXXX)"
# The capture's working directory is part of the capsule and must still exist
# at replay, so it lives OUTSIDE the build directories the portability rows
# delete. That requirement is stated, not hidden: a process capsule records
# the cwd because relative paths cannot resolve the same way from anywhere
# else, and replay abstains by name when it is gone.
HOME_DIR="$OUT/session-home"
mkdir -p "$HOME_DIR"
cleanup() { rm -rf "$OUT"; }
trap cleanup EXIT

echo "platform: $(uname -m), glibc $(ldd --version | awk 'NR==1{print $NF}')"

# --- toolchain: shim, CLI, and both subjects ------------------------------

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

command -v sdl2-config > /dev/null 2>&1 \
  || { echo "FAIL this image has no sdl2-config" >&2; exit 1; }
mkdir -p "$OUT/sdl-build"
gcc -O1 -o "$OUT/sdl-build/engine" "$ROOT/validation/process/engine.c" \
  $(sdl2-config --cflags --libs) \
  || { echo "FAIL SDL engine build" >&2; exit 1; }

echo "=== building the bevy sample (cached) ==="
(cd "$ROOT/examples/engine-session-bevy" && cargo build -q --release) \
  || { echo "FAIL bevy sample build" >&2; exit 1; }
BEVY_TARGET="${CARGO_TARGET_DIR:-$ROOT/examples/engine-session-bevy/target}"
mkdir -p "$OUT/bevy-build"
cp "$BEVY_TARGET/release/engine-session-bevy" "$OUT/bevy-build/engine" \
  || { echo "FAIL bevy binary missing" >&2; exit 1; }
BEVY_VERSION="$(grep -A1 'name = "bevy_app"' \
  "$ROOT/examples/engine-session-bevy/Cargo.lock" | awk -F'"' '/version/{print $2}')"
echo "engine versions: SDL $(sdl2-config --version), bevy_app $BEVY_VERSION"

export SDL_VIDEODRIVER=dummy ENGINE_FRAMES=120

feed_spread() { # three presses, 0.25 s apart: far past STALE_AFTER frames
  python3 - <<'PY'
import sys, time
for _ in range(3):
    sys.stdout.buffer.write(b"u")
    sys.stdout.buffer.flush()
    time.sleep(0.25)
PY
}

check_case() { # check_case <capsule> <command> <expected-exit> <label> <marker>
  local capsule="$1" command="$2" expected="$3" label="$4" marker="$5"
  "$REPROIT" --yes check "$capsule" --exec "$command" > "$OUT/check.txt" 2>&1
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

tamper_tick() { # tamper_tick <capsule> <out>: move the last press onto the
  # first press's very next tick, so the schedule claims the presses arrived
  # back to back, which the premise row proved SAFE. Without the load-time
  # schedule check this replays to a false "fixed".
  python3 - "$1" "$2" <<'PY'
import json, sys
capsule = json.load(open(sys.argv[1]))
inputs = [i for i, l in enumerate(capsule["entries"]) if l.startswith("input\t")]
assert len(inputs) >= 2, "need two input events to tamper the schedule"
first = capsule["entries"][inputs[0]].split("\t")
last = capsule["entries"][inputs[-1]].split("\t")
last[4] = str(int(first[4]) + 1)
capsule["entries"][inputs[-1]] = "\t".join(last)
json.dump(capsule, open(sys.argv[2], "w"))
PY
}

subject_rows() { # subject_rows <name> <build-dir> <crash-marker>
  local name="$1" build="$2" crash="$3"
  local engine="$build/engine"
  local moved="$OUT/$name-moved-$RANDOM"

  # 1 premise: the same bytes back to back are SAFE, so timing is the defect.
  printf 'uuu' | "$engine" > "$OUT/$name-burst.out" 2>&1
  if [[ $? -ne 0 ]] || ! grep -q "survived" "$OUT/$name-burst.out"; then
    echo "FAIL $name premise: back-to-back bytes should be safe" >&2
    cat "$OUT/$name-burst.out" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $name premise: back-to-back bytes are safe, the bug is the schedule"

  # 2 capture: presses spread 0.25 s apart crash as planted.
  (cd "$HOME_DIR" && feed_spread | "$REPROIT" --yes internal process-capture \
    --out "$OUT/$name.json" -- "$engine") > "$OUT/$name-cap.txt" 2>&1
  if ! grep -q "$crash" "$OUT/$name-cap.txt"; then
    echo "FAIL $name capture: the spread session did not crash as planted" >&2
    cat "$OUT/$name-cap.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $name capture: the spread session crashed and was captured"

  # 3 portability: a CLEAN COPY of the binary at a DIFFERENT absolute path,
  # the original build deleted. No input is attached; replay serves the
  # session from the capsule on the recorded tick schedule.
  mkdir -p "$moved"
  cp "$engine" "$moved/engine"
  rm -rf "$build"
  check_case "$OUT/$name.json" "$moved/engine" 1 \
    "$name portability: crash reproduces from a clean copy at $moved" \
    "reproduced by re-execution"

  # 4 fix: the guarded build replays the same capsule to a clean exit.
  check_case "$OUT/$name.json" "REPROIT_FIXED=1 $moved/engine" 0 \
    "$name fix: discarding the stale combo certifies the fix" \
    "the program now exits cleanly"

  # 5 tamper: the moved tick refuses by name before the program runs.
  tamper_tick "$OUT/$name.json" "$OUT/$name-tampered.json"
  check_case "$OUT/$name-tampered.json" "$moved/engine" 3 \
    "$name tamper: a moved input tick diverges naming the tick" \
    '"kind":"input-tick"'
  if ! grep -q 'records tick=.*log places it at tick=' "$OUT/check.txt"; then
    echo "FAIL $name tamper: the divergence does not name both ticks" >&2
    cat "$OUT/check.txt" >&2
    exit 1
  fi
}

subject_rows sdl "$OUT/sdl-build" "fatal signal"
subject_rows bevy "$OUT/bevy-build" "panicked at"

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL gate accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "gate-session: the timed input stream round-trips ($CASES_RUN/$EXPECTED_CASES cases)"
