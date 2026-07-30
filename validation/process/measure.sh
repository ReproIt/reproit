#!/usr/bin/env bash
# Phase 1 measurement: does an LD_PRELOAD boundary record enough of a real
# native program that replay does NOT diverge spuriously?
#
# Runs entirely inside the container: build shim + subject, start a local
# upstream, RECORD one failing run, then REPLAY it with the config file
# DELETED, the upstream DOWN, and no network, and print the raw counters.
set -u

WORK=/work
OUT=/tmp/reproit-process
mkdir -p "$OUT" /tmp/reproit-subject

echo "=== build ==="
gcc -shared -fPIC -O1 -o "$OUT/reproit_shim.so" "$WORK/runners/process-shim/reproit_shim.c" \
  "$WORK/runners/process-shim/reproit_shim_capsule.c" \
  "$WORK/runners/process-shim/reproit_shim_movers.c" -ldl \
  || { echo "SHIM BUILD FAILED"; exit 1; }
gcc -O1 -o "$OUT/subject" "$WORK/validation/process/subject.c" \
  || { echo "SUBJECT BUILD FAILED"; exit 1; }
echo "built"

start_upstream() {
  python3 - "$1" <<'PY' &
import socket, sys
limit = sys.argv[1]
body = '{"quote":42,"limit":%s}' % limit
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
        conn.sendall(("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n" + body).encode())
    finally:
        conn.close()
PY
  UPSTREAM_PID=$!
  sleep 0.6
}

stop_upstream() {
  kill "${UPSTREAM_PID:-0}" 2>/dev/null || true
  wait "${UPSTREAM_PID:-0}" 2>/dev/null || true
}

record_run() {
  local mode_env="$1" log="$2"
  rm -f "$log"
  mkdir -p /tmp/reproit-subject
  printf '{ "strict": true }' > /tmp/reproit-subject/config.json
  start_upstream null
  env $mode_env LD_PRELOAD="$OUT/reproit_shim.so" REPROIT_RECORD="$log" "$OUT/subject" \
    > "$OUT/record.out" 2> "$OUT/record.err"
  RECORD_STATUS=$?
  stop_upstream
}

replay_run() {
  local mode_env="$1" log="$2" extra_env="${3:-}"
  # Hermetic state: the config file is GONE and no upstream is listening.
  rm -rf /tmp/reproit-subject
  env $mode_env $extra_env LD_PRELOAD="$OUT/reproit_shim.so" REPROIT_REPLAY_LOG="$log" \
    REPROIT_REPLAY_SEED=c0ffee00c0ffee00 "$OUT/subject" \
    > "$OUT/replay.out" 2> "$OUT/replay.err"
  REPLAY_STATUS=$?
}

report() {
  local label="$1"
  echo "--- $label ---"
  echo "record exit: $RECORD_STATUS   replay exit: $REPLAY_STATUS"
  echo "recorded entries: $(wc -l < "$2" | tr -d ' ')"
  echo "  by kind:"
  awk -F'\t' '{print "    " $1}' "$2" | sort | uniq -c | sort -rn
  echo "replay counters:"
  grep -o 'REPROIT:PROCESS-REPLAY .*' "$OUT/replay.err" || echo "  (no counter line)"
  echo "divergences:"
  grep -c 'REPROIT:DIVERGENCE' "$OUT/replay.err" || true
  grep -o 'REPROIT:DIVERGENCE .*' "$OUT/replay.err" | head -10 || true
  echo "replay stdout:"
  sed 's/^/    /' "$OUT/replay.out"
  echo "replay stderr (non-marker):"
  grep -v 'REPROIT:' "$OUT/replay.err" | sed 's/^/    /'
}

echo
echo "=== A. POSIX path (open/read), planted defect present ==="
record_run "" "$OUT/posix.log"
replay_run "" "$OUT/posix.log"
report "posix" "$OUT/posix.log"

echo
echo "=== B. same capsule, program fixed (REPROIT_FIXED=1) ==="
replay_run "" "$OUT/posix.log" "REPROIT_FIXED=1"
echo "replay exit: $REPLAY_STATUS  (0 means the fix certifies)"
grep -o 'REPROIT:PROCESS-REPLAY .*' "$OUT/replay.err" || true

echo
echo "=== C. tampered capsule (socket data removed) ==="
grep -v $'^recv\t' "$OUT/posix.log" > "$OUT/tampered.log"
replay_run "" "$OUT/tampered.log"
echo "replay exit: $REPLAY_STATUS"
grep -o 'REPROIT:DIVERGENCE .*' "$OUT/replay.err" | head -3

echo
echo "=== D. stdio path (fopen/fread), measuring what glibc internals hide ==="
record_run "REPROIT_STDIO=1" "$OUT/stdio.log"
replay_run "REPROIT_STDIO=1" "$OUT/stdio.log"
report "stdio" "$OUT/stdio.log"
