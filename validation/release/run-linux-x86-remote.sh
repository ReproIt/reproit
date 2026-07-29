#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
GATEWAY="${REPROIT_LINUX_GATEWAY:-black@zgx-5a09.local}"
LINUX_HOST="${REPROIT_LINUX_HOST:-strix}"
OUTPUT_DIR="${REPROIT_GATE_OUTPUT_DIR:-$ROOT/target/reproit-validation/linux-x86}"
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
usage: validation/release/run-linux-x86-remote.sh [options]

Options:
  --current-tree  Run the current tracked and non-ignored tree as diagnostic evidence.
  --gate ID       Run one registered Linux gate. Repeat to select several gates.
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
  GATES=(
    web-chromium
    web-engines
    electron
    tui-pty
    backend-contract
    tauri
    linux-atspi-gtk
    linux-atspi-toolkits
  )
fi
if ((${#GATES[@]} > 16)); then
  echo "at most 16 gates may be selected" >&2
  exit 2
fi
for gate in "${GATES[@]}"; do
  [[ "$gate" =~ ^[a-z0-9-]{1,64}$ ]] || {
    echo "invalid gate id: $gate" >&2
    exit 2
  }
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
RUN_ID="reproit-linux-$(date -u +%Y%m%dT%H%M%SZ)-$$"
REMOTE_BASE=".cache/reproit-linux-validation/$RUN_ID"
REMOTE_ARCHIVE="$REMOTE_BASE/source.tar.gz"
REMOTE_RUNNER="$REMOTE_BASE/run.sh"
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
    echo "Linux remote work retained: $GATEWAY -> $LINUX_HOST:$REMOTE_BASE" >&2
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

git -C "$ROOT" ls-files -co --exclude-standard -z >"$WORK/files"
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

cat >"$WORK/run.sh" <<'REMOTE'
#!/usr/bin/env bash
set -u -o pipefail

BASE="$1"
if [[ "$BASE" != /* ]]; then
  BASE="$HOME/$BASE"
fi
MODE="$2"
COMMIT="$3"
ARCHIVE_SHA256="$4"
GATE_CSV="$5"
KEEP_REMOTE="$6"
SOURCE="$BASE/source"
EVIDENCE="$BASE/evidence"
RESULT="$BASE/result.tar.gz"
IMAGE_PREFIX="${BASE##*/}"
IMAGE_PREFIX="${IMAGE_PREFIX,,}"
HOSTED=()
CONTAINER=()
OVERALL=0

cleanup() {
  docker rm -f "reproit-${BASHPID}-hosted" >/dev/null 2>&1 || true
  if [[ -n "${IMAGE:-}" ]]; then
    docker image rm -f "$IMAGE" >/dev/null 2>&1 || true
  fi
  docker image rm -f \
    "$IMAGE_PREFIX-tauri" \
    "$IMAGE_PREFIX-atspi" \
    "$IMAGE_PREFIX-qt-atspi" >/dev/null 2>&1 || true
  rm -rf "$SOURCE"
  if [[ "$KEEP_REMOTE" == 0 ]]; then
    rm -f "$BASE/source.tar.gz" "$BASE/run.sh"
  fi
}
trap cleanup EXIT INT TERM

mkdir -p "$SOURCE" "$EVIDENCE"
tar -xzf "$BASE/source.tar.gz" -C "$SOURCE"
ACTUAL_ARCHIVE_SHA256="$(sha256sum "$BASE/source.tar.gz" | awk '{print $1}')"
if [[ "$ACTUAL_ARCHIVE_SHA256" != "$ARCHIVE_SHA256" ]]; then
  echo "uploaded archive digest mismatch" >&2
  exit 2
fi
if [[ "$(git -C "$SOURCE" rev-parse HEAD)" != "$COMMIT" ]]; then
  echo "source commit mismatch" >&2
  exit 2
fi
if [[ "$MODE" == exact && -n "$(git -C "$SOURCE" status --porcelain=v1)" ]]; then
  echo "remote exact source is not clean" >&2
  exit 2
fi
if [[ "$(uname -m)" != x86_64 ]]; then
  echo "remote host is not native x86_64" >&2
  exit 2
fi
if [[ "$(docker info --format '{{.Architecture}}/{{.OSType}}')" != x86_64/linux ]]; then
  echo "remote Docker engine is not native x86_64 Linux" >&2
  exit 2
fi

IFS=, read -r -a GATES <<<"$GATE_CSV"
for gate in "${GATES[@]}"; do
  case "$gate" in
    web-chromium|web-engines|electron|tui-pty|backend-contract)
      HOSTED+=("$gate")
      ;;
    tauri|linux-atspi-gtk|linux-atspi-toolkits)
      CONTAINER+=("$gate")
      ;;
    *)
      echo "gate is not assigned to the Linux x86 lane: $gate" >&2
      exit 2
      ;;
  esac
done

python3 - "$EVIDENCE/run-metadata.json" "$MODE" "$COMMIT" \
  "$ARCHIVE_SHA256" "$GATE_CSV" <<'PY'
import datetime
import json
import platform
import socket
import subprocess
import sys

path, mode, commit, archive_sha256, gates = sys.argv[1:]
metadata = {
    "schema": 1,
    "route": "black@zgx-5a09.local -> strix",
    "host": socket.gethostname(),
    "hostOs": platform.system().lower(),
    "hostArchitecture": platform.machine().lower(),
    "docker": subprocess.check_output(
        ["docker", "info", "--format", "{{.Architecture}}/{{.OSType}}"],
        text=True,
        timeout=20,
    ).strip(),
    "sourceMode": mode,
    "baseCommit": commit,
    "sourceArchiveSha256": archive_sha256,
    "gates": gates.split(","),
    "startedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "processOwnership": "one remote directory and one named hosted container",
    "readiness": "archive digest, Git commit, host architecture, and Docker architecture",
    "cleanup": "gate traps, named container removal, and owned remote directory removal",
}
with open(path, "w", encoding="utf-8") as output:
    json.dump(metadata, output, indent=2)
    output.write("\n")
PY

if ((${#HOSTED[@]})); then
  cat >"$BASE/Dockerfile.hosted" <<'DOCKER'
FROM rust:1.88-bookworm AS rust-toolchain
FROM node:24-bookworm
COPY --from=rust-toolchain /usr/local/cargo /usr/local/cargo
COPY --from=rust-toolchain /usr/local/rustup /usr/local/rustup
ENV PATH=/usr/local/cargo/bin:/usr/local/bin:$PATH \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang curl jq libatspi2.0-dev libgtk-3-0 libnotify4 libxss1 \
    python3 util-linux xauth xdg-utils xvfb \
    && rm -rf /var/lib/apt/lists/*
DOCKER
  IMAGE="reproit-linux-hosted-$ARCHIVE_SHA256"
  docker build -t "$IMAGE" -f "$BASE/Dockerfile.hosted" "$BASE" || OVERALL=1
  if ((OVERALL == 0)); then
    HOSTED_CSV="$(IFS=,; echo "${HOSTED[*]}")"
    docker run --rm --name "reproit-${BASHPID}-hosted" \
      -e REPROIT_GATE_OUTPUT_DIR=/evidence \
      -e REPROIT_HOSTED_GATES="$HOSTED_CSV" \
      -v "$SOURCE:/repo:z" \
      -v "$EVIDENCE:/evidence:z" \
      -w /repo \
      "$IMAGE" bash -c '
        set -euo pipefail
        overall=0
        trap \
          "chown -R 1000:1000 /repo/target /repo/runners/web/node_modules /evidence \
          >/dev/null 2>&1 || true" EXIT
        git config --global --add safe.directory /repo
        engines=()
        if [[ ",$REPROIT_HOSTED_GATES," == *",web-chromium,"* ||
              ",$REPROIT_HOSTED_GATES," == *",electron,"* ]]; then
          engines+=(chromium)
        fi
        if [[ ",$REPROIT_HOSTED_GATES," == *",web-engines,"* ]]; then
          engines+=(firefox webkit)
        fi
        (
          cd runners/web
          npm ci
          if ((${#engines[@]})); then
            npx playwright install --with-deps "${engines[@]}"
          fi
        ) || exit 1
        IFS=, read -r -a gates <<<"$REPROIT_HOSTED_GATES"
        for gate in "${gates[@]}"; do
          if [[ "$gate" == electron ]]; then
            runuser -u node -- \
              xvfb-run -a python3 validation/backends/gate.py "$gate" || overall=1
          else
            python3 validation/backends/gate.py "$gate" || overall=1
          fi
        done
        exit "$overall"
      ' 2>&1 | tee "$EVIDENCE/hosted-worker.log" || OVERALL=1
  fi
fi

for gate in "${CONTAINER[@]}"; do
  (
    cd "$SOURCE"
    REPROIT_GATE_OUTPUT_DIR="$EVIDENCE" \
    REPROIT_TAURI_GATE_IMAGE="$IMAGE_PREFIX-tauri" \
    REPROIT_ATSPI_GATE_IMAGE="$IMAGE_PREFIX-atspi" \
    REPROIT_QT_ATSPI_GATE_IMAGE="$IMAGE_PREFIX-qt-atspi" \
    REPROIT_DOCKER_VOLUME_LABEL=",z" \
      python3 validation/backends/gate.py "$gate" --architecture x86_64
  ) || OVERALL=1
done

python3 - "$EVIDENCE" "$OVERALL" "$MODE" "$ARCHIVE_SHA256" <<'PY'
import datetime
import json
import sys
from pathlib import Path

directory, status, mode, archive_sha256 = sys.argv[1:]
directory = Path(directory)
path = directory / "run-metadata.json"
with path.open(encoding="utf-8") as source:
    metadata = json.load(source)
evidence_revision = metadata["baseCommit"]
if mode == "current-tree":
    evidence_revision = archive_sha256[:40]
    for result_path in directory.glob("*.json"):
        if result_path == path:
            continue
        with result_path.open(encoding="utf-8") as source:
            result = json.load(source)
        if result.get("gateId"):
            result["commit"] = evidence_revision
            with result_path.open("w", encoding="utf-8") as output:
                json.dump(result, output, indent=2)
                output.write("\n")
metadata["evidenceRevision"] = evidence_revision
metadata["finishedAt"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
metadata["outcome"] = "passed" if status == "0" else "failed"
with path.open("w", encoding="utf-8") as output:
    json.dump(metadata, output, indent=2)
    output.write("\n")
PY
tar -czf "$RESULT" -C "$EVIDENCE" .
exit "$OVERALL"
REMOTE

ssh "$GATEWAY" "ssh $LINUX_HOST 'cat > $REMOTE_RUNNER && chmod 700 $REMOTE_RUNNER'" \
  <"$WORK/run.sh"

set +e
remote "timeout 15000 $REMOTE_RUNNER $REMOTE_BASE $SOURCE_MODE $COMMIT \
$ARCHIVE_SHA256 $GATE_CSV $KEEP_REMOTE"
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
  echo "Linux x86 evidence collected: $OUTPUT_DIR"
else
  echo "remote Linux run returned no evidence archive" >&2
fi
exit "$REMOTE_STATUS"
