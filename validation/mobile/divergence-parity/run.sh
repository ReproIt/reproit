#!/bin/bash
# Cross-platform proof that the mobile divergence-marker split holds.
#
# Ledger gap 3. Android, iOS and React Native each emit the structured
# `REPROIT:DIVERGENCE` marker the CLI's verdict path parses ALONGSIDE the frozen
# `CAPSULE:MISS` runner contract the fuzz harness consumes byte for byte. Before
# this script the two markers were only ever checked one platform at a time, by
# hand, and mostly by grepping source text. Mobile had already shipped emitting
# CAPSULE:MISS alone, which meant a mobile capsule replayed through `reproit
# check` could never report Diverged at all.
#
# Source text is not the proof. Each platform is RUN:
#   Android      the real CausalHttp, dexed and executed on a booted emulator
#   iOS          the real ReproItCausalURLProtocol, on a booted iPhone simulator
#   ReactNative  the real installCausalFetch, under node
#
# All three are handed the SAME capsule and the SAME live call, taken from
# sdk/capture-behavior-v1.json (vocabularies.divergenceMarkers.parityScenario),
# and all three must answer identically. Divergence between the platforms is the
# defect this script exists to catch, so agreement is asserted, not each
# platform's output separately.
#
# Requires: a booted Android emulator (adb), a booted iOS simulator (simctl),
# kotlinc, node. Every prerequisite is a hard failure; there is no skip path,
# because a skipped platform is exactly the silence this gap describes.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
VECTORS="$ROOT/sdk/capture-behavior-v1.json"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

ANDROID_SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/Library/Android/sdk}}
ADB="$ANDROID_SDK/platform-tools/adb"
IOS_DEVICE=${IOS_DEVICE:-iPhone 16 Pro}

# Cases actually executed, asserted against EXPECTED_CASES at the end. A harness
# that exits early must not look like one that passed.
CASES=0
EXPECTED_CASES=7

step() {
  CASES=$((CASES + 1))
  echo "  [$CASES] $1"
}

fail() {
  echo "divergence-parity: $1" >&2
  exit 1
}

# --- the shared question -----------------------------------------------------

python3 - "$VECTORS" "$WORK" <<'PY'
import json, sys
scenario = json.load(open(sys.argv[1]))["vocabularies"]["divergenceMarkers"]["parityScenario"]
work = sys.argv[2]
with open(f"{work}/capsule.json", "w") as handle:
    json.dump(scenario["capsule"], handle)
with open(f"{work}/expect.json", "w") as handle:
    json.dump(scenario["expect"], handle)
with open(f"{work}/url", "w") as handle:
    handle.write(scenario["live"]["url"])
PY

PROBE_URL=$(cat "$WORK/url")
CAPSULE_JSON=$(cat "$WORK/capsule.json")
echo "divergence-parity: one capsule, one unmatched call, three platforms"
echo "  live call: $PROBE_URL"

# --- Android: dex the real SDK and run it on the emulator --------------------

command -v kotlinc >/dev/null || fail "kotlinc is required to build the Android probe"
[ -x "$ADB" ] || fail "adb not found at $ADB"
"$ADB" get-state >/dev/null 2>&1 || fail "no Android device; boot an emulator first"

ANDROID_JAR=$(ls -d "$ANDROID_SDK"/platforms/android-* | sort -V | tail -1)/android.jar
[ -f "$ANDROID_JAR" ] || fail "no android.jar under $ANDROID_SDK/platforms"
D8=$(ls -d "$ANDROID_SDK"/build-tools/* | sort -V | tail -1)/d8
[ -x "$D8" ] || fail "no d8 under $ANDROID_SDK/build-tools"

SRC="$ROOT/sdk/reproit-android/src/main/kotlin/com/reproit/android"
echo "  building the Android probe from the real SDK sources"
kotlinc -nowarn -cp "$ANDROID_JAR" \
  "$SRC/Signature.kt" "$SRC/Json.kt" "$SRC/Config.kt" "$SRC/Engine.kt" \
  "$SRC/Fingerprint.kt" "$SRC/Compose.kt" "$SRC/IndicatorRelation.kt" \
  "$SRC/StructuralContracts.kt" "$SRC/Exchange.kt" "$SRC/CaptureBatch.kt" \
  "$SRC/CausalHttp.kt" "$HERE/probes/AndroidProbe.kt" \
  -include-runtime -d "$WORK/android-probe.jar" >/dev/null

# d8 warns loudly about rewriting kotlin metadata it is too old to parse. That
# is noise about the stdlib, not about the SDK, and it buries a real failure.
"$D8" --lib "$ANDROID_JAR" --output "$WORK" "$WORK/android-probe.jar" \
  >/dev/null 2>"$WORK/d8.log" || { cat "$WORK/d8.log" >&2; fail "d8 failed"; }

REMOTE=/data/local/tmp/reproit-divergence-parity
"$ADB" shell "rm -rf $REMOTE && mkdir -p $REMOTE" >/dev/null
"$ADB" push "$WORK/classes.dex" "$REMOTE/probe.dex" >/dev/null
printf '%s' "$CAPSULE_JSON" > "$WORK/capsule-push.json"
"$ADB" push "$WORK/capsule-push.json" "$REMOTE/capsule.json" >/dev/null

# app_process runs a plain main() under ART, so this is the production Kotlin
# executing on the device rather than a host JVM approximation.
"$ADB" shell "cd $REMOTE && \
  REPROIT_CAPSULE=$REMOTE/capsule.json PROBE_URL='$PROBE_URL' \
  CLASSPATH=$REMOTE/probe.dex app_process / com.reproit.android.AndroidProbeKt" \
  >"$WORK/android.out" 2>"$WORK/android.err" || true
"$ADB" shell "rm -rf $REMOTE" >/dev/null

# --- iOS: build for the simulator and run it there ---------------------------

xcrun simctl bootstatus "$IOS_DEVICE" -b >/dev/null 2>&1 ||
  fail "iOS simulator '$IOS_DEVICE' is not available"

SIM_SDK=$(xcrun --sdk iphonesimulator --show-sdk-path)
# Target the RUNTIME the device actually runs, not the newest SDK installed. A
# binary built for a runtime newer than the simulator's aborts in dyld before
# main, which reads exactly like a platform that emitted no marker at all.
SIM_VERSION=$(xcrun simctl list devices available --json |
  IOS_DEVICE="$IOS_DEVICE" python3 -c 'import json,sys,os
device = os.environ["IOS_DEVICE"]
listing = json.load(sys.stdin)["devices"]
for runtime, devices in listing.items():
    if any(entry["name"] == device for entry in devices):
        print(runtime.rsplit(".", 1)[-1].removeprefix("iOS-").replace("-", "."))
        break')
[ -n "$SIM_VERSION" ] || fail "could not resolve the runtime of '$IOS_DEVICE'"
echo "  building the iOS probe from the real SDK sources (iOS $SIM_VERSION runtime)"
# One module with the SDK sources so the internal installer stays internal.
xcrun swiftc -sdk "$SIM_SDK" \
  -target "$(uname -m)-apple-ios${SIM_VERSION}-simulator" \
  "$ROOT/sdk/reproit-ios/Sources/ReproIt"/*.swift "$HERE/probes/main.swift" \
  -o "$WORK/ios-probe" >/dev/null

# simctl hands the child only the SIMCTL_CHILD_ prefixed variables, stripping
# the prefix. Setting them unprefixed leaves the probe with no capsule, which
# looks identical to a platform that chose not to emit a marker.
SIMCTL_CHILD_REPROIT_CAUSAL=1 \
  SIMCTL_CHILD_REPROIT_CAPSULE_JSON="$CAPSULE_JSON" \
  SIMCTL_CHILD_PROBE_URL="$PROBE_URL" \
  xcrun simctl spawn "$IOS_DEVICE" "$WORK/ios-probe" \
  >"$WORK/ios.out" 2>"$WORK/ios.err" || true

# --- React Native: the real wrapper under node -------------------------------

RN="$ROOT/sdk/reproit-react-native"
[ -d "$RN/node_modules" ] || fail "run npm install in $RN first"
echo "  building the React Native probe from the real SDK sources"
"$RN/node_modules/.bin/tsc" --outDir "$WORK/rn" --module commonjs \
  --moduleResolution node --target es2019 --skipLibCheck --strict false \
  --typeRoots "$RN/node_modules/@types" --types node \
  --rootDir "$ROOT" "$HERE/probes/rn-probe.ts" >/dev/null

REPROIT_CAPSULE_JSON="$CAPSULE_JSON" PROBE_URL="$PROBE_URL" \
  node "$WORK/rn/validation/mobile/divergence-parity/probes/rn-probe.js" \
  >"$WORK/rn.out" 2>"$WORK/rn.err" || true

# --- assert: both markers on every platform, and identical across them -------

echo "divergence-parity: comparing what the three platforms said"
python3 - "$WORK" "$VECTORS" <<'PY' || exit 1
import json, sys

work, vectors = sys.argv[1], sys.argv[2]
expect = json.load(open(vectors))["vocabularies"]["divergenceMarkers"]["parityScenario"]["expect"]
markers = json.load(open(vectors))["vocabularies"]["divergenceMarkers"]
structured_prefix = markers["structured"]

problems = []
structured = {}
contracts = {}

for platform in ("android", "ios", "rn"):
    out = open(f"{work}/{platform}.out", encoding="utf-8", errors="replace").read()
    err = open(f"{work}/{platform}.err", encoding="utf-8", errors="replace").read()

    # The structured marker must START a line on stderr. Ruby's warn(uplevel:)
    # prefixed it with file:line: and the CLI stopped seeing it entirely.
    lines = [line for line in err.splitlines() if line.startswith(structured_prefix)]
    if not lines:
        if structured_prefix in err:
            why = "it appears mid-line on stderr, and the CLI matches the line start"
        elif structured_prefix in out:
            why = "it went to stdout, and the CLI's verdict path reads stderr"
        else:
            why = "it was not emitted at all, so this replay can never report Diverged"
        problems.append(
            f"{platform}: no line STARTS with {structured_prefix!r} on stderr; {why}"
        )
    else:
        try:
            structured[platform] = json.loads(lines[0][len(structured_prefix):])
        except json.JSONDecodeError as error:
            problems.append(f"{platform}: the marker payload is not JSON: {error}")

    # The frozen runner contract reaches the caller as a thrown message, which
    # the probe echoes verbatim on stdout.
    miss = [line for line in out.splitlines() if line.startswith("PROBE:MISS ")]
    if not miss:
        problems.append(f"{platform}: the call did not fail closed; stdout was {out!r}")
    else:
        contracts[platform] = miss[0][len("PROBE:MISS "):].strip()

# Every platform agrees with the vector, and therefore with each other.
for platform, report in structured.items():
    if report != expect["structured"]:
        problems.append(
            f"{platform}: structured marker is {report} but the vector says "
            f"{expect['structured']}"
        )
for platform, contract in contracts.items():
    if contract != expect["runnerContract"]:
        problems.append(
            f"{platform}: runner contract is {contract!r} but the vector says "
            f"{expect['runnerContract']!r}"
        )

# Stated separately from the vector comparison: this is the cross-platform
# claim, and it must be false only when a platform already failed above.
if len(structured) == 3 and len({json.dumps(v, sort_keys=True) for v in structured.values()}) != 1:
    problems.append(f"the three platforms disagree on the structured marker: {structured}")
if len(contracts) == 3 and len(set(contracts.values())) != 1:
    problems.append(f"the three platforms disagree on the runner contract: {contracts}")

if problems:
    print("divergence-parity FAILED", file=sys.stderr)
    for problem in problems:
        print(f"  {problem}", file=sys.stderr)
    sys.exit(1)

print(f"  structured marker, identical on all three: {json.dumps(structured['android'])}")
print(f"  runner contract, identical on all three:   {contracts['android']}")
PY

step "android emits REPROIT:DIVERGENCE at the start of a stderr line"
step "ios emits REPROIT:DIVERGENCE at the start of a stderr line"
step "react native emits REPROIT:DIVERGENCE at the start of a stderr line"
step "all three still throw the frozen CAPSULE:MISS runner contract"
step "the structured payload is identical across all three platforms"
step "the runner contract is identical across all three platforms"
step "both markers were emitted together, never one instead of the other"

[ "$CASES" = "$EXPECTED_CASES" ] ||
  fail "ran $CASES cases, expected $EXPECTED_CASES"
echo "divergence-parity: PASS ($CASES cases)"
