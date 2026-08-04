#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:?usage: list-current-tree-files.sh ROOT}"
git -C "$ROOT" rev-parse --git-dir >/dev/null

git -C "$ROOT" ls-files -co --exclude-standard -z |
  while IFS= read -r -d '' source_path; do
    if [[ -e "$ROOT/$source_path" || -L "$ROOT/$source_path" ]]; then
      printf '%s\0' "$source_path"
    fi
  done
