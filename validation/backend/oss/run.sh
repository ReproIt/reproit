#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
WORK="$(mktemp -d -t reproit-backend-oss)"
export REPROIT_OSS_TMP="$WORK/captured"
mkdir -p "$REPROIT_OSS_TMP"
PIDS=()
cleanup() {
  for pid in ${PIDS[@]+"${PIDS[@]}"}; do kill "$pid" 2>/dev/null || true; done
  for pid in ${PIDS[@]+"${PIDS[@]}"}; do wait "$pid" 2>/dev/null || true; done
  docker rm -f reproit-backend-oss-petstore >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

wait_http() {
  local url="$1"
  for _ in $(seq 1 90); do
    if curl -fsS "$url" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  echo "timed out waiting for $url" >&2
  return 1
}

PETSTORE_IMAGE='swaggerapi/petstore3@sha256:'
PETSTORE_IMAGE+='7013040e865d642be5ddafd113116710acdb16addb0c4c59d29f0a6d68d2aa93'
docker run --rm -d --platform linux/amd64 --name reproit-backend-oss-petstore \
  -p 18080:8080 "$PETSTORE_IMAGE" >/dev/null
wait_http http://127.0.0.1:18080/api/v3/openapi.json
curl -fsS http://127.0.0.1:18080/api/v3/openapi.json -o "$REPROIT_OSS_TMP/petstore-openapi.json"
curl -fsS -H 'content-type: application/json' \
  -d '{"id":987654321,"name":"Reproit dogfood","photoUrls":[]}' \
  http://127.0.0.1:18080/api/v3/pet -o "$REPROIT_OSS_TMP/petstore-add.json"
curl -fsS -H 'content-type: application/x-www-form-urlencoded' \
  --data 'id=987654322&name=FormDog&photoUrls=https%3A%2F%2Fexample.test%2Fdog.png' \
  http://127.0.0.1:18080/api/v3/pet -o "$REPROIT_OSS_TMP/petstore-form.json"
curl -fsS http://127.0.0.1:18080/api/v3/pet/987654321 -o "$REPROIT_OSS_TMP/petstore-get.json"
curl -fsS 'http://127.0.0.1:18080/api/v3/pet/findByStatus?status=available' \
  -o "$REPROIT_OSS_TMP/petstore-list.json"
curl -fsS http://127.0.0.1:18080/api/v3/store/inventory \
  -o "$REPROIT_OSS_TMP/petstore-inventory.json"
curl -fsS -D "$REPROIT_OSS_TMP/petstore-xml.headers" -H 'accept: application/xml' \
  http://127.0.0.1:18080/api/v3/pet/987654321 -o "$REPROIT_OSS_TMP/petstore-get.xml"
grep -qi '^content-type: application/xml' "$REPROIT_OSS_TMP/petstore-xml.headers"
curl -fsS -X DELETE -H 'api_key: dogfood' \
  http://127.0.0.1:18080/api/v3/pet/987654321 >/dev/null
curl -fsS -X DELETE -H 'api_key: dogfood' \
  http://127.0.0.1:18080/api/v3/pet/987654322 >/dev/null

COUNTRIES="$WORK/countries"
git clone -q https://github.com/trevorblades/countries.git "$COUNTRIES"
git -C "$COUNTRIES" checkout -q 5a150acb0ef9fc0f220db3f154896f1a5c37c405
npm install --ignore-scripts --no-audit --no-fund --prefix "$COUNTRIES" >/dev/null
(cd "$COUNTRIES" && npm run dev -- --port 18787 >"$WORK/countries.log" 2>&1) &
PIDS+=("$!")
for _ in $(seq 1 90); do
  if curl -fsS http://127.0.0.1:18787/graphql -H 'content-type: application/json' \
    --data-binary '{"query":"{ __typename }"}' >/dev/null 2>&1; then break; fi
  sleep 1
done

INTROSPECTION='query IntrospectionQuery { __schema { queryType { name } '
INTROSPECTION+='mutationType { name } subscriptionType { name } types { kind name '
INTROSPECTION+='fields(includeDeprecated: true) { name args { name type { kind name '
INTROSPECTION+='ofType { kind name ofType { kind name ofType { kind name } } } } } '
INTROSPECTION+='type { kind name ofType { kind name ofType { kind name '
INTROSPECTION+='ofType { kind name } } } } } inputFields { name type { kind name '
INTROSPECTION+='ofType { kind name ofType { kind name } } } } '
INTROSPECTION+='enumValues(includeDeprecated: true) { name } possibleTypes { name } } } }'
post_graphql() {
  local url="$1" query="$2" output="$3" variables='{}'
  if [[ $# -ge 4 ]]; then variables="$4"; fi
  jq -cn --arg query "$query" --argjson variables "$variables" \
    '{query:$query,variables:$variables}' |
    curl -fsS "$url" -H 'content-type: application/json' --data-binary @- -o "$output"
}
post_graphql http://127.0.0.1:18787/graphql "$INTROSPECTION" \
  "$REPROIT_OSS_TMP/countries-introspection.json"
alias_query='query Aliased($code: ID!) { nation: country(code: $code) { '
alias_query+='...CountryBits } } fragment CountryBits on Country { code name '
alias_query+='languages { code name } }'
post_graphql http://127.0.0.1:18787/graphql \
  "$alias_query" \
  "$WORK/countries-alias.json" '{"code":"US"}'
jq '.data.nation' "$WORK/countries-alias.json" > "$REPROIT_OSS_TMP/countries-alias-output.json"
post_graphql http://127.0.0.1:18787/graphql \
  'query { countries(filter:{code:{in:["US","CA","MX"]}}) { code name } }' \
  "$WORK/countries-list.json"
jq '.data.countries' "$WORK/countries-list.json" > "$REPROIT_OSS_TMP/countries-list-output.json"
post_graphql http://127.0.0.1:18787/graphql \
  'query { country(code:"ZZ") { code name } }' "$WORK/countries-null.json"
jq -e '.data.country == null' "$WORK/countries-null.json" >/dev/null
post_graphql http://127.0.0.1:18787/graphql \
  'query { country(code:"US") { code name(lang:"zz-not-real") } }' "$WORK/countries-error.json"
jq -e '.data.country == null and (.errors|length == 1)' "$WORK/countries-error.json" >/dev/null

GRAPHQL_PACKAGE_JSON="$COUNTRIES/package.json" PORT=18788 \
  node "$ROOT/validation/backend/oss/graphql-service.mjs" >"$WORK/graphql-shapes.log" 2>&1 &
PIDS+=("$!")
for _ in $(seq 1 30); do
  if curl -fsS http://127.0.0.1:18788/graphql -H 'content-type: application/json' \
    --data-binary '{"query":"{ __typename }"}' >/dev/null 2>&1; then break; fi
  sleep 1
done
post_graphql http://127.0.0.1:18788/graphql "$INTROSPECTION" \
  "$REPROIT_OSS_TMP/graphql-shapes-introspection.json"
shape_query='query($kind:String!){ subject:node(kind:$kind){ __typename '
shape_query+='... on User { id name nickname } ... on Service { id endpoint } } }'
post_graphql http://127.0.0.1:18788/graphql \
  "$shape_query" \
  "$WORK/graphql-interface.json" '{"kind":"user"}'
jq '.data.subject' "$WORK/graphql-interface.json" > "$REPROIT_OSS_TMP/graphql-interface-output.json"
search_query='query { search { __typename ... on User { id name nickname } '
search_query+='... on Service { id endpoint } } }'
post_graphql http://127.0.0.1:18788/graphql \
  "$search_query" \
  "$WORK/graphql-union.json"
jq '.data.search' "$WORK/graphql-union.json" > "$REPROIT_OSS_TMP/graphql-union-output.json"
post_graphql http://127.0.0.1:18788/graphql \
  'query { nullableNode { __typename } }' "$WORK/graphql-null.json"
jq -e '.data.nullableNode == null' "$WORK/graphql-null.json" >/dev/null
post_graphql http://127.0.0.1:18788/graphql 'query { explode }' "$WORK/graphql-error.json"
jq -e '.data.explode == null and (.errors|length == 1)' "$WORK/graphql-error.json" >/dev/null
REPROIT_BACKEND_URL=http://127.0.0.1:18788/graphql \
  cargo run --quiet -p reproit -- --json scan \
  "$REPROIT_OSS_TMP/graphql-shapes-introspection.json" >"$WORK/graphql-headless-scan.json"
jq -e '.complete == true and .exercised == 4 and (.findings | length) == 0' \
  "$WORK/graphql-headless-scan.json" >/dev/null
echo "CLEAN graphql-shapes public headless scan operations=4"

GRPC="$WORK/grpc-go"
git clone -q https://github.com/grpc/grpc-go.git "$GRPC"
git -C "$GRPC" checkout -q 2a112a82f5c53ab3b89b5aa4a02b4195e2706879
(cd "$GRPC/examples" && go run ./helloworld/greeter_server >"$WORK/grpc-server.log" 2>&1) &
PIDS+=("$!")
for _ in $(seq 1 90); do
  if grep -q 'server listening' "$WORK/grpc-server.log" 2>/dev/null; then break; fi
  sleep 1
done
(cd "$GRPC/examples" && go run "$ROOT/validation/backend/oss/grpc-dogfood.go")
REPROIT_BACKEND_URL=http://127.0.0.1:50051 \
  cargo run --quiet -p reproit -- --json fuzz \
  "$GRPC/examples/helloworld/helloworld/helloworld.proto" --runs 1 \
  >"$WORK/grpc-headless-fuzz.json"
jq -e '.complete == true and .exercised == 1 and (.findings | length) == 0' \
  "$WORK/grpc-headless-fuzz.json" >/dev/null
echo "CLEAN grpc-go public headless fuzz operations=1"
cp "$ROOT/validation/backend/oss/grpc-int64-descriptor.json" \
  "$REPROIT_OSS_TMP/grpc-int64-descriptor.json"

cargo run --quiet --manifest-path "$ROOT/validation/backend/oss/Cargo.toml"
echo "OSS backend contract gate passed"
