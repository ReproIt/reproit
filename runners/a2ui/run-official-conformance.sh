#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
checkout="${1:-${A2UI_CHECKOUT:-}}"
[[ -n "$checkout" ]] || { echo "usage: $0 /path/to/a2ui-checkout" >&2; exit 2; }
checkout="$(cd "$checkout" && pwd)"
actual="$(git -C "$checkout" rev-parse HEAD)"
expected="${A2UI_EXPECTED_COMMIT:-$actual}"
[[ "$actual" == "$expected" ]] || {
  echo "A2UI checkout must be at $expected (got $actual)" >&2
  exit 1
}

artifacts="${A2UI_ARTIFACT_DIR:-${TMPDIR:-/tmp}/a2ui-conformance-$actual}"
mkdir -p "$artifacts/fixtures"

A2UI_EXPECTED_COMMIT="$expected" node "$root/runners/a2ui/generate-official-fixtures.mjs" \
  "$checkout" "$artifacts/fixtures" > "$artifacts/fixtures-report.json"

renderer_status=0
A2UI_CHECKOUT="$checkout" \
A2UI_EXPECTED_COMMIT="$expected" \
A2UI_HARNESS="$root/runners/a2ui/official-fixture-renderer-harness.mjs" \
A2UI_REPORT="$artifacts/renderer-report.json" \
  "$root/runners/a2ui/run-official-live.sh" > "$artifacts/renderer-stdout.log" || renderer_status=$?

A2UI_CHECKOUT="$checkout" \
A2UI_EXPECTED_COMMIT="$expected" \
A2UI_HARNESS="$root/runners/a2ui/upstream-issue-harness.mjs" \
A2UI_REPORT="$artifacts/issue-report.json" \
  "$root/runners/a2ui/run-official-live.sh" > "$artifacts/issue-stdout.log"

policy_status=0
node "$root/runners/a2ui/conformance-policy.mjs" \
  "$artifacts/renderer-report.json" \
  "$artifacts/issue-report.json" \
  "$artifacts/conformance-report.json" || policy_status=$?

if [[ "$renderer_status" -ne 0 && "$policy_status" -eq 0 ]]; then
  echo "renderer harness failed without a policy finding" >&2
  exit "$renderer_status"
fi
if [[ "$policy_status" -ne 0 ]]; then
  echo "A2UI conformance found unexpected regressions" >&2
  echo "  commit:    $actual" >&2
  echo "  artifacts: $artifacts" >&2
  exit "$policy_status"
fi

echo "A2UI conformance passed"
echo "  commit:    $actual"
echo "  artifacts: $artifacts"
