#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$root"

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing required validator: $1" >&2; exit 1; }; }
need cargo
need node
need npm
need go
need flutter
need swift
need cc
need dotnet
need pod
need xcrun
if command -v pytest >/dev/null 2>&1; then
  pytest_run() { pytest "$@"; }
elif command -v uv >/dev/null 2>&1; then
  pytest_run() { uvx pytest "$@"; }
else
  echo "missing required validator: pytest (or uv)" >&2
  exit 1
fi

cargo test -p reproit
node --test runners/web/capsule.test.mjs runners/electron-capsule.test.mjs
node --test sdk/reproit-tauri/test/init.test.mjs
cargo check --manifest-path sdk/reproit-tauri/Cargo.toml

(cd sdk/reproit-react-native && npm test -- --runInBand && npm run typecheck && npm run build && pod ipc spec reproit-react-native.podspec >/dev/null)
(cd sdk/reproit-linux && pytest_run -q)
(cd sdk/reproit_flutter && flutter test test/causal_test.dart)
(cd sdk/reproit-ios && swift test)
sdk/reproit-android/run_host_test.sh
(cd sdk/reproit-tui-go && go test ./...)
(cd sdk/reproit-tui-ts && npm run test:all && npm run typecheck)
(cd sdk/reproit-tui-py && PYTHONPATH=. pytest_run -q)
cargo test --manifest-path sdk/reproit-tui-rs/Cargo.toml
cc -std=c11 -Wall -Wextra -Werror runners/test_causal.c -o /tmp/reproit-test-causal
/tmp/reproit-test-causal

dotnet test sdk/reproit-windows/test/ReproIt.ParityTests/ReproIt.ParityTests.csproj
dotnet build sdk/reproit-windows/src/ReproIt.Windows/ReproIt.Windows.csproj -p:EnableWindowsTargeting=true

echo "causal validation matrix passed"
