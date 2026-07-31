#!/usr/bin/env bash
# Backend finding-artifact portability acceptance (version 3): a finding
# discovered in one checkout replays from a COPY at a different absolute
# path with no environment surgery, because the artifact stores the schema
# project-relative and request URLs origin-relative with the discovering
# origin recorded once. A hand-built version-2 artifact still replays
# through the legacy absolute-URL path.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
CLI_E2E="$ROOT/validation/backend/cli-e2e"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-portability.XXXXXX")"
PORT=19897
SERVER_PID=""
cleanup() {
  if [[ -n "$SERVER_PID" ]]; then kill "$SERVER_PID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

boot_server() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  env "$@" PORT=$PORT node "$CLI_E2E/server.mjs" >/dev/null 2>&1 &
  SERVER_PID="$!"
  for _ in $(seq 1 40); do
    if curl -fsS "http://127.0.0.1:$PORT" >/dev/null 2>&1; then return; fi
    sleep 0.2
  done
  echo "fixture server did not boot" >&2
  exit 1
}

cli() {
  local dir="$1"
  shift
  (cd "$dir" && cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- "$@")
}

# Project A: backend-only, one GET the scan exercises; the server 500s on it.
mkdir -p "$WORK/a"
cat > "$WORK/a/openapi.yaml" << 'YAML'
openapi: 3.1.0
info:
  title: Reproit portability fixture
  version: 1.0.0
paths:
  /headless-message:
    get:
      operationId: getHeadlessMessage
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                required: [id]
                properties:
                  id: { type: string }
YAML
cat > "$WORK/a/reproit.yaml" << YAML
backend:
  enabled: true
  schemas: [openapi.yaml]
  target: http://127.0.0.1:$PORT
YAML

boot_server SERVER_ERROR=1
set +e
cli "$WORK/a" --json internal scan > "$WORK/scan.json" 2>&1
SCAN_STATUS="$?"
set -e
test "$SCAN_STATUS" -eq 1 || { echo "expected scan exit 1, got $SCAN_STATUS" >&2; cat "$WORK/scan.json" >&2; exit 1; }

FID="$(python3 - "$WORK/scan.json" << 'EOF'
import json, sys
report = json.load(open(sys.argv[1]))
print(report["findings"][0]["id"])
EOF
)"

ARTIFACT="$(ls "$WORK"/a/.reproit/findings/*/backend.json | head -1)"
python3 - "$ARTIFACT" "$PORT" << 'EOF'
import json, sys
artifact = json.load(open(sys.argv[1]))
assert artifact["version"] == 3, artifact["version"]
assert artifact["origin"] == "http://127.0.0.1:" + sys.argv[2], artifact["origin"]
assert artifact["failing"]["request"]["url"].startswith("/"), artifact["failing"]["request"]["url"]
assert artifact["schema"] == "openapi.yaml", artifact["schema"]
print("artifact v3 shape holds")
EOF

# The whole project moves to a different absolute path; nothing else changes.
cp -R "$WORK/a" "$WORK/b"
OUT="$(cli "$WORK/b" "$FID" 2>&1 || true)"
grep -q "reproduced exactly" <<< "$OUT" || { echo "v3 replay from the copy did not reproduce: $OUT" >&2; exit 1; }
echo "PASS v3 artifact replays from a moved checkout (reproduced)"

# Legacy: the same finding hand-lowered to version 2 (absolute URLs, no
# origin) replays through the old retarget path with REPROIT_BACKEND_URL.
cp -R "$WORK/a" "$WORK/c"
python3 - "$WORK/c" "$PORT" << 'EOF'
import glob, json, sys
path = glob.glob(sys.argv[1] + "/.reproit/findings/*/backend.json")[0]
artifact = json.load(open(path))
base = "http://127.0.0.1:" + sys.argv[2]
artifact["version"] = 2
artifact.pop("origin", None)
for step in artifact.get("setup", []):
    if step["request"]["url"].startswith("/"):
        step["request"]["url"] = base + step["request"]["url"]
if artifact["failing"]["request"]["url"].startswith("/"):
    artifact["failing"]["request"]["url"] = base + artifact["failing"]["request"]["url"]
json.dump(artifact, open(path, "w"))
EOF
OUT="$( (cd "$WORK/c" && REPROIT_BACKEND_URL="http://127.0.0.1:$PORT" \
  cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- "$FID") 2>&1 || true)"
grep -q "reproduced exactly" <<< "$OUT" || { echo "v2 legacy replay failed: $OUT" >&2; exit 1; }
echo "PASS v2 artifact still replays through the legacy path"

# Fix the service; the moved-checkout artifact proves the fix.
boot_server VALID_RESPONSE=1
OUT="$(cli "$WORK/b" "$FID" 2>&1 || true)"
grep -qi "fixed\|no longer reproduces\|held" <<< "$OUT" || { echo "v3 replay did not certify the fix: $OUT" >&2; exit 1; }
echo "PASS v3 artifact certifies the fix from the moved checkout"

# The SCHEMA artifact (backend-schema.json) gets the same treatment at
# version 2: a schema-level violation is recorded with a project-relative
# path, so it too replays from a moved checkout. A duplicate parameter is a
# static defect in the schema itself, which is why it needs no live target.
mkdir -p "$WORK/s"
cat > "$WORK/s/openapi.yaml" << 'YAML'
openapi: 3.1.0
info:
  title: Reproit schema portability fixture
  version: 1.0.0
paths:
  /items:
    get:
      operationId: listItems
      parameters:
        - { name: limit, in: query, schema: { type: integer } }
        - { name: limit, in: query, schema: { type: integer } }
      responses:
        "200":
          content:
            application/json:
              schema: { type: object }
YAML
cat > "$WORK/s/reproit.yaml" << YAML
backend:
  enabled: true
  schemas: [openapi.yaml]
  target: http://127.0.0.1:$PORT
YAML
set +e
cli "$WORK/s" --json internal scan > "$WORK/schema-scan.json" 2>&1
set -e
SCHEMA_ARTIFACT="$(ls "$WORK"/s/.reproit/findings/*/backend-schema.json 2>/dev/null | head -1)"
if [[ -z "$SCHEMA_ARTIFACT" ]]; then
  echo "expected a backend-schema.json finding for the duplicate parameter" >&2
  cat "$WORK/schema-scan.json" >&2
  exit 1
fi
python3 - "$SCHEMA_ARTIFACT" << 'EOF'
import json, sys
artifact = json.load(open(sys.argv[1]))
assert artifact["version"] == 2, artifact["version"]
assert artifact["schema"] == "openapi.yaml", artifact["schema"]
assert not artifact.get("schemaOutsideRoot", False), artifact
print("schema artifact v2 shape holds (project-relative path)")
EOF
SFID="$(python3 -c "
import json,sys
print(json.load(open('$SCHEMA_ARTIFACT'))['finding']['id'])
")"
# Move the whole project; the schema artifact must resolve its schema against
# the NEW root, which an absolute stored path could never do.
cp -R "$WORK/s" "$WORK/s-moved"
OUT="$(cli "$WORK/s-moved" "$SFID" 2>&1 || true)"
grep -q "reproduced exactly" <<< "$OUT" \
  || { echo "v2 schema artifact did not replay from the moved checkout: $OUT" >&2; exit 1; }
echo "PASS v2 schema artifact replays from a moved checkout"

# Legacy: the same artifact hand-lowered to version 1 with the absolute path
# it used to store still replays, so old findings keep working.
cp -R "$WORK/s" "$WORK/s-legacy"
python3 - "$WORK/s-legacy" << 'EOF'
import glob, json, os, sys
path = glob.glob(sys.argv[1] + "/.reproit/findings/*/backend-schema.json")[0]
artifact = json.load(open(path))
artifact["version"] = 1
artifact.pop("schemaOutsideRoot", None)
artifact["schema"] = os.path.join(sys.argv[1], "openapi.yaml")
json.dump(artifact, open(path, "w"))
EOF
OUT="$(cli "$WORK/s-legacy" "$SFID" 2>&1 || true)"
grep -q "reproduced exactly" <<< "$OUT" \
  || { echo "v1 schema artifact legacy path failed: $OUT" >&2; exit 1; }
echo "PASS v1 schema artifact still replays through the legacy absolute path"

echo "artifact-portability-e2e: both artifacts are portable, both legacy versions still replay"
