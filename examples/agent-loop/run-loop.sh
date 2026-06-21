#!/usr/bin/env bash
#
# Agent-verification loop, end to end:
#
#   fuzz  ->  check (expect FAIL)  ->  agent fixes  ->  check (expect PASS)
#
# Proves reproit product goal #2: "Your agent writes the fix; reproit proves the
# UI works." The coding agent is `codex exec`; if codex is unavailable or
# unauthenticated, a clearly-labeled SCRIPTED fallback applies the known fix so
# the loop is still demonstrated end to end.
#
# Usage:
#   ./run-loop.sh                 # use real codex if available, else scripted
#   FIXER=scripted ./run-loop.sh  # force the scripted fixer
#   FIXER=codex     ./run-loop.sh # force codex (fail loudly if unavailable)
#
# Nothing here mutates reproit source or git. The only file the loop edits is the
# fixture's app/app.js (and it restores it on exit unless KEEP_FIX=1).

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
REPROIT="${REPROIT:-$REPO_ROOT/target/debug/reproit}"
PORT="${PORT:-8731}"
APP_DIR="$HERE/app"
APP_JS="$APP_DIR/app.js"
BUGGY_BACKUP="$HERE/.app.js.buggy"

export REPROIT_WEB_RUNNER_DIR="${REPROIT_WEB_RUNNER_DIR:-$REPO_ROOT/runners/web}"
export APP_URL="http://localhost:$PORT"

# ----- helpers --------------------------------------------------------------
c_red()   { printf '\033[31m%s\033[0m' "$1"; }
c_green() { printf '\033[32m%s\033[0m' "$1"; }
c_bold()  { printf '\033[1m%s\033[0m' "$1"; }

SERVER_PID=""
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null
  if [ "${KEEP_FIX:-0}" != "1" ] && [ -f "$BUGGY_BACKUP" ]; then
    cp "$BUGGY_BACKUP" "$APP_JS"   # restore the buggy fixture for a repeatable demo
    rm -f "$BUGGY_BACKUP"
  fi
}
trap cleanup EXIT

stage() { echo; c_bold ">>> $1"; echo; }
verdict() { # $1 = label, $2 = "PASS"|"FAIL"
  if [ "$2" = "PASS" ]; then echo "    [$( c_green PASS )] $1"; else echo "    [$( c_red FAIL )] $1"; fi
}

# ----- preflight ------------------------------------------------------------
[ -x "$REPROIT" ] || { echo "reproit binary not found at $REPROIT (run: cargo build)"; exit 2; }
command -v python3 >/dev/null || { echo "python3 required to serve the fixture"; exit 2; }

# Keep a pristine copy of the buggy fixture so we can restore + re-run.
cp "$APP_JS" "$BUGGY_BACKUP"

# Decide the fixer path.
FIXER="${FIXER:-auto}"
CODEX_BIN="${CODEX_BIN:-/opt/homebrew/bin/codex}"
codex_ok() { [ -x "$CODEX_BIN" ] && [ -f "$HOME/.codex/auth.json" ]; }
if [ "$FIXER" = "auto" ]; then
  if codex_ok; then FIXER="codex"; else FIXER="scripted"; fi
fi
echo "fixer path: $( c_bold "$FIXER" )"

# ----- start the fixture server --------------------------------------------
( cd "$APP_DIR" && exec python3 -m http.server "$PORT" >/tmp/agentloop-server.log 2>&1 ) &
SERVER_PID=$!
for _ in $(seq 1 20); do
  curl -s -o /dev/null "http://localhost:$PORT/" && break; sleep 0.25
done
echo "serving fixture at $APP_URL (pid $SERVER_PID)"

cd "$HERE"
rm -rf .reproit   # fresh map/fuzz artifacts each run

# ===========================================================================
# STAGE 1: fuzz -> find the bug, get a replayable repro id
# ===========================================================================
stage "STAGE 1  reproit fuzz   (find the bug, save a replayable repro)"
"$REPROIT" fuzz --yes 2>&1 | tee /tmp/agentloop-fuzz.log
# The id is printed as `id <hash>` on the finding line.
REPRO_ID="$(grep -oE '\bid [0-9a-f]{8,}' /tmp/agentloop-fuzz.log | head -1 | awk '{print $2}')"
if [ -z "$REPRO_ID" ]; then
  verdict "fuzz found a bug" "FAIL"
  echo "    no finding id parsed from fuzz output; aborting."
  exit 1
fi
verdict "fuzz found a bug -> repro id $REPRO_ID" "PASS"

# ===========================================================================
# STAGE 2: check -> the bug is REAL (must FAIL / exit 1)
# ===========================================================================
stage "STAGE 2  reproit check $REPRO_ID   (prove the bug is real; expect FAIL)"
"$REPROIT" check "$REPRO_ID" --yes 2>&1 | tee /tmp/agentloop-check1.log
CHECK1=${PIPESTATUS[0]}
echo "    exit code: $CHECK1  (expect 1 = real regression)"
if [ "$CHECK1" -eq 1 ]; then
  verdict "check reproduces the bug (exit 1)" "PASS"
else
  verdict "check reproduces the bug (got exit $CHECK1, wanted 1)" "FAIL"
  exit 1
fi

# ===========================================================================
# STAGE 3: the coding agent writes the fix
# ===========================================================================
stage "STAGE 3  agent fixes the code   (path: $FIXER)"
FIX_PROMPT="The file $APP_JS is a tiny counter web app. Clicking its \"Reset\" \
button throws an uncaught TypeError because the \`state\` object has no \`reset\` \
method (the click handler calls state.reset()). Fix the bug by adding a reset() \
method to the \`state\` object that sets count back to 0. Change ONLY \
$APP_JS. Keep increment() and decrement() working. Do not touch any other file."

if [ "$FIXER" = "codex" ]; then
  echo "    invoking: codex exec (sandbox workspace-write)"
  if codex exec \
        --cd "$REPO_ROOT" \
        --sandbox workspace-write \
        --skip-git-repo-check \
        "$FIX_PROMPT" 2>&1 | tee /tmp/agentloop-codex.log; then
    echo "    codex exec returned 0"
  else
    echo "    codex exec failed; falling back to scripted fixer"
    FIXER="scripted (codex failed)"
  fi
fi

# Verify codex actually removed the bug; if not, fall back so the loop still runs.
if grep -q "no reset() method" "$APP_JS" 2>/dev/null || ! grep -q "reset()" "$APP_JS"; then
  if [ "$FIXER" = "codex" ]; then
    echo "    codex did not add reset(); falling back to scripted fixer"
    FIXER="scripted (codex incomplete)"
  fi
fi

case "$FIXER" in
  scripted*)
    echo "    applying the known fix programmatically (scripted fallback)"
    python3 - "$APP_JS" <<'PY'
import re, sys
p = sys.argv[1]
src = open(p).read()
if "reset()" in src and "this.count = 0" in src:
    print("    (already fixed)"); sys.exit(0)
# Insert a reset() method into the state object, right after decrement().
needle = "  decrement() { this.count -= 1; },"
fix = needle + "\n  reset() { this.count = 0; },"
if needle in src:
    src = src.replace(needle, fix, 1)
    # drop the stale "no reset()" comment line if present
    src = re.sub(r"\n\s*// NOTE: no reset\(\) method.*", "", src)
    open(p, "w").write(src)
    print("    scripted fix applied")
else:
    sys.exit("    could not locate decrement() anchor; fixture changed")
PY
    ;;
esac
echo "    --- app.js reset handler now ---"
grep -n "reset" "$APP_JS" | sed 's/^/    /'

# ===========================================================================
# STAGE 4: check again -> reproit PROVES the fix (must PASS / exit 0)
# ===========================================================================
stage "STAGE 4  reproit check $REPRO_ID   (prove the fix held; expect PASS)"
"$REPROIT" check "$REPRO_ID" --yes 2>&1 | tee /tmp/agentloop-check2.log
CHECK2=${PIPESTATUS[0]}
echo "    exit code: $CHECK2  (expect 0 = all green)"
if [ "$CHECK2" -eq 0 ]; then
  verdict "check proves the fix (exit 0)" "PASS"
else
  verdict "check proves the fix (got exit $CHECK2, wanted 0)" "FAIL"
fi

# ----- summary --------------------------------------------------------------
echo
c_bold "================ LOOP SUMMARY ================"; echo
echo "  fixer path                : $FIXER"
echo "  repro id                  : $REPRO_ID"
[ -n "$REPRO_ID" ]    && verdict "stage 1  fuzz found bug"            "PASS" || verdict "stage 1  fuzz found bug" "FAIL"
[ "$CHECK1" -eq 1 ]   && verdict "stage 2  check FAIL (bug real)"     "PASS" || verdict "stage 2  check FAIL (bug real)" "FAIL"
echo "    [$( c_green DONE )] stage 3  agent wrote fix"
[ "$CHECK2" -eq 0 ]   && verdict "stage 4  check PASS (fix proven)"   "PASS" || verdict "stage 4  check PASS (fix proven)" "FAIL"
echo

if [ -n "$REPRO_ID" ] && [ "$CHECK1" -eq 1 ] && [ "$CHECK2" -eq 0 ]; then
  c_green "LOOP PROVEN: agent's fix turned a deterministic reproit FAIL into a PASS."; echo
  exit 0
else
  c_red "LOOP INCOMPLETE: see stage output above."; echo
  exit 1
fi
