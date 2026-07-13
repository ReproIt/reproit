#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
pin="${A2UI_EXPECTED_COMMIT:-96abfdc60de0657c6322028d10c1cc7bc25c237c}"
owned=
if [[ -n "${A2UI_CHECKOUT:-}" ]]; then
  checkout="$A2UI_CHECKOUT"
else
  checkout="$(mktemp -d "${TMPDIR:-/tmp}/reproit-a2ui.XXXXXX")"
  owned=1
  git clone --filter=blob:none https://github.com/google/A2UI.git "$checkout"
  git -C "$checkout" checkout --detach "$pin"
fi

cleanup() {
  if [[ -n "${react_pid:-}" ]]; then kill "$react_pid" 2>/dev/null || true; fi
  if [[ -n "${lit_pid:-}" ]]; then kill "$lit_pid" 2>/dev/null || true; fi
  if [[ -n "$owned" ]]; then rm -rf "$checkout"; fi
  return 0
}
trap cleanup EXIT

actual="$(git -C "$checkout" rev-parse HEAD)"
[[ "$actual" == "$pin" ]] || { echo "A2UI checkout must be pinned to $pin (got $actual)" >&2; exit 1; }

(cd "$checkout" && corepack yarn install --immutable)
(cd "$checkout" && corepack yarn workspace @a2ui/react-explorer build)
(cd "$checkout" && corepack yarn workspace @a2ui/lit-explorer build)

(cd "$checkout/renderers/react/a2ui_explorer" && corepack yarn vite --host 127.0.0.1 --port 4311) >"${TMPDIR:-/tmp}/reproit-a2ui-react.log" 2>&1 &
react_pid=$!
(cd "$checkout/renderers/lit/a2ui_explorer" && corepack yarn vite --host 127.0.0.1 --port 4312) >"${TMPDIR:-/tmp}/reproit-a2ui-lit.log" 2>&1 &
lit_pid=$!

for url in http://127.0.0.1:4311 http://127.0.0.1:4312; do
  ready=
  for _ in {1..60}; do
    if curl --fail --silent --output /dev/null "$url"; then ready=1; break; fi
    sleep 0.25
  done
  [[ -n "$ready" ]] || { echo "renderer did not start: $url" >&2; exit 1; }
done

harness="${A2UI_HARNESS:-$root/runners/a2ui/live-renderer-harness.mjs}"
node "$harness" "$checkout" http://127.0.0.1:4311 http://127.0.0.1:4312
