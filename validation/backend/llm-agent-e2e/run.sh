#!/usr/bin/env bash
# LLM/agent capsule acceptance: capture the planted agent bug (a streamed
# model response directs the wrong, destructive tool; the guardrail oracle
# fires) on the owned fixture, then re-execute it with `check --exec` while
# the model API and the tool service do not exist. Asserts the four-way
# verdict contract: reproduced (1), fixed (0), reproduced again (1), and
# diverged (3) when a recorded model response body is tampered, with the
# prompt-drift report naming the first differing message index.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE="$ROOT/examples/llm-agent-fixture"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reproit-llm-agent.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

MODE=capture CAPTURE_OUT="$WORK/capture.json" node "$FIXTURE/agent.mjs" >/dev/null
test -s "$WORK/capture.json"

# The capture must carry the marked agent oracle and the stream shape, or the
# replay legs below would be exercising a weaker capsule than claimed.
python3 - "$WORK/capture.json" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
assert capture['oracle'] == 'agent-guardrail-violation', capture['oracle']
streamed = [
    event for event in capture['events']
    if (event.get('exchange') or {}).get('response', {}).get('stream')
]
assert streamed, 'no exchange recorded stream chunk boundaries'
chunks = streamed[0]['exchange']['response']['stream']['chunks']
assert len(chunks) > 1, chunks
EOF

# Case accounting, as in hermetic-e2e: reaching the end is not the pass
# condition; the count of completed cases matching intent is.
CASES_RUN=0
EXPECTED_CASES=4

run_case() {
  local capture="$1" command="$2" expected="$3" label="$4" marker="$5"
  local status=0
  cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p reproit -- \
    check "$capture" --exec "$command" >"$WORK/out.txt" 2>&1 || status="$?"
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL $label: expected exit $expected, got $status" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  if ! grep -q "$marker" "$WORK/out.txt"; then
    echo "FAIL $label: output lacks the verdict marker '$marker'" >&2
    cat "$WORK/out.txt" >&2
    exit 1
  fi
  CASES_RUN=$((CASES_RUN + 1))
  echo "PASS $label (exit $status)"
}

run_case "$WORK/capture.json" "node $FIXTURE/agent.mjs" 1 \
  "agent bug reproduces hermetically (model API unreachable)" \
  "reproduced by re-execution (agent-guardrail-violation on POST /assist)"
run_case "$WORK/capture.json" "FIXED=1 node $FIXTURE/agent.mjs" 0 \
  "tool-allowlist fix certifies" \
  "the operation now answers cleanly"
run_case "$WORK/capture.json" "node $FIXTURE/agent.mjs" 1 \
  "revert reproduces again" \
  "reproduced by re-execution"

# Tamper a recorded MODEL RESPONSE body: rewrite one word of the streamed
# assistant text. The replayed agent then builds a different second prompt,
# and the divergence must name the first differing message index (1: the
# assistant message), not just the call site.
python3 - "$WORK/capture.json" "$WORK/tampered.json" << 'EOF'
import json, sys
capture = json.load(open(sys.argv[1]))
tampered = 0
for event in capture['events']:
    exchange = event.get('exchange') or {}
    response = exchange.get('response') or {}
    body = response.get('body')
    if isinstance(body, str) and 'Handling' in body:
        response['body'] = body.replace('Handling', 'Deleting')
        tampered += 1
assert tampered == 1, tampered
json.dump(capture, open(sys.argv[2], 'w'))
EOF
run_case "$WORK/tampered.json" "node $FIXTURE/agent.mjs" 3 \
  "tampered model response diverges naming the message index" \
  "DIVERGED"
if ! grep -q '"firstDifferingMessage":1' "$WORK/out.txt"; then
  echo "FAIL prompt drift: divergence does not name message index 1" >&2
  cat "$WORK/out.txt" >&2
  exit 1
fi

if [[ "$CASES_RUN" -ne "$EXPECTED_CASES" ]]; then
  echo "FAIL harness accounting: $CASES_RUN of $EXPECTED_CASES cases ran" >&2
  exit 1
fi
echo "llm-agent-e2e: all four verdicts hold ($CASES_RUN/$EXPECTED_CASES cases)"
