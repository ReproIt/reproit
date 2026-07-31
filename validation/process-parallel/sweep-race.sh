#!/usr/bin/env bash
# Sweep the race window width and measure the observed-failure rate with the
# schedule fuzzer off and on, for both variants. The interesting regime is a
# LOW natural rate: that is where a reproduction tool would actually need help,
# and therefore where fuzzing has to earn its claim.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
RUNS="${RUNS:-200}"
DELAY_NS="${DELAY_NS:-50000}"
RATE="${RATE:-50}"
WINDOWS="${WINDOWS:-0 1 4 16 64 256}"

cd "$HERE"
cc -O2 -pthread -o race race.c || exit 1
cc -O2 -fPIC -shared -o schedfuzz.so schedfuzz.c -ldl || exit 1

cell() {
  local variant="$1" fuzz="$2" window="$3" observed=0 run status
  for ((run = 0; run < RUNS; run++)); do
    if [[ "$fuzz" == "on" ]]; then
      REPROIT_RACE_WINDOW="$window" \
      REPROIT_SCHED_FUZZ=1 REPROIT_SCHED_SEED="$run" \
      REPROIT_SCHED_DELAY_NS="$DELAY_NS" REPROIT_SCHED_RATE="$RATE" \
      LD_PRELOAD="$HERE/schedfuzz.so" ./race "$variant" >/dev/null 2>&1
      status=$?
    else
      REPROIT_RACE_WINDOW="$window" ./race "$variant" >/dev/null 2>&1
      status=$?
    fi
    [[ "$status" -eq 42 ]] && observed=$((observed + 1))
  done
  echo "$observed"
}

echo "runs per cell: $RUNS   fuzz delay: ${DELAY_NS}ns   fire rate: ${RATE}%"
echo
printf '%-8s %-8s %-12s %-12s %s\n' "variant" "window" "natural" "fuzzed" "delta"
for variant in pure libc; do
  for window in $WINDOWS; do
    off="$(cell "$variant" off "$window")"
    on="$(cell "$variant" on "$window")"
    python3 - "$variant" "$window" "$off" "$on" "$RUNS" <<'PY'
import sys
variant, window, off, on, runs = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
o, f = 100 * off / runs, 100 * on / runs
print(f"{variant:<8} {window:<8} {off:>3}/{runs} {o:>5.1f}%  {on:>3}/{runs} {f:>5.1f}%  {f-o:+.1f}pp")
PY
  done
done
