#!/usr/bin/env bash
# Measure the natural failure rate of a real data race, then measure it again
# under schedule fuzzing, for two race variants: one whose window crosses a
# libc boundary a preload can hook, and one that is pure memory traffic.
#
# Run inside Linux (Docker) so LD_PRELOAD applies. Exit code 42 from the
# subject means the race was observed.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
RUNS="${RUNS:-200}"
DELAY_NS="${DELAY_NS:-50000}"
RATE="${RATE:-50}"

cd "$HERE"
cc -O2 -pthread -o race race.c || exit 1
cc -O2 -fPIC -shared -o schedfuzz.so schedfuzz.c -ldl || exit 1

count_failures() {
  local variant="$1" fuzz="$2" observed=0 run=0 status=0
  for ((run = 0; run < RUNS; run++)); do
    if [[ "$fuzz" == "on" ]]; then
      REPROIT_SCHED_FUZZ=1 \
      REPROIT_SCHED_SEED="$run" \
      REPROIT_SCHED_DELAY_NS="$DELAY_NS" \
      REPROIT_SCHED_RATE="$RATE" \
      LD_PRELOAD="$HERE/schedfuzz.so" \
        ./race "$variant" >/dev/null 2>&1
      status=$?
    else
      ./race "$variant" >/dev/null 2>&1
      status=$?
    fi
    if [[ "$status" -eq 42 ]]; then
      observed=$((observed + 1))
    fi
  done
  echo "$observed"
}

echo "runs per cell: $RUNS   delay: ${DELAY_NS}ns   rate: ${RATE}%"
echo

printf '%-8s %-12s %-10s %s\n' "variant" "schedule" "observed" "rate"
for variant in pure libc; do
  for fuzz in off on; do
    observed="$(count_failures "$variant" "$fuzz")"
    rate="$(python3 -c "print(f'{100*$observed/$RUNS:.1f}%')")"
    printf '%-8s %-12s %-10s %s\n' "$variant" "$fuzz" "$observed/$RUNS" "$rate"
  done
done
