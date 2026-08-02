#!/usr/bin/env bash
set -uo pipefail

# Best-effort native stack capture of the still-running Flutter/Dart tool
# processes at stall time. Invoked by run-output-contract.py through
# --stall-diagnostic-command BEFORE the timed-out child is killed: the hung
# process is the evidence, and post-mortem there is nothing left to sample.
# Diagnostic only: bounded output, always exits 0, macOS only (`sample`).
#
# Measured need (2026-08-02, run 30747023317): three idle-timeout stalls where
# the tool went silent between "Xcode build done" and any app output; the
# simulator log alone cannot say where the TOOL is blocked.

MAX_PROCESSES=6
MAX_LINES_PER_PROCESS=150

if ! command -v sample >/dev/null; then
  echo "sample-stalled-tools: no sample(1) on this host; skipping"
  exit 0
fi

# Scope strictly to the stalled child's process group (the contract starts it
# with start_new_session, so the flutter tool and everything it spawned share
# that group). Never sample unrelated processes on the host.
stalled_pid="${REPROIT_STALLED_PID:-}"
if [[ -z "$stalled_pid" ]]; then
  echo "sample-stalled-tools: REPROIT_STALLED_PID is not set; skipping"
  exit 0
fi
pgid="$(ps -o pgid= -p "$stalled_pid" | tr -d ' ')"
if [[ -z "$pgid" ]]; then
  echo "sample-stalled-tools: stalled pid $stalled_pid already gone; skipping"
  exit 0
fi
pids="$(pgrep -g "$pgid" | head -n "$MAX_PROCESSES")"
if [[ -z "$pids" ]]; then
  echo "sample-stalled-tools: no live processes in group $pgid"
  exit 0
fi

for pid in $pids; do
  echo "==== stalled-tool sample pid $pid: $(ps -o command= -p "$pid" | cut -c1-160)"
  sample "$pid" 2 -file /dev/stdout 2>/dev/null | head -n "$MAX_LINES_PER_PROCESS"
done
exit 0
