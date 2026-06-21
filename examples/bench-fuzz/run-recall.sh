#!/usr/bin/env bash
#
# Autonomous-fuzz RECALL benchmark + coverage-fix A/B.
#
# Measures reproit product goal #3 ("point it at an app, get real repros") on a
# delegated-click SPA built ENTIRELY from <div role=option tabindex=-1
# data-testid=...> rows driven by one document-level click listener (no native
# <button>s). It reports, for each arm:
#   (a) STATES MAPPED   - distinct UI states the crawler reached
#   (b) RECALL          - how many of the 5 seeded bugs reproit found
#
# It runs two arms:
#   WITH  the coverage fix (current runners/web/runner.mjs)
#   WITHOUT it            (temporarily `git stash push -- runners/web/runner.mjs`,
#                          run, then `git stash pop` to restore it).
#
# The fix adds KEYED pointer-operable controls (cursor:pointer / ARIA-interactive
# role / focusable tabindex delegation) to the fuzzer's candidate set, so the
# explorer can actually tap the delegated <div>s. Without it, a delegated-click
# SPA maps to ~1 state / 0 transitions and almost every seeded bug is unreachable.
#
# This script never git-commits. The ONLY git operation is the stash/pop that
# temporarily reverts runner.mjs for the WITHOUT arm; it is always restored
# (verified) before the script exits, success or failure.
#
# Usage:
#   ./run-recall.sh            # run both arms (WITH then WITHOUT), print A/B table
#   ARM=with    ./run-recall.sh  # only the WITH arm (no stash)
#   ARM=without ./run-recall.sh  # only the WITHOUT arm (stash/pop)
#   LOCALES="en,de"            # locales to fuzz across (default en,de; de is needed
#                              #   for the locale-specific bug #5)

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
REPROIT="${REPROIT:-$REPO_ROOT/target/debug/reproit}"
RUNNER="$REPO_ROOT/runners/web/runner.mjs"
PORT="${PORT:-8741}"
APP_DIR="$HERE/app"
LOCALES="${LOCALES:-en,de}"
ARM="${ARM:-both}"

export REPROIT_WEB_RUNNER_DIR="${REPROIT_WEB_RUNNER_DIR:-$REPO_ROOT/runners/web}"
export APP_URL="http://localhost:$PORT"

c_bold()  { printf '\033[1m%s\033[0m' "$1"; }
c_green() { printf '\033[32m%s\033[0m' "$1"; }
c_red()   { printf '\033[31m%s\033[0m' "$1"; }
stage()   { echo; c_bold ">>> $1"; echo; }

# The 5 seeded bugs, each identified by a grep-able signature in the run
# artifacts. RECALL = how many of these the arm's fuzz surfaced.
#   1 crash      account.purge is not a function          (Danger > Delete account)
#   2 crash      reading 'serialize'                        (Profile > Save profile)
#   3 dead-end   the Appearance view (no outgoing edge)     (graph oracle)
#   4 a11y       unlabeled tappable                         (Notifications icon control)
#   5 i18n       formatDE is not a function, German only    (About, --locale de)

SERVER_PID=""
STASHED=0
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null
  # SAFETY: if we stashed runner.mjs and never popped (e.g. an error mid-arm),
  # restore it now so the pre-existing uncommitted fix is never lost.
  if [ "$STASHED" = "1" ]; then
    echo "cleanup: restoring runner.mjs (git stash pop)"
    ( cd "$REPO_ROOT" && git stash pop ) || echo "WARNING: stash pop failed; check 'git stash list'"
    STASHED=0
  fi
}
trap cleanup EXIT

# ----- preflight ------------------------------------------------------------
[ -x "$REPROIT" ] || { echo "reproit not built at $REPROIT (run: cargo build)"; exit 2; }
command -v python3 >/dev/null || { echo "python3 required"; exit 2; }
command -v git >/dev/null || { echo "git required"; exit 2; }

# ----- serve the fixture ----------------------------------------------------
( cd "$APP_DIR" && exec python3 -m http.server "$PORT" >/tmp/benchfuzz-server.log 2>&1 ) &
SERVER_PID=$!
for _ in $(seq 1 20); do curl -s -o /dev/null "$APP_URL/" && break; sleep 0.25; done
echo "serving fixture at $APP_URL (pid $SERVER_PID)"

# run_arm <label> -> sets globals STATES and RECALL (and prints the bug table).
run_arm() {
  local label="$1"
  local tag="${label// /_}"
  local outlog="/tmp/benchfuzz-${tag}.log"

  cd "$HERE"
  rm -rf .reproit
  stage "ARM: $label   (fuzz --all --locale $LOCALES)"
  "$REPROIT" fuzz --all --locale "$LOCALES" --yes 2>&1 | tee "$outlog" | grep -E "FINDING|unique bugs|locale diff|only in:|drive web" || true

  # STATES MAPPED: distinct EXPLORE:STATE signatures across all per-seed walks.
  # (The per-seed "explored N states" line is the same number for the last seed;
  #  distinct sigs across the whole run is the honest union.)
  STATES="$(grep -rho 'EXPLORE:STATE {"sig":"[^"]*"' .reproit/runs/*/drive-a.log 2>/dev/null | sort -u | wc -l | tr -d ' ')"

  # RECALL: count which of the 5 seeded bugs appear in the run artifacts.
  local all_md; all_md="$(cat .reproit/runs/finding-*/fuzz.md 2>/dev/null)"
  local found=0; BUGS_FOUND=""; BUGS_MISSED=""
  bug_seen() { # $1 = name, $2 = grep pattern, $3 = corpus
    if printf '%s' "$3" | grep -qE "$2"; then
      found=$((found+1)); BUGS_FOUND="$BUGS_FOUND $1"; return 0
    else
      BUGS_MISSED="$BUGS_MISSED $1"; return 1
    fi
  }
  # Bug 3 must be the APPEARANCE dead-end specifically (not the degenerate
  # home-state dead-end the WITHOUT arm reports), so match its screen hint.
  bug_seen "1:crash-purge"   "account\.purge is not a function"           "$all_md"
  bug_seen "2:crash-null"    "reading 'serialize'"                         "$all_md"
  bug_seen "3:dead-end-appearance" "no-dead-end.*\[Appearance"            "$all_md"
  bug_seen "4:a11y-unlabeled" "all-labeled.*unlabeled tappable"           "$all_md"
  # Bug 5 surfaces in the locale diff ("only in: de") of the run stdout.
  bug_seen "5:i18n-formatDE" "formatDE is not a function"                 "$(cat "$outlog")"

  RECALL="$found"
  echo
  echo "  states mapped : $STATES"
  echo "  recall        : $RECALL / 5"
  echo "  bugs found    :${BUGS_FOUND:- (none)}"
  echo "  bugs missed   :${BUGS_MISSED:- (none)}"
}

WITH_STATES=""; WITH_RECALL=""; WO_STATES=""; WO_RECALL=""

if [ "$ARM" = "both" ] || [ "$ARM" = "with" ]; then
  run_arm "WITH coverage fix"
  WITH_STATES="$STATES"; WITH_RECALL="$RECALL"
fi

if [ "$ARM" = "both" ] || [ "$ARM" = "without" ]; then
  stage "stashing runner.mjs to revert the coverage fix"
  ( cd "$REPO_ROOT" && git stash push -- runners/web/runner.mjs )
  STASHED=1
  if grep -q "pointerOperable" "$RUNNER"; then
    echo "ERROR: runner.mjs still has the fix after stash; aborting."
    exit 1
  fi
  echo "fix reverted (pointerOperable absent from runner.mjs)"

  run_arm "WITHOUT coverage fix"
  WO_STATES="$STATES"; WO_RECALL="$RECALL"

  stage "restoring runner.mjs (git stash pop)"
  ( cd "$REPO_ROOT" && git stash pop )
  STASHED=0
  if grep -q "pointerOperable" "$RUNNER"; then
    echo "$( c_green OK ): runner.mjs restored (pointerOperable present again)"
  else
    echo "$( c_red WARNING ): runner.mjs does NOT show the fix after pop; check git status."
  fi
fi

# ----- A/B summary ----------------------------------------------------------
echo
c_bold "================ RECALL A/B ================"; echo
printf "  %-22s %-14s %-12s\n" "arm" "states mapped" "recall"
printf "  %-22s %-14s %-12s\n" "----------------------" "-------------" "----------"
[ -n "$WO_STATES" ]   && printf "  %-22s %-14s %-12s\n" "WITHOUT coverage fix" "$WO_STATES" "$WO_RECALL / 5"
[ -n "$WITH_STATES" ] && printf "  %-22s %-14s %-12s\n" "WITH coverage fix"    "$WITH_STATES" "$WITH_RECALL / 5"
echo
if [ -n "$WITH_RECALL" ]; then
  pct=$(( WITH_RECALL * 100 / 5 ))
  c_bold "goal #3 metric: recall WITH fix = ${WITH_RECALL}/5 (${pct}%)"; echo
fi
echo "(raw fuzz output: /tmp/benchfuzz-*.log ; run artifacts: examples/bench-fuzz/.reproit/runs/)"
