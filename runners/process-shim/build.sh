#!/usr/bin/env bash
# Build the process shim for this platform. Sets nothing: the caller points
# REPROIT_PROCESS_SHIM at the result, because a capsule may never name a
# library to load.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${1:-$HERE/reproit_shim.so}"
case "$(uname -s)" in
  Darwin) SUFFIX="dylib" ;;
  *) SUFFIX="so" ;;
esac
cc -shared -fPIC -O1 -o "${OUT%.so}.$SUFFIX" \
  "$HERE/reproit_shim.c" "$HERE/reproit_shim_capsule.c" "$HERE/reproit_shim_movers.c" -ldl
echo "${OUT%.so}.$SUFFIX"
