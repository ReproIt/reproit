#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

UDID="${REPROIT_IOS_UDID:-$(xcrun simctl list devices booted -j | python3 -c '
import json,sys
j=json.load(sys.stdin)
for runtime, devices in j.get("devices", {}).items():
    if "iOS" not in runtime: continue
    for d in devices:
        if d.get("state") == "Booted" and d.get("isAvailable", True):
            print(d["udid"]); raise SystemExit
')}"
if [[ -z "$UDID" ]]; then
  echo "FlutterDrive gate needs a booted iOS simulator (or REPROIT_IOS_UDID)" >&2
  exit 2
fi

APP="$WORK/app"
flutter create --platforms=ios --project-name reproit_flutter_fixture "$APP"
cp "$ROOT/examples/flutter-fixture/lib/main.dart" "$APP/lib/main.dart"
cargo build -p reproit --manifest-path "$ROOT/Cargo.toml"
(cd "$APP" && "$ROOT/target/debug/reproit" init --platform flutter --force --yes)
printf '{"budget":4}' > "$WORK/fuzz.json"

(cd "$APP" && flutter drive \
  --driver=test_driver/integration_driver.dart \
  --target=integration_test/journey_explore.dart \
  -d "$UDID" \
  --dart-define=REPROIT_FUZZ_CONFIG="$WORK/fuzz.json" \
  --dart-define=REPROIT_DEVICE=a) | tee "$WORK/run.log"

grep -q 'EXPLORE:STATE ' "$WORK/run.log"
grep -q 'EXPLORE:EDGE ' "$WORK/run.log"
grep -q 'key:s:toggle' "$WORK/run.log"
grep -q 'Detail revealed' "$WORK/run.log"
grep -q 'JOURNEY DONE' "$WORK/run.log"
grep -q 'All tests passed' "$WORK/run.log"
! grep -q 'EXCEPTION CAUGHT BY REPROIT' "$WORK/run.log"

echo "FlutterDrive backend passed native Flutter/iOS simulator runtime"
