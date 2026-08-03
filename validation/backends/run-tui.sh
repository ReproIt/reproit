#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
FUZZ="$WORK/fuzz.json"
LOG="$WORK/run.log"
printf '{"budget":12}' > "$FUZZ"

cargo build -p reproit --manifest-path "$ROOT/Cargo.toml"
REPROIT_TUI_CMD='python3 examples/tui-demo/menu.py' \
REPROIT_TUI_CWD="$ROOT" \
REPROIT_FUZZ_CONFIG="$FUZZ" \
"$ROOT/target/debug/reproit" __tui | tee "$LOG"

grep -q '^EXPLORE:STATE ' "$LOG"
grep -q '^EXPLORE:EDGE ' "$LOG"
grep -q 'Now.*Playing\|Now Playing' "$LOG"
grep -q '^JOURNEY DONE$' "$LOG"
grep -q '^All tests passed$' "$LOG"
! grep -q 'EXCEPTION CAUGHT BY REPROIT' "$LOG"

echo "Tui backend passed native curses PTY runtime"
