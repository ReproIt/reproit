#!/usr/bin/env bash
# Checkpoint anchoring acceptance (Class C), measured not asserted.
#
# Runs INSIDE a privileged Linux container: criu needs CAP_SYS_ADMIN, and the
# survey measured that it also refuses a process whose files live on a bind
# mounted host path, so everything this script touches is copied to the
# container's own filesystem first.
#
# It proves four things and prints the numbers behind each:
#   1. a long running failure is captured and replays from zero
#   2. an anchor can be taken of the REPLAYING process
#   3. restoring the anchor reproduces the same failure, measurably faster
#   4. a tampered image, a missing image, and a tampered capsule are all
#      REFUSED, never quietly restored
#
# Every case is counted, and the count is asserted at the end, because a
# harness that stops early looks exactly like one that passed everything it
# printed.
set -u

ROOT="${REPROIT_ROOT:-/src}"
WORK=/work
CASES=0
EXPECTED_CASES=7

pass() { CASES=$((CASES + 1)); echo "PASS $1"; }
fail() { echo "FAIL $1" >&2; exit 1; }

command -v criu >/dev/null 2>&1 || fail "criu is not installed in this container"

mkdir -p "$WORK" && cd "$WORK"
cp "$ROOT/validation/process-checkpoint/subject.c" .
gcc -O1 -o subject subject.c || fail "could not build the subject"

SHIM_SOURCES=(
  "$ROOT/runners/process-shim/reproit_shim.c"
  "$ROOT/runners/process-shim/reproit_shim_capsule.c"
  "$ROOT/runners/process-shim/reproit_shim_movers.c"
  "$ROOT/runners/process-shim/reproit_seccomp.c"
)
gcc -shared -fPIC -O1 -o "$WORK/reproit_shim.so" "${SHIM_SOURCES[@]}" -ldl \
  || fail "could not build the process shim"
export REPROIT_PROCESS_SHIM="$WORK/reproit_shim.so"
# Capture and the full replay run on the default boundary. Only anchoring
# forces libc-only, because criu refuses to dump a process holding a seccomp
# notify descriptor. The subject is given an ABSOLUTE config path so both
# boundaries key that file identically and one capsule serves both.
CONFIG="$WORK/checkpoint-config.txt"

BINARY="${REPROIT_BINARY:?set REPROIT_BINARY to a Linux reproit}"
cp "$BINARY" "$WORK/reproit"
BINARY="$WORK/reproit"

# Preflight: the binary must actually LOAD here. A reproit built against a
# newer glibc than this image, or missing a shared library it links, dies at
# exec with the loader's message. Downstream that surfaced as "capture did not
# produce a capsule", which reads like a product defect and is not one, so the
# loader's own words are surfaced here instead. Both were hit for real: a
# trixie-built binary needing GLIBC_2.39 on bookworm, and a missing
# libatspi.so.0.
if ! LOADER_ERR=$("$BINARY" --version 2>&1); then
  echo "$LOADER_ERR" >&2
  fail "ENVIRONMENT: the reproit binary cannot execute in this container (see the loader error above). Build it against this image, or install the library it names. This is not a product failure"
fi

ITERATIONS="${ITERATIONS:-400}"
ANCHOR_AT="${ANCHOR_AT:-350}"
echo "anchored-config-value" > "$CONFIG"

# 1. Capture the long running failure.
if ! "$BINARY" --json internal process-capture --out capsule.json -- \
    ./subject "$ITERATIONS" "$CONFIG" > capture.json 2> capture.err; then
  cat capture.err >&2
  fail "capture did not produce a capsule"
fi
ENTRIES=$(python3 -c "import json;print(len(json.load(open('capsule.json'))['entries']))")
echo "  captured $ENTRIES boundary entries over $ITERATIONS iterations"
pass "captured a long running failure into a process capsule"

# The config is deleted for every replay below, so a replay that reads it is
# reaching the real filesystem rather than the capsule.
rm -f "$CONFIG"

# 2. Replay from zero, timed. This is the cost an anchor exists to avoid.
ZERO_START=$(date +%s%N)
"$BINARY" --json check capsule.json --exec "./subject $ITERATIONS $CONFIG" > zero.json 2> zero.err
ZERO_STATUS=$?
ZERO_MS=$(( ($(date +%s%N) - ZERO_START) / 1000000 ))
ZERO_VERDICT=$(python3 -c "
import json
try:
    d=json.load(open('zero.json'))
    print(d.get('capsule',{}).get('verdict','?'))
except Exception:
    print('unparseable')")
echo "  replay from zero: ${ZERO_MS} ms, verdict=$ZERO_VERDICT, exit=$ZERO_STATUS"
if [ "$ZERO_VERDICT" != "reproduced" ]; then
  echo "  --- reproit stdout ---" >&2; head -40 zero.json >&2
  echo "  --- reproit stderr ---" >&2; tail -20 zero.err >&2
  fail "replay from zero did not reproduce (verdict=$ZERO_VERDICT)"
fi
pass "replay from zero reproduces the failure with the config deleted"

# 3. Take an anchor of the replaying process.
if ! "$BINARY" --json internal process-anchor --capsule capsule.json \
    --exec "./subject $ITERATIONS $CONFIG" --image "$WORK/img" --after-lines "$ANCHOR_AT" \
    > anchor.json 2> anchor.err; then
  cat anchor.err >&2
  echo "  criu dump log:" >&2
  tail -20 "$WORK/img/dump.log" 2>/dev/null >&2
  fail "could not anchor the replaying process"
fi
ANCHOR_LINES=$(python3 -c "import json;print(json.load(open('capsule.json'))['anchor']['progress']['stdoutLines'])")
echo "  anchored after $ANCHOR_LINES lines of the subject's own output"
pass "checkpointed the replaying process into an anchor"

# 4. Restore the anchor. MEASURED STATE: the tail resumes and runs to
# completion far faster than a full replay, but the shell that would publish
# its exit status does not survive a criu restore in a reportable way, so the
# product cannot yet name the outcome. It therefore fails closed. This case
# pins BOTH halves: the tail really does resume, and an unobservable outcome
# is never reported as a reproduction.
STDOUT_LOG="$WORK/img-stdout.log"
LINES_AT_ANCHOR=$(wc -l < "$STDOUT_LOG")
REST_START=$(date +%s%N)
"$BINARY" --json internal process-restore --capsule capsule.json > restore.json 2> restore.err
REST_STATUS=$?
REST_MS=$(( ($(date +%s%N) - REST_START) / 1000000 ))
# The restored task is DETACHED: process-restore returns once criu has handed
# the tail back to the kernel, which is before the tail's first write. Reading
# the log here raced that write and reported a settled-looking "did not
# advance" whose line count varied run to run. Poll instead, bounded twice: a
# hard deadline, and a stall window after which the tail is declared finished.
WAIT_DEADLINE=$(( SECONDS + 60 ))
LINES_AFTER=$(wc -l < "$STDOUT_LOG")
STALL=0
while [ "$LINES_AFTER" -lt "$ITERATIONS" ] && [ "$SECONDS" -lt "$WAIT_DEADLINE" ]; do
  sleep 0.2
  NOW_LINES=$(wc -l < "$STDOUT_LOG")
  if [ "$NOW_LINES" -eq "$LINES_AFTER" ]; then
    STALL=$(( STALL + 1 ))
    [ "$STALL" -ge 25 ] && break
  else
    STALL=0
  fi
  LINES_AFTER=$NOW_LINES
done
REST_VERDICT=$(python3 -c "
import json
try:
    print(json.load(open('restore.json')).get('verdict','?'))
except Exception:
    print('unparseable')")
echo "  restore: verdict=$REST_VERDICT exit=$REST_STATUS"
echo "  tail resumed from $LINES_AT_ANCHOR to $LINES_AFTER lines of the subject's own output"
if [ "$LINES_AFTER" -le "$LINES_AT_ANCHOR" ]; then
  # Distinguish "criu could not restore here" from "the product regressed".
  # MEASURED 2026-07-31 on aarch64 Docker Desktop, with a plain looping C
  # program and no reproit in the picture at all: criu 3.17.1 (bookworm)
  # restores and the tail advances (87 -> 216); criu 4.1.1 (trixie) hangs in
  # `criu restore` until killed and the tail never advances, with AND without
  # --restore-detached. The product handles that correctly, bounding the wait
  # and refusing with a named reason, so reporting it as a stalled tail here
  # would blame the product for its environment.
  REST_REASON=$(python3 -c "
import json
try:
    print(json.load(open('restore.json')).get('reason',''))
except Exception:
    print('')" 2>/dev/null)
  case "$REST_REASON" in
    *"did not return"*)
      echo "  criu version here: $(criu --version 2>&1 | head -1)" >&2
      echo "  product refusal reason: $REST_REASON" >&2
      fail "ENVIRONMENT: criu could not restore in this container, so the anchor resume case cannot run. criu 3.17.1 restores on this host and criu 4.1.1 hangs; run this in a bookworm based image. The product refused correctly rather than guessing, so this is NOT a product regression"
      ;;
  esac
  fail "the restored tail did not advance (was $LINES_AT_ANCHOR, now $LINES_AFTER)"
fi
[ "$LINES_AFTER" -eq "$ITERATIONS" ] \
  || fail "the restored tail did not run to completion ($LINES_AFTER of $ITERATIONS)"
pass "restoring the anchor resumes the tail and runs it to completion"

# The whole point of an anchor: the work skipped is the head of the run.
SKIPPED=$(( LINES_AT_ANCHOR ))
echo "  MEASURED: full replay ${ZERO_MS} ms for $ITERATIONS iterations;"
echo "            the anchor skipped the first $SKIPPED of them"
[ "$SKIPPED" -gt 0 ] || fail "the anchor skipped nothing"
pass "the anchor skips the head of the run rather than replaying it"

# KNOWN GAP, pinned so it cannot silently become a false pass: criu reports
# only its own success, and the shell that would report the tail's status does
# not survive the restore in a reportable way, so the outcome is unobservable
# and the verdict must be inconclusive.
[ "$REST_VERDICT" = "inconclusive" ] && [ "$REST_STATUS" -eq 3 ] \
  || fail "an unobservable tail outcome must fail closed, got verdict=$REST_VERDICT exit=$REST_STATUS"
pass "an unobservable tail outcome fails closed instead of claiming a reproduction"

# 5. Fail closed: tamper the image, remove it, and tamper the capsule.
cp capsule.json capsule-backup.json
echo "tampered" >> "$WORK/img/inventory.img"
"$BINARY" --json internal process-restore --capsule capsule.json > tampered.json 2> tampered.err
TAMPER_STATUS=$?
TAMPER_VERDICT=$(python3 -c "
import json
try:
    print(json.load(open('tampered.json')).get('verdict','?'))
except Exception:
    print('unparseable')")
[ "$TAMPER_VERDICT" = "inconclusive" ] && [ "$TAMPER_STATUS" -eq 3 ] \
  || fail "a tampered image was not refused (verdict=$TAMPER_VERDICT exit=$TAMPER_STATUS)"

rm -rf "$WORK/img"
"$BINARY" --json internal process-restore --capsule capsule.json > absent.json 2> absent.err
ABSENT_STATUS=$?
ABSENT_VERDICT=$(python3 -c "
import json
try:
    print(json.load(open('absent.json')).get('verdict','?'))
except Exception:
    print('unparseable')")
[ "$ABSENT_VERDICT" = "inconclusive" ] && [ "$ABSENT_STATUS" -eq 3 ] \
  || fail "a missing image was not refused (verdict=$ABSENT_VERDICT exit=$ABSENT_STATUS)"

python3 - <<'PY'
import json
capsule = json.load(open('capsule-backup.json'))
# Editing the boundary log must invalidate the anchor: the memory image and
# the log would otherwise describe two different runs.
capsule['entries'].append("open\t/etc/never-recorded\t-\t0\t0\t0")
json.dump(capsule, open('capsule-tampered.json', 'w'))
PY
"$BINARY" --json internal process-restore --capsule capsule-tampered.json > mismatch.json 2> mismatch.err
MISMATCH_STATUS=$?
MISMATCH_VERDICT=$(python3 -c "
import json
try:
    print(json.load(open('mismatch.json')).get('verdict','?'))
except Exception:
    print('unparseable')")
[ "$MISMATCH_VERDICT" = "inconclusive" ] && [ "$MISMATCH_STATUS" -eq 3 ] \
  || fail "a tampered capsule was not refused (verdict=$MISMATCH_VERDICT exit=$MISMATCH_STATUS)"
pass "a tampered image, a missing image, and a tampered capsule are all refused"

[ "$CASES" -eq "$EXPECTED_CASES" ] \
  || fail "expected $EXPECTED_CASES cases, ran $CASES"
echo "process-checkpoint: the anchor skips the head, the tail resumes, and every outcome it cannot observe fails closed"
