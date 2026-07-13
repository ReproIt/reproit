#!/usr/bin/env bash
# Release gate for the product promise: a production SDK event becomes a real,
# locally executable repro through the public CLI. Requires a disposable cloud
# project/key; it never sends user data and uses a unique contract-only crash.
set -euo pipefail

BASE="${REPROIT_CLOUD_URL:?set REPROIT_CLOUD_URL}"
KEY="${REPROIT_CLOUD_KEY:?set REPROIT_CLOUD_KEY}"
APP="${REPROIT_CLOUD_APP:?set REPROIT_CLOUD_APP}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${REPROIT_BIN:-$ROOT/target/debug/reproit}"
WORK="$(mktemp -d)"
PORT="${REPROIT_CONTRACT_PORT:-18779}"
cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then kill "$SERVER_PID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

if [[ ! -x "$BIN" ]]; then
  cargo build -p reproit --manifest-path "$ROOT/Cargo.toml"
fi

mkdir -p "$WORK/home" "$WORK/app"
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
evidence:
  outDir: .reproit/runs
YAML

python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$WORK/app" \
  > "$WORK/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 50); do
  curl -fsS "http://127.0.0.1:$PORT/" >/dev/null && break
  sleep 0.1
done

export HOME="$WORK/home"
cd "$WORK"
"$BIN" cloud setup --app "$APP" --key "$KEY" --cloud "$BASE" --no-workflow

# This is the exact PII-safe SDK wire shape. It describes the real action path;
# the local fixture independently proves that path still triggers the same crash.
curl -fsS -X POST "$BASE/v1/events" \
  -H "authorization: Bearer $KEY" \
  -H 'content-type: application/json' \
  -d '{
    "appId":"'"$APP"'",
    "ctx":{"build":{"version":"contract-gate"},"platform":"web"},
    "events":[{
      "kind":"error","oracle":"crash",
      "sig":"crash:ReproitContractError:contract-gate",
      "message":"TypeError: ReproitContractError",
      "path":[
        {"sig":"home","action":"load"},
        {"sig":"home","action":"tap:key:testid:contract-crash"}
      ]
    }]
  }' > "$WORK/ingest.json"

"$BIN" --json bugs contract-gate > "$WORK/bugs.json"
BUCKET="$(python3 - "$WORK/bugs.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
items=d.get("items", d.get("buckets", []))
print(next((x.get("bucketId", "") for x in items if "contract" in str(x).lower()), ""))
PY
)"
[[ "$BUCKET" == bkt_* ]] || { echo "contract bucket not found" >&2; cat "$WORK/bugs.json"; exit 1; }

"$BIN" pull "$BUCKET" --as cloud-contract --no-run
"$BIN" check cloud-contract

echo "production loop passed: SDK event -> bucket -> pull -> real local reproduction"
