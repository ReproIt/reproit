#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
GATEWAY="${REPROIT_ANDROID_GATEWAY:-black@zgx-5a09.local}"
LINUX_HOST="${REPROIT_ANDROID_HOST:-strix}"
OUTPUT_DIR="${REPROIT_GATE_OUTPUT_DIR:-$ROOT/target/reproit-validation/android-x86}"
MAX_ARCHIVE_BYTES=$((512 * 1024 * 1024))
SOURCE_MODE=exact
KEEP_REMOTE=0
GATES=()

for remote_name in "$GATEWAY" "$LINUX_HOST"; do
  [[ "$remote_name" =~ ^[A-Za-z0-9._@-]{1,255}$ ]] || {
    echo "invalid remote host name: $remote_name" >&2
    exit 2
  }
done

usage() {
  cat <<'EOF'
usage: validation/release/run-android-x86-remote.sh [options]

Options:
  --current-tree  Run the current tracked and non-ignored tree as diagnostic evidence.
  --gate ID       Run one Android gate. Repeat to select several gates.
  --keep-remote   Retain the owned remote directory for diagnosis.
EOF
}

while (($#)); do
  case "$1" in
    --current-tree)
      SOURCE_MODE=current-tree
      shift
      ;;
    --gate)
      [[ $# -ge 2 ]] || {
        echo "--gate requires an id" >&2
        exit 2
      }
      GATES+=("$2")
      shift 2
      ;;
    --keep-remote)
      KEEP_REMOTE=1
      shift
      ;;
    --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if ((${#GATES[@]} == 0)); then
  GATES=(compose-android react-native-android flutter-android)
fi
if ((${#GATES[@]} > 3)); then
  echo "at most 3 Android gates may be selected" >&2
  exit 2
fi
for gate in "${GATES[@]}"; do
  case "$gate" in
    compose-android|react-native-android|flutter-android) ;;
    *)
      echo "gate is not assigned to the Android x86_64 lane: $gate" >&2
      exit 2
      ;;
  esac
done

COMMIT="$(git -C "$ROOT" rev-parse HEAD)"
[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || {
  echo "git returned an invalid commit" >&2
  exit 2
}
TREE_STATUS="$(git -C "$ROOT" status --porcelain=v1)"
if [[ "$SOURCE_MODE" == exact && -n "$TREE_STATUS" ]]; then
  echo "exact mode requires a clean worktree; use --current-tree for diagnostics" >&2
  exit 2
fi

WORK="$(mktemp -d)"
RUN_ID="reproit-android-$(date -u +%Y%m%dT%H%M%SZ)-$$"
REMOTE_BASE=".cache/reproit-android-validation/$RUN_ID"
REMOTE_ARCHIVE="$REMOTE_BASE/source.tar.gz"
REMOTE_RESULT="$REMOTE_BASE/result.tar.gz"
ARCHIVE="$WORK/source.tar.gz"
RESULT="$WORK/result.tar.gz"

remote() {
  local command="$1"
  ssh "$GATEWAY" "ssh $LINUX_HOST '$command'"
}

cleanup() {
  if [[ "$KEEP_REMOTE" == 0 ]]; then
    remote "rm -rf $REMOTE_BASE" >/dev/null 2>&1 || true
  else
    echo "Android remote work retained: $GATEWAY -> $LINUX_HOST:$REMOTE_BASE" >&2
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

bash "$ROOT/validation/release/list-current-tree-files.sh" "$ROOT" >"$WORK/files"
(
  cd "$ROOT"
  COPYFILE_DISABLE=1 tar --no-xattrs --null -T "$WORK/files" -czf "$ARCHIVE" .git
)
ARCHIVE_BYTES="$(wc -c <"$ARCHIVE" | tr -d ' ')"
if ((ARCHIVE_BYTES > MAX_ARCHIVE_BYTES)); then
  echo "source archive exceeds the 512 MiB bound" >&2
  exit 2
fi
ARCHIVE_SHA256="$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')"
GATE_CSV="$(IFS=,; echo "${GATES[*]}")"

remote "mkdir -p $REMOTE_BASE"
ssh "$GATEWAY" "ssh $LINUX_HOST 'cat > $REMOTE_ARCHIVE'" <"$ARCHIVE"

set +e
remote "mkdir -p $REMOTE_BASE/source && \
tar -xzf $REMOTE_ARCHIVE -C $REMOTE_BASE/source && \
timeout 15000 bash $REMOTE_BASE/source/validation/release/android-x86/remote-worker.sh \
$REMOTE_BASE $SOURCE_MODE $COMMIT $ARCHIVE_SHA256 $GATE_CSV"
REMOTE_STATUS=$?
set -e

if remote "test -f $REMOTE_RESULT"; then
  ssh "$GATEWAY" "ssh $LINUX_HOST 'cat $REMOTE_RESULT'" >"$RESULT"
  RESULT_BYTES="$(wc -c <"$RESULT" | tr -d ' ')"
  if ((RESULT_BYTES > MAX_ARCHIVE_BYTES)); then
    echo "evidence archive exceeds the 512 MiB bound" >&2
    exit 2
  fi
  python3 - "$RESULT" <<'PY'
import sys
import tarfile

with tarfile.open(sys.argv[1], "r:gz") as archive:
    for member in archive.getmembers():
        parts = member.name.split("/")
        if member.name.startswith("/") or ".." in parts:
            raise SystemExit(f"unsafe evidence archive member: {member.name}")
PY
  mkdir -p "$OUTPUT_DIR"
  tar -xzf "$RESULT" -C "$OUTPUT_DIR"
  echo "Android x86_64 evidence collected: $OUTPUT_DIR"
else
  echo "remote Android run returned no evidence archive" >&2
fi
exit "$REMOTE_STATUS"
