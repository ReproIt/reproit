#!/bin/sh
# Prune stale cargo build artifacts. The workspace target/ (and the nested
# fixture target/ under validation/backend/oss) accumulate hundreds of GB of
# incremental artifacts that cargo never evicts. Deleting files inside a
# target/ dir is always safe: cargo rebuilds anything missing.
#
# Policy: delete FILES not modified in the last REPROIT_PRUNE_AGE_DAYS
# (default 7) inside gitignored directories literally named `target`, then
# drop emptied directories. Bounded and fail-closed:
#   - only directories git itself reports as ignored are eligible;
#   - a candidate containing ANY tracked file aborts the run;
#   - candidates must resolve inside the repository root.
#
# Usage: scripts/prune-target.sh [--dry-run]
set -eu

age_days="${REPROIT_PRUNE_AGE_DAYS:-7}"
case "$age_days" in
  *[!0-9]*|'') echo "REPROIT_PRUNE_AGE_DAYS must be a whole number" >&2; exit 1 ;;
esac

dry_run=0
if [ "${1:-}" = "--dry-run" ]; then
  dry_run=1
elif [ "$#" -gt 0 ]; then
  echo "usage: scripts/prune-target.sh [--dry-run]" >&2
  exit 1
fi

root="$(git rev-parse --show-toplevel)"
cd "$root"

freed_kb=0
for candidate in target validation/backend/oss/target; do
  [ -d "$candidate" ] || continue
  git check-ignore -q "$candidate" || {
    echo "refusing $candidate: not gitignored" >&2
    exit 1
  }
  if git ls-files -- "$candidate" | head -1 | grep -q .; then
    echo "refusing $candidate: contains tracked files" >&2
    exit 1
  fi
  resolved="$(cd "$candidate" && pwd -P)"
  case "$resolved" in
    "$root"/*) ;;
    *) echo "refusing $candidate: resolves outside the repository" >&2; exit 1 ;;
  esac

  before_kb="$(du -sk "$candidate" | cut -f1)"
  if [ "$dry_run" -eq 1 ]; then
    stale_kb="$(find "$candidate" -type f -mtime +"$age_days" -print0 \
      | xargs -0 du -sk 2>/dev/null | awk '{s+=$1} END {print s+0}')"
    echo "$candidate: would free ${stale_kb}KB of files older than ${age_days}d"
    continue
  fi
  find "$candidate" -type f -mtime +"$age_days" -delete
  find "$candidate" -mindepth 1 -type d -empty -delete
  after_kb="$(du -sk "$candidate" | cut -f1)"
  freed_kb=$((freed_kb + before_kb - after_kb))
  echo "$candidate: $((before_kb / 1024))MB -> $((after_kb / 1024))MB"
done

echo "freed $((freed_kb / 1024))MB (files older than ${age_days} days)"
