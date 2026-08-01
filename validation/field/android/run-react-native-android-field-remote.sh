#!/usr/bin/env bash
# Drive one React Native Android field campaign on the native x86_64 worker.
#
# Every local Android system image on the development machine is arm64-v8a, so
# the react-native-android bound of android-emulator/x86_64 can only be met on
# the zgx gateway's strix host. This is the field-campaign twin of
# validation/release/run-android-x86-remote.sh: it ships the tracked tree plus
# the two application archives under test, runs the campaign inside the same
# pinned worker image with Docker network mode none, and collects the evidence.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
GATEWAY="${REPROIT_ANDROID_GATEWAY:-black@zgx-5a09.local}"
LINUX_HOST="${REPROIT_ANDROID_HOST:-strix}"
OUTPUT_DIR="${REPROIT_FIELD_OUTPUT_DIR:-$ROOT/target/reproit-validation/react-native-android-field}"
MAX_ARCHIVE_BYTES=$((512 * 1024 * 1024))
MAX_PAYLOAD_BYTES=$((512 * 1024 * 1024))
SOURCE_MODE=exact
KEEP_REMOTE=0
APPLICATION=""
AFFECTED_APK=""
FIXED_APK=""
FIXTURE_DIR=""
WITH_CORPUS=0
RUNS=3

for remote_name in "$GATEWAY" "$LINUX_HOST"; do
  [[ "$remote_name" =~ ^[A-Za-z0-9._@-]{1,255}$ ]] || {
    echo "invalid remote host name: $remote_name" >&2
    exit 2
  }
done

usage() {
  cat <<'EOF'
usage: validation/field/android/run-react-native-android-field-remote.sh
         --application joplin|music|notesnook
         --affected-apk PATH --fixed-apk PATH
         [--fixture-dir PATH] [--runs 1-3] [--with-corpus]
         [--current-tree] [--keep-remote]

Options:
  --application ID  React Native application under campaign.
  --affected-apk P  Application archive built at the affected revision.
  --fixed-apk P     Application archive built at the fixed revision.
  --fixture-dir P   Media fixture directory; required by the music campaign.
  --runs N          Reproductions per revision, 1 to 3. Defaults to 3.
  --with-corpus     Also run the per-target clean and adversarial corpus.
  --current-tree    Ship the current tracked tree as diagnostic evidence.
  --keep-remote     Retain the owned remote directory for diagnosis.
EOF
}

while (($#)); do
  case "$1" in
    --application)
      [[ $# -ge 2 ]] || { echo "--application requires an id" >&2; exit 2; }
      APPLICATION="$2"
      shift 2
      ;;
    --affected-apk)
      [[ $# -ge 2 ]] || { echo "--affected-apk requires a path" >&2; exit 2; }
      AFFECTED_APK="$2"
      shift 2
      ;;
    --fixed-apk)
      [[ $# -ge 2 ]] || { echo "--fixed-apk requires a path" >&2; exit 2; }
      FIXED_APK="$2"
      shift 2
      ;;
    --fixture-dir)
      [[ $# -ge 2 ]] || { echo "--fixture-dir requires a path" >&2; exit 2; }
      FIXTURE_DIR="$2"
      shift 2
      ;;
    --runs)
      [[ $# -ge 2 ]] || { echo "--runs requires a count" >&2; exit 2; }
      RUNS="$2"
      shift 2
      ;;
    --with-corpus)
      WITH_CORPUS=1
      shift
      ;;
    --current-tree)
      SOURCE_MODE=current-tree
      shift
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

case "$APPLICATION" in
  joplin|notesnook) ;;
  music)
    [[ -n "$FIXTURE_DIR" ]] || {
      echo "the music campaign requires --fixture-dir" >&2
      exit 2
    }
    ;;
  *)
    echo "unsupported React Native application: ${APPLICATION:-none}" >&2
    exit 2
    ;;
esac
[[ "$RUNS" =~ ^[1-3]$ ]] || {
  echo "--runs must be 1, 2, or 3" >&2
  exit 2
}
for apk in "$AFFECTED_APK" "$FIXED_APK"; do
  [[ -f "$apk" ]] || {
    echo "application archive is not a file: ${apk:-none}" >&2
    exit 2
  }
  bytes="$(wc -c <"$apk" | tr -d ' ')"
  if ((bytes > MAX_PAYLOAD_BYTES)); then
    echo "application archive exceeds the 512 MiB bound: $apk" >&2
    exit 2
  fi
done
if [[ -n "$FIXTURE_DIR" && ! -d "$FIXTURE_DIR" ]]; then
  echo "fixture directory is not a directory: $FIXTURE_DIR" >&2
  exit 2
fi

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
RUN_ID="reproit-rn-field-$(date -u +%Y%m%dT%H%M%SZ)-$$"
REMOTE_BASE=".cache/reproit-android-validation/$RUN_ID"
REMOTE_ARCHIVE="$REMOTE_BASE/source.tar.gz"
REMOTE_PAYLOAD="$REMOTE_BASE/payload.tar.gz"
REMOTE_RESULT="$REMOTE_BASE/result.tar.gz"
ARCHIVE="$WORK/source.tar.gz"
PAYLOAD="$WORK/payload.tar.gz"
RESULT="$WORK/result.tar.gz"

remote() {
  local command="$1"
  ssh "$GATEWAY" "ssh $LINUX_HOST '$command'"
}

cleanup() {
  if [[ "$KEEP_REMOTE" == 0 ]]; then
    remote "rm -rf $REMOTE_BASE" >/dev/null 2>&1 || true
  else
    echo "React Native field remote work retained: $GATEWAY -> $LINUX_HOST:$REMOTE_BASE" >&2
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

# The worker verifies the commit and, in exact mode, the cleanliness of what it
# received, so the archive has to carry a working repository and not only the
# files. Packing this checkout's own .git cannot do that from a linked git
# worktree, where .git is a FILE naming an absolute path that does not exist on
# the worker, and the worker's first act is then "fatal: not a git repository".
# A depth-1 clone would fix the shape but not the size: this repository's
# history is over 512 MiB, which is the archive bound.
#
# What is staged instead is a one-commit repository holding exactly the objects
# HEAD names. index-pack verifies those objects against the commit on the
# worker, and the emptiness of `git status` there proves the files it received
# are that commit's tree, which is what the mode was ever asserting.
STAGE="$WORK/source"
mkdir -p "$STAGE"
git -C "$STAGE" init --quiet
mkdir -p "$STAGE/.git/objects/pack"
git -C "$ROOT" rev-list --objects --no-walk HEAD |
  git -C "$ROOT" pack-objects --quiet "$STAGE/.git/objects/pack/snapshot"
printf '%s\n' "$COMMIT" >"$STAGE/.git/shallow"
git -C "$STAGE" update-ref refs/heads/staged "$COMMIT"
git -C "$STAGE" symbolic-ref HEAD refs/heads/staged
if [[ "$SOURCE_MODE" == current-tree ]]; then
  git -C "$ROOT" ls-files -co --exclude-standard -z >"$WORK/files"
else
  git -C "$ROOT" ls-files -z >"$WORK/files"
fi
(
  cd "$ROOT"
  COPYFILE_DISABLE=1 tar --no-xattrs --null -T "$WORK/files" -cf -
) | tar -xf - -C "$STAGE"
git -C "$STAGE" read-tree HEAD
if [[ "$(git -C "$STAGE" rev-parse HEAD)" != "$COMMIT" ]]; then
  echo "staged source is not at the requested commit" >&2
  exit 2
fi
if [[ "$SOURCE_MODE" == exact && -n "$(git -C "$STAGE" status --porcelain=v1)" ]]; then
  echo "staged exact source does not match its commit" >&2
  exit 2
fi
COPYFILE_DISABLE=1 tar --no-xattrs -C "$STAGE" -czf "$ARCHIVE" .
ARCHIVE_BYTES="$(wc -c <"$ARCHIVE" | tr -d ' ')"
if ((ARCHIVE_BYTES > MAX_ARCHIVE_BYTES)); then
  echo "source archive exceeds the 512 MiB bound" >&2
  exit 2
fi
ARCHIVE_SHA256="$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')"

mkdir -p "$WORK/payload"
cp "$AFFECTED_APK" "$WORK/payload/affected.apk"
cp "$FIXED_APK" "$WORK/payload/fixed.apk"
if [[ -n "$FIXTURE_DIR" ]]; then
  cp -R "$FIXTURE_DIR" "$WORK/payload/fixtures"
fi
COPYFILE_DISABLE=1 tar --no-xattrs -C "$WORK/payload" -czf "$PAYLOAD" .
PAYLOAD_BYTES="$(wc -c <"$PAYLOAD" | tr -d ' ')"
if ((PAYLOAD_BYTES > MAX_PAYLOAD_BYTES)); then
  echo "campaign payload exceeds the 512 MiB bound" >&2
  exit 2
fi
PAYLOAD_SHA256="$(shasum -a 256 "$PAYLOAD" | awk '{print $1}')"
AFFECTED_SHA256="$(shasum -a 256 "$AFFECTED_APK" | awk '{print $1}')"
FIXED_SHA256="$(shasum -a 256 "$FIXED_APK" | awk '{print $1}')"

remote "mkdir -p $REMOTE_BASE"
ssh "$GATEWAY" "ssh $LINUX_HOST 'cat > $REMOTE_ARCHIVE'" <"$ARCHIVE"
ssh "$GATEWAY" "ssh $LINUX_HOST 'cat > $REMOTE_PAYLOAD'" <"$PAYLOAD"

set +e
remote "mkdir -p $REMOTE_BASE/source && \
tar -xzf $REMOTE_ARCHIVE -C $REMOTE_BASE/source && \
timeout 15000 bash $REMOTE_BASE/source/validation/field/android/\
react-native-android-field-worker.sh \
$REMOTE_BASE $SOURCE_MODE $COMMIT $ARCHIVE_SHA256 $PAYLOAD_SHA256 \
$APPLICATION $RUNS $WITH_CORPUS $AFFECTED_SHA256 $FIXED_SHA256"
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
  mkdir -p "$OUTPUT_DIR/$APPLICATION"
  tar -xzf "$RESULT" -C "$OUTPUT_DIR/$APPLICATION"
  echo "React Native Android field evidence collected: $OUTPUT_DIR/$APPLICATION"
else
  echo "remote React Native field run returned no evidence archive" >&2
fi
exit "$REMOTE_STATUS"
