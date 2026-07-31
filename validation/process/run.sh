#!/usr/bin/env bash
# Process capsule acceptance, driven end to end by the CLI.
#
# Captures a native program's failure into a process capsule, then re-executes
# it with the config file DELETED, the upstream DOWN, and no network, and
# asserts the four verdicts: reproduced (1), fixed (0), reproduced again (1),
# and diverged (3) when the capsule is missing the socket bytes.
#
# LINUX ONLY. On macOS, SIP strips DYLD_INSERT_LIBRARIES when the CLI spawns
# the command through /bin/sh, so the shim never reaches the subject; that is
# a platform fact, not a bug in this script.
set -u

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REPROIT="${REPROIT_BINARY:-$ROOT/target/debug/reproit}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-process.XXXXXX")"
UPSTREAM_PID=""
cleanup() {
  [[ -n "$UPSTREAM_PID" ]] && kill "$UPSTREAM_PID" 2>/dev/null
  rm -rf "$WORK" /tmp/reproit-subject
}
trap cleanup EXIT

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "SKIP process-e2e: LD_PRELOAD injection through sh is Linux only"
  exit 0
fi
if [[ ! -x "$REPROIT" ]]; then
  echo "SKIP process-e2e: no CLI binary at $REPROIT (set REPROIT_BINARY)"
  exit 0
fi

SHIM_SOURCES=("$ROOT/runners/process-shim/reproit_shim.c"
  "$ROOT/runners/process-shim/reproit_shim_capsule.c"
  "$ROOT/runners/process-shim/reproit_shim_movers.c")
# The syscall completeness layer is Linux only; this script already is.
SHIM_SOURCES+=("$ROOT/runners/process-shim/reproit_seccomp.c")
gcc -shared -fPIC -O1 -o "$WORK/reproit_shim.so" "${SHIM_SOURCES[@]}" -ldl
gcc -O1 -o "$WORK/subject" "$ROOT/validation/process/subject.c"
export REPROIT_PROCESS_SHIM="$WORK/reproit_shim.so"

start_upstream() {
  python3 - <<'PY' &
import socket
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", 19981))
srv.listen(8)
while True:
    try:
        conn, _ = srv.accept()
    except OSError:
        break
    try:
        conn.recv(4096)
        conn.sendall(b'HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{"quote":42,"limit":null}')
    finally:
        conn.close()
PY
  UPSTREAM_PID=$!
  sleep 0.6
}

# RECORD: config present, upstream up, defect fires.
mkdir -p /tmp/reproit-subject
printf '{ "strict": true }' > /tmp/reproit-subject/config.json
start_upstream
"$REPROIT" --yes internal process-capture --out "$WORK/capsule.json" -- "$WORK/subject" \
  > "$WORK/capture.txt" 2>&1
kill "$UPSTREAM_PID" 2>/dev/null; UPSTREAM_PID=""
if ! grep -q "fatal signal" "$WORK/capture.txt"; then
  echo "FAIL capture: the subject did not fail as planted" >&2
  cat "$WORK/capture.txt" >&2
  exit 1
fi
echo "PASS captured the planted abort into a process capsule"

# HERMETIC STATE: no config file, no upstream, for every run below.
rm -rf /tmp/reproit-subject

run_case() {
  local capsule="$1" command="$2" expected="$3" label="$4" marker="$5"
  # This script runs without errexit on purpose: several subjects below are
  # SUPPOSED to exit non-zero. Enabling it here and never restoring it aborted
  # the run at the first such command, so the status is simply captured.
  "$REPROIT" --yes check "$capsule" --exec "$command" > "$WORK/out.txt" 2>&1
  local status=$?
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  if ! grep -q "$marker" "$WORK/out.txt"; then
    echo "FAIL $label: output lacks the verdict marker '$marker'" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  echo "PASS $label (exit $status)"
}

run_case "$WORK/capsule.json" "$WORK/subject" 1 \
  "bug reproduces with the file deleted and the upstream down" "reproduced by re-execution"
run_case "$WORK/capsule.json" "REPROIT_FIXED=1 $WORK/subject" 0 \
  "fix certifies" "the program now exits cleanly"
run_case "$WORK/capsule.json" "$WORK/subject" 1 \
  "revert reproduces again" "reproduced by re-execution"

python3 - "$WORK/capsule.json" "$WORK/tampered.json" <<'PY'
import json, sys
capsule = json.load(open(sys.argv[1]))
capsule["entries"] = [e for e in capsule["entries"] if not e.startswith("recv\t")]
json.dump(capsule, open(sys.argv[2], "w"))
PY
run_case "$WORK/tampered.json" "$WORK/subject" 3 \
  "missing socket bytes diverges" "DIVERGED"

# An INTERPRETED runtime. Measured separately because its boundary coverage
# is not the compiled case: see validation/process/MEASUREMENT.md. Whatever it
# does, it must never report a passing or reproducing verdict for a replay
# that did not re-execute, so this case pins the fail-closed property rather
# than assuming a reproduction.
mkdir -p /tmp/reproit-subject
printf 'boom' > /tmp/reproit-subject/input.txt
cat > "$WORK/script.py" <<'PY_SUBJECT'
import sys
data = open('/tmp/reproit-subject/input.txt').read().strip()
print("read:" + data)
sys.exit(3 if data == "boom" else 0)
PY_SUBJECT
"$REPROIT" --yes internal process-capture --out "$WORK/py-capsule.json" --   python3 "$WORK/script.py" > "$WORK/py-capture.txt" 2>&1
PY_CAPTURED=$?
rm -rf /tmp/reproit-subject
if [[ "$PY_CAPTURED" -ne 0 ]]; then
  echo "FAIL python3 subject: capture refused" >&2
  cat "$WORK/py-capture.txt" >&2
  exit 1
fi
# An INTERPRETED runtime, with its input file deleted. This asserts a REAL
# reproduction rather than merely failing closed: serving recorded content as
# real files rather than memfd copies is what made an interpreter's startup
# resolve identically to the recorded run.
run_case "$WORK/py-capsule.json" "python3 $WORK/script.py" 1 \
  "python3 subject reproduces hermetically" "reproduced by re-execution"

# The verdict alone does not prove the replayed program produced the same
# OUTPUT, only that it failed the same way. This compares stdout byte for
# byte, at the shim boundary because the CLI discards the subject's stdout and
# a redirect inside --exec would itself be an unrecorded open.
mkdir -p /tmp/reproit-subject
printf 'boom' > /tmp/reproit-subject/input.txt
LD_PRELOAD="$WORK/reproit_shim.so" REPROIT_RECORD="$WORK/stdout.log" \
  python3 "$WORK/script.py" > "$WORK/stdout.record" 2>/dev/null
rm -rf /tmp/reproit-subject
LD_PRELOAD="$WORK/reproit_shim.so" REPROIT_REPLAY_LOG="$WORK/stdout.log" \
  REPROIT_REPLAY_SEED=c0ffee00c0ffee00 \
  python3 "$WORK/script.py" > "$WORK/stdout.replay" 2>/dev/null
if ! cmp -s "$WORK/stdout.record" "$WORK/stdout.replay"; then
  echo "FAIL python3 replayed stdout is not byte identical to the recording" >&2
  echo "  recorded: $(tr '\n' '|' < "$WORK/stdout.record")" >&2
  echo "  replayed: $(tr '\n' '|' < "$WORK/stdout.replay")" >&2
  exit 1
fi
echo "PASS python3 replayed stdout is byte identical to the recording"

# An interpreted runtime this boundary does NOT replay correctly. Ruby reaches
# zero divergences but its startup resolves libraries in a different order, so
# it must never claim a reproduction. This pins the fail closed property: the
# honest verdict is INCONCLUSIVE, not a pass and not a reproduction.
if command -v ruby > /dev/null 2>&1; then
  mkdir -p /tmp/reproit-subject
  printf 'boom' > /tmp/reproit-subject/input.txt
  cat > "$WORK/script.rb" <<'RB_SUBJECT'
data = File.read('/tmp/reproit-subject/input.txt').strip
puts "read:" + data
exit(data == "boom" ? 3 : 0)
RB_SUBJECT
  "$REPROIT" --yes internal process-capture --out "$WORK/rb-capsule.json" -- \
    ruby "$WORK/script.rb" > "$WORK/rb-capture.txt" 2>&1
  rm -rf /tmp/reproit-subject
  run_case "$WORK/rb-capsule.json" "ruby $WORK/script.rb" 3 \
    "ruby subject fails closed rather than claiming a reproduction" "INCONCLUSIVE"
else
  echo "SKIP ruby case: no ruby in this image"
fi

echo "process-e2e: all four verdicts hold"
