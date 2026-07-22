#!/usr/bin/env bash
# Production release gate: create a disposable Cloud project, ingest SDK-shaped
# occurrences, prove raw values never enter the batch, measure the hosted path, reproduce
# the bucket locally, then permanently delete the project.
set -euo pipefail

BASE="${REPROIT_CLOUD_URL:-https://cloud.reproit.com}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${REPROIT_BIN:-$ROOT/target/debug/reproit}"
WORK="$(mktemp -d)"
PORT="${REPROIT_CONTRACT_PORT:-18779}"
PROJECT_NAME="${REPROIT_CONTRACT_PROJECT_NAME:-Release Gate $(date -u +%Y%m%d-%H%M%S)}"
RESULTS_OUT="${REPROIT_CONTRACT_RESULTS:-$WORK/production-results.json}"
ACCOUNT_KEY="${REPROIT_CLOUD_ACCOUNT_KEY:-}"
APP=""
PROJECT_KEY=""
PUBLISHABLE_KEY=""
SERVER_PID=""
DELETED=false

if [[ -z "$ACCOUNT_KEY" && -f "$HOME/.reproit/token" ]]; then
  ACCOUNT_KEY="$(python3 - "$HOME/.reproit/token" <<'PY'
import json, sys
print(json.load(open(sys.argv[1])).get("token", ""))
PY
)"
fi
[[ "$ACCOUNT_KEY" == sk_live_* ]] || {
  echo "set REPROIT_CLOUD_ACCOUNT_KEY or run reproit login first" >&2
  exit 1
}

delete_project() {
  [[ -n "$APP" ]] || return 0
  [[ "${REPROIT_KEEP_CONTRACT_PROJECT:-}" == "1" ]] && return 0
  local code
  code="$(curl -sS -o "$WORK/delete.json" -w '%{http_code}' \
    -X DELETE "$BASE/v1/projects/$APP" \
    -H "authorization: Bearer $ACCOUNT_KEY" \
    -H 'content-type: application/json' \
    --data-binary "$(python3 - "$PROJECT_NAME" <<'PY'
import json, sys
print(json.dumps({"confirm": sys.argv[1]}))
PY
)")"
  if [[ "$code" == "200" ]]; then
    DELETED=true
  else
    echo "warning: disposable project cleanup failed (HTTP $code)" >&2
    cat "$WORK/delete.json" >&2
  fi
}

cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ "$DELETED" != true ]]; then delete_project; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

if [[ ! -x "$BIN" ]]; then
  cargo build -p reproit --manifest-path "$ROOT/Cargo.toml"
fi

CREATE_CODE="$(curl -sS -o "$WORK/project.json" -w '%{http_code}' \
  -X POST "$BASE/v1/projects" \
  -H "authorization: Bearer $ACCOUNT_KEY" \
  -H 'content-type: application/json' \
  --data-binary "$(python3 - "$PROJECT_NAME" <<'PY'
import json, sys
print(json.dumps({"name": sys.argv[1]}))
PY
)")"
[[ "$CREATE_CODE" == "201" ]] || {
  echo "could not create disposable project (HTTP $CREATE_CODE)" >&2
  cat "$WORK/project.json" >&2
  exit 1
}
read -r APP PROJECT_KEY PUBLISHABLE_KEY < <(
  python3 - "$WORK/project.json" <<'PY'
import json, sys
d=json.load(open(sys.argv[1]))
print(d["appId"], d["apiKey"], d["publishableKey"])
PY
)

mkdir -p "$WORK/app"
cat > "$WORK/app/index.html" <<'HTML'
<!doctype html><html><body>
<button data-testid="contract-crash">Crash</button>
<script>
document.querySelector('[data-testid="contract-crash"]').addEventListener('click', () => {
  throw new TypeError('ReproitContractError');
});
</script>
</body></html>
HTML
cat > "$WORK/reproit.yaml" <<YAML
app:
  platform: web
  webRunnerDir: $ROOT/runners/web
  url: http://127.0.0.1:$PORT
devices:
  namePrefix: ReleaseGate
journeys:
  driver: ""
  doneMarkers: ["Release gate complete"]
evidence:
  outDir: .reproit/runs
YAML

python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$WORK/app" \
  > "$WORK/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 50); do
  curl -fsS "http://127.0.0.1:$PORT/" >/dev/null 2>&1 && break
  sleep 0.1
done

export REPROIT_CLOUD_URL="$BASE"
export REPROIT_CLOUD_KEY="$ACCOUNT_KEY"
export REPROIT_CLOUD_APP="$APP"
export REPROIT_BENCH_PROJECT_KEY="$PROJECT_KEY"
export REPROIT_BENCH_PUBLISHABLE_KEY="$PUBLISHABLE_KEY"
export REPROIT_BENCH_APP="$APP"
export REPROIT_BENCH_BASE="$BASE"
export REPROIT_BENCH_OUT="$WORK/hosted.json"
python3 "$ROOT/validation/cloud/production-benchmark.py"

BUCKET="$(python3 - "$WORK/hosted.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))["bucketId"])
PY
)"
[[ "$BUCKET" == bkt_* ]] || { echo "contract bucket not found" >&2; exit 1; }

cd "$WORK"
REPLAY_START="$(python3 -c 'import time; print(time.monotonic_ns())')"
"$BIN" "$BUCKET" --no-run > "$WORK/pull.log"
set +e
"$BIN" check --repro-id "$BUCKET" --json > "$WORK/check.json" 2> "$WORK/check.err"
CHECK_EXIT=$?
set -e
REPLAY_END="$(python3 -c 'import time; print(time.monotonic_ns())')"
REPLAY_MS="$(( (REPLAY_END - REPLAY_START) / 1000000 ))"
[[ "$REPLAY_MS" -lt 60000 ]] || {
  echo "production replay took ${REPLAY_MS}ms, above the 60s release ceiling" >&2
  exit 1
}

[[ "$CHECK_EXIT" -eq 1 ]] || {
  echo "expected reproduced regression exit 1, got $CHECK_EXIT" >&2
  cat "$WORK/check.err" >&2
  cat "$WORK/check.json" >&2
  exit 1
}
[[ -s "$WORK/check.json" ]] || {
  echo "production replay returned no JSON verdict" >&2
  cat "$WORK/check.err" >&2
  while IFS= read -r log; do
    echo "replay driver log: $log" >&2
    cat "$log" >&2
  done < <(find "$WORK/.reproit/runs" -name 'drive-*.log' -type f 2>/dev/null)
  exit 1
}
python3 - "$WORK/check.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
if d.get("outcome") != "fail":
    raise SystemExit(f"expected outcome=fail, got {d!r}")
PY

DIRECT_START="$(python3 -c 'import time; print(time.monotonic_ns())')"
"$BIN" "$BUCKET" > "$WORK/direct-replay.log"
DIRECT_END="$(python3 -c 'import time; print(time.monotonic_ns())')"
DIRECT_MS="$(( (DIRECT_END - DIRECT_START) / 1000000 ))"
grep -q 'REPRODUCED:' "$WORK/direct-replay.log" || {
  echo "direct bucket command did not confirm the production failure" >&2
  cat "$WORK/direct-replay.log" >&2
  exit 1
}
[[ "$DIRECT_MS" -lt 60000 ]] || {
  echo "direct production replay took ${DIRECT_MS}ms, above the 60s release ceiling" >&2
  exit 1
}

delete_project
[[ "$DELETED" == true || "${REPROIT_KEEP_CONTRACT_PROJECT:-}" == "1" ]] || exit 1
mkdir -p "$(dirname "$RESULTS_OUT")"
python3 - "$WORK/hosted.json" "$RESULTS_OUT" "$REPLAY_MS" "$DIRECT_MS" "$DELETED" <<'PY'
import datetime, json, sys
source, destination, replay_ms, direct_ms, deleted = sys.argv[1:]
d = json.load(open(source))
d.update({
    "directBucketCommandMs": int(direct_ms),
    "measuredAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "replayMs": int(replay_ms),
    "projectDeleted": deleted == "true",
})
with open(destination, "w") as f:
    json.dump(d, f, indent=2, sort_keys=True)
    f.write("\n")
print(json.dumps(d, indent=2, sort_keys=True))
PY

echo "production gate passed: disposable project -> strict ingest -> redaction markers -> "\
"bkt replay -> deletion"
