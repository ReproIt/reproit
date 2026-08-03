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

# Case accounting. An acceptance script that stops early prints only PASS
# lines and looks exactly like one that passed everything it printed, which is
# the shape this project hit four separate times in one day. Every case
# increments this counter after it has fully asserted, and the run fails
# loudly if the total does not match.
CASES_RUN=0
# 13 through phase 3, plus: ruby byte identity (the abstention became a
# reproduction), relative-path keying and its byte identity, the two static
# binary refusals, and keeping a capsule as a guard and replaying it by id.
EXPECTED_CASES=20

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
  "$ROOT/runners/process-shim/reproit_shim_movers.c"
  "$ROOT/runners/process-shim/reproit_shim_time.c")
# The syscall completeness layer is Linux only; this script already is.
SHIM_SOURCES+=("$ROOT/runners/process-shim/reproit_seccomp.c"
  "$ROOT/runners/process-shim/reproit_seccomp_scratch.c"
  "$ROOT/runners/process-shim/reproit_elf.c")
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
"$REPROIT" --yes process-capture --out "$WORK/capsule.json" -- "$WORK/subject" \
  > "$WORK/capture.txt" 2>&1
kill "$UPSTREAM_PID" 2>/dev/null; UPSTREAM_PID=""
if ! grep -q "fatal signal" "$WORK/capture.txt"; then
  echo "FAIL capture: the subject did not fail as planted" >&2
  cat "$WORK/capture.txt" >&2
  exit 1
fi
CASES_RUN=$((CASES_RUN + 1))
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
  CASES_RUN=$((CASES_RUN + 1))
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
"$REPROIT" --yes process-capture --out "$WORK/py-capsule.json" --   python3 "$WORK/script.py" > "$WORK/py-capture.txt" 2>&1
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
CASES_RUN=$((CASES_RUN + 1))
echo "PASS python3 replayed stdout is byte identical to the recording"

# A SECOND interpreted runtime, and the one that used to fail closed. Ruby was
# diagnosed as unreplayable because its library search "resolved in a different
# order"; measuring it instead showed the capsule serving
# /usr/lib/ruby/vendor_ruby/rubygems.rb at 74,490 bytes for a 37,245 byte file.
# The recording opens it TWICE and the replay concatenated both copies, so
# rubygems evaluated its own text twice and Debian's
# `alias upstream_default_path default_path` aliased itself into a
# SystemStackError. Reads are now scoped to the open they followed, and ruby
# reproduces like python does.
if command -v ruby > /dev/null 2>&1; then
  mkdir -p /tmp/reproit-subject
  printf 'boom' > /tmp/reproit-subject/input.txt
  cat > "$WORK/script.rb" <<'RB_SUBJECT'
data = File.read('/tmp/reproit-subject/input.txt').strip
puts "read:" + data
exit(data == "boom" ? 3 : 0)
RB_SUBJECT
  "$REPROIT" --yes process-capture --out "$WORK/rb-capsule.json" -- \
    ruby "$WORK/script.rb" > "$WORK/rb-capture.txt" 2>&1
  rm -rf /tmp/reproit-subject
  run_case "$WORK/rb-capsule.json" "ruby $WORK/script.rb" 1 \
    "ruby subject reproduces hermetically" "reproduced by re-execution"

  # Same byte-identity proof python gets: the verdict says how it died, this
  # says it produced the same output getting there.
  mkdir -p /tmp/reproit-subject
  printf 'boom' > /tmp/reproit-subject/input.txt
  LD_PRELOAD="$WORK/reproit_shim.so" REPROIT_RECORD="$WORK/rb-stdout.log" \
    ruby "$WORK/script.rb" > "$WORK/rb.record" 2>/dev/null
  rm -rf /tmp/reproit-subject
  LD_PRELOAD="$WORK/reproit_shim.so" REPROIT_REPLAY_LOG="$WORK/rb-stdout.log" \
    REPROIT_REPLAY_SEED=c0ffee00c0ffee00 \
    ruby "$WORK/script.rb" > "$WORK/rb.replay" 2>/dev/null
  if ! cmp -s "$WORK/rb.record" "$WORK/rb.replay"; then
    echo "FAIL ruby replayed stdout is not byte identical to the recording" >&2
    echo "  recorded: $(tr '\n' '|' < "$WORK/rb.record")" >&2
    echo "  replayed: $(tr '\n' '|' < "$WORK/rb.replay")" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS ruby replayed stdout is byte identical to the recording"
else
  echo "SKIP ruby case: no ruby in this image"
fi

# RELATIVE PATH KEYING, in both directions a bad key can break. Measured with
# the libc boundary alone, because that is the layer that had the defect: the
# syscall layer already resolved through /proc symmetrically.
#
# Before the fix this exact subject printed `A=<ERR>` with a divergence on a
# file the capsule held, AND `B=OUTERINNER`, two different files concatenated
# under one relative key with ZERO divergences. The second is the silent wrong
# replay, so byte identity is the assertion that matters here, not the verdict
# alone.
gcc -O1 -o "$WORK/relkey" "$ROOT/validation/process/relkey.c"
mkdir -p "$WORK/reldir/sub"
printf 'OUTER' > "$WORK/reldir/data.txt"
printf 'INNER' > "$WORK/reldir/sub/data.txt"
(
  cd "$WORK/reldir" || exit 1
  export REPROIT_SECCOMP=0
  "$REPROIT" --yes process-capture --out "$WORK/rel-capsule.json" -- \
    "$WORK/relkey" > "$WORK/rel-capture.txt" 2>&1
)
( cd "$WORK/reldir" && "$WORK/relkey" > "$WORK/rel.record" 2>/dev/null )
rm -f "$WORK/reldir/data.txt" "$WORK/reldir/sub/data.txt"
# The capsule carries the recorded cwd, so check replays from there no matter
# where it is run. Nothing below cds.
run_case "$WORK/rel-capsule.json" "$WORK/relkey" 1 \
  "a subject opening relative paths reproduces with both files deleted" \
  "reproduced by re-execution"

python3 - "$WORK/rel-capsule.json" "$WORK/rel.log" <<'PY'
import json, sys
capsule = json.load(open(sys.argv[1]))
open(sys.argv[2], "w").write("\n".join(capsule["entries"]) + "\n")
PY
( cd "$WORK/reldir" && LD_PRELOAD="$WORK/reproit_shim.so" REPROIT_SECCOMP=0 \
    REPROIT_REPLAY_LOG="$WORK/rel.log" REPROIT_REPLAY_SEED=c0ffee00c0ffee00 \
    "$WORK/relkey" > "$WORK/rel.replay" 2>/dev/null )
if ! cmp -s "$WORK/rel.record" "$WORK/rel.replay"; then
  echo "FAIL relative-path keying: replayed output is not the recorded output" >&2
  echo "  recorded: $(tr '\n' '|' < "$WORK/rel.record")" >&2
  echo "  replayed: $(tr '\n' '|' < "$WORK/rel.replay")" >&2
  exit 1
fi
CASES_RUN=$((CASES_RUN + 1))
echo "PASS two files sharing a relative NAME replay apart, byte for byte"

# STATIC BINARIES. The libc half of the boundary needs a dynamic loader, so a
# statically linked image is covered by the syscall layer ALONE: its files are
# seen and its clock, randomness, environment, and sockets are not. Both routes
# into that state must refuse, and neither may write a capsule.
gcc -static -O1 -o "$WORK/staticsub" "$ROOT/validation/process/subject.c" 2>/dev/null
if [[ -x "$WORK/staticsub" ]]; then
  mkdir -p /tmp/reproit-subject
  printf '{ "strict": true }' > /tmp/reproit-subject/config.json
  rm -f "$WORK/static-direct.json"
  "$REPROIT" --yes process-capture --out "$WORK/static-direct.json" -- \
    "$WORK/staticsub" > "$WORK/static-direct.txt" 2>&1
  if [[ -f "$WORK/static-direct.json" ]] \
    || ! grep -q "statically linked" "$WORK/static-direct.txt"; then
    echo "FAIL a statically linked subject must be refused before it runs" >&2
    cat "$WORK/static-direct.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS a statically linked subject is refused before it runs"

  # The launched command is judged before it runs, but a dynamic program can
  # exec into a static one afterwards, and a seccomp filter survives execve
  # while LD_PRELOAD does not. Measured: that shape produced a six entry
  # capsule that replayed as a clean "reproduced" while seeing none of the
  # libc classes. The supervisor now names the image and capture refuses.
  printf '#!/bin/sh\nexec %s\n' "$WORK/staticsub" > "$WORK/wrap.sh"
  chmod +x "$WORK/wrap.sh"
  rm -f "$WORK/static-wrapped.json"
  "$REPROIT" --yes process-capture --out "$WORK/static-wrapped.json" -- \
    "$WORK/wrap.sh" > "$WORK/static-wrapped.txt" 2>&1
  rm -rf /tmp/reproit-subject
  if [[ -f "$WORK/static-wrapped.json" ]] \
    || ! grep -q "INCOMPLETE in classes it cannot report" "$WORK/static-wrapped.txt"; then
    echo "FAIL a static image reached through a dynamic wrapper must be named incomplete" >&2
    cat "$WORK/static-wrapped.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS a static image behind a dynamic wrapper is refused as an INCOMPLETE capture"
else
  echo "SKIP static binary cases: this image cannot link -static"
fi

# PHASE 2: a timed input stream. A session shaped program's trigger is input
# arriving over time, not a single request, so the capsule stamps each input
# with the TICK it arrived on and replay holds it back until the program
# reaches that tick again.
#
# The engine's planted defect is a STALE COMBO, which fires only when presses
# arrive FAR APART. That direction is what makes this a test of the schedule
# rather than of the bytes: the same two presses back to back are safe, so a
# replay that delivered the recorded input immediately would NOT reproduce the
# crash. The first assertion below pins that premise.
if command -v sdl2-config > /dev/null 2>&1; then
  gcc -O1 -o "$WORK/engine" "$ROOT/validation/process/engine.c" \
    $(sdl2-config --cflags --libs) 2>/dev/null
  if [[ -x "$WORK/engine" ]]; then
    export SDL_VIDEODRIVER=dummy ENGINE_FRAMES=120
    printf 'uuu' > "$WORK/burst.in"
    "$WORK/engine" < "$WORK/burst.in" > "$WORK/burst.out" 2>/dev/null
    if [[ $? -ne 0 ]]; then
      echo "FAIL premise: the same bytes back to back should be SAFE" >&2
      cat "$WORK/burst.out" >&2
      exit 1
    fi
    CASES_RUN=$((CASES_RUN + 1))
    echo "PASS premise: the same bytes back to back are safe, so timing is the defect"

    cat > "$WORK/feeder.py" <<'FEEDER'
import sys, time
for _ in range(3):
    sys.stdout.buffer.write(b"u")
    sys.stdout.buffer.flush()
    time.sleep(0.25)
FEEDER
    python3 "$WORK/feeder.py" 2>/dev/null | "$REPROIT" --yes process-capture \
      --out "$WORK/spread.json" -- "$WORK/engine" > "$WORK/spread.txt" 2>&1
    if ! grep -q "fatal signal" "$WORK/spread.txt"; then
      echo "FAIL capture: the spread session did not crash as planted" >&2
      cat "$WORK/spread.txt" >&2
      exit 1
    fi
    # No shell redirect on purpose: replay serves stdin FROM THE CAPSULE, and a
    # `< /dev/null` inside --exec is itself an open the recording never made,
    # which the boundary correctly reports as a divergence.
    run_case "$WORK/spread.json" "$WORK/engine" 1 \
      "a crash that depends on input TIMING reproduces with no real input" \
      "reproduced by re-execution"
    run_case "$WORK/spread.json" "REPROIT_FIXED=1 $WORK/engine" 0 \
      "discarding the stale combo certifies the fix" "the program now exits cleanly"
  else
    echo "SKIP phase 2 engine case: SDL2 present but the engine did not build"
  fi
else
  echo "SKIP phase 2 engine case: no sdl2-config in this image"
fi

# PHASE 3: failure identity. Every failed assertion dies with SIGABRT, so the
# exit status alone cannot tell two of them apart. Without the recorded
# failure text, a replay that aborted for an UNRELATED reason was reported as
# a reproduction, which is a false proof in the one direction that matters.
gcc -O1 -o "$WORK/twoasserts" "$ROOT/validation/process/twoasserts.c"
mkdir -p /tmp/reproit-subject
printf 'boom' > /tmp/reproit-subject/input.txt
"$REPROIT" --yes process-capture --out "$WORK/assert.json" -- \
  "$WORK/twoasserts" > "$WORK/assert.txt" 2>&1
rm -rf /tmp/reproit-subject
run_case "$WORK/assert.json" "$WORK/twoasserts" 1 \
  "the same assertion reproduces" "reproduced by re-execution"
run_case "$WORK/assert.json" "OTHER_BUG=1 $WORK/twoasserts" 3 \
  "a DIFFERENT assertion with the same signal is not a reproduction" "INCONCLUSIVE"

# KEEPING a capsule as a regression test. A capsule that can reproduce a
# failure and cannot be RETAINED breaks find-keep-check for exactly the
# programs this format exists to serve. The guard is proven live at keep time,
# lands in .reproit/repros/<id>/ as capsule.json plus its boot recipe, and
# `reproit check <id>` replays it with no capsule path and no --exec.
mkdir -p "$WORK/keepproj"
KEPT="$(cd "$WORK/keepproj" && "$REPROIT" --yes --json keep "$WORK/capsule.json" \
  --exec "$WORK/subject" 2>"$WORK/keep.err")"
KEEP_STATUS=$?
KEPT_ID="$(printf '%s' "$KEPT" | python3 -c \
  'import json,sys
text = sys.stdin.read()
decoder = json.JSONDecoder()
at = 0
while at < len(text):
    start = text.find("{", at)
    if start < 0:
        break
    try:
        value, end = decoder.raw_decode(text, start)
    except ValueError:
        at = start + 1
        continue
    at = end
    if isinstance(value, dict) and value.get("command") == "keep":
        print(value["id"])
        break')"
if [[ "$KEEP_STATUS" -ne 0 || -z "$KEPT_ID" \
  || ! -f "$WORK/keepproj/.reproit/repros/$KEPT_ID/capsule.json" ]]; then
  echo "FAIL keep: a process capsule did not land as a guard" >&2
  cat "$WORK/keep.err" >&2
  printf '%s\n' "$KEPT" >&2
  exit 1
fi
CASES_RUN=$((CASES_RUN + 1))
echo "PASS a process capsule keeps as a guard ($KEPT_ID)"

# The prefixed id is the reference `check` resolves; the bare directory name
# is not, which is why keep prints the prefixed form.
( cd "$WORK/keepproj" && "$REPROIT" --yes check "rep_$KEPT_ID" ) > "$WORK/keep-check.txt" 2>&1
KEEP_CHECK=$?
if [[ "$KEEP_CHECK" -ne 1 ]] \
  || ! grep -q "reproduced by re-execution" "$WORK/keep-check.txt"; then
  echo "FAIL keep: the kept guard did not replay by id (exit $KEEP_CHECK)" >&2
  cat "$WORK/keep-check.txt" >&2
  exit 1
fi
CASES_RUN=$((CASES_RUN + 1))
echo "PASS the kept guard replays from its id alone, with no capsule path and no --exec"

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "process-e2e: all four verdicts hold ($CASES_RUN/$EXPECTED_CASES cases)"
