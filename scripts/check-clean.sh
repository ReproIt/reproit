#!/usr/bin/env bash
# Guard against leaking private identifiers (private app names, personal info)
# into this public repo. Run by the pre-push hook (.githooks/pre-push) at the
# last gate before anything goes public. The term list is supplied OUT OF BAND
# so the denylist is never itself committed here:
#   - a gitignored .forbidden-terms file (one term per line; # = comment), or
#   - the FORBIDDEN_TERMS env var (comma-separated), if you prefer.
# With no terms available it no-ops (so a fresh clone without the list isn't
# blocked). Matching is case-insensitive and whole-word.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

terms=()
if [[ -n "${FORBIDDEN_TERMS:-}" ]]; then
  IFS=',' read -ra raw <<<"$FORBIDDEN_TERMS"
  for t in "${raw[@]}"; do
    t="${t//[[:space:]]/}"
    [[ -n "$t" ]] && terms+=("$t")
  done
elif [[ -f .forbidden-terms ]]; then
  while IFS= read -r line; do
    line="${line%%#*}"
    line="${line//[[:space:]]/}"
    [[ -n "$line" ]] && terms+=("$line")
  done <.forbidden-terms
fi

if [[ ${#terms[@]} -eq 0 ]]; then
  echo "check-clean: no FORBIDDEN_TERMS / .forbidden-terms provided; skipping."
  exit 0
fi

pat="$(
  IFS='|'
  echo "${terms[*]}"
)"

# git grep over tracked files only; -w whole-word (portable, unlike \b);
# vendored third-party trees and the denylist files themselves are excluded.
hits="$(git grep -nIiwE -e "$pat" -- . \
  ':(exclude)*.lock' \
  ':(exclude)scripts/check-clean.sh' \
  ':(exclude).forbidden-terms.example' \
  ':(exclude)examples/imgui-headless' \
  ':(exclude)examples/clay-headless' 2>/dev/null || true)"

if [[ -n "$hits" ]]; then
  echo "ERROR: forbidden private identifier(s) found in tracked files:" >&2
  echo "$hits" >&2
  echo >&2
  echo "Remove them before pushing (blocked by the pre-push hook)." >&2
  exit 1
fi
echo "check-clean: clean (${#terms[@]} term(s) checked)."
