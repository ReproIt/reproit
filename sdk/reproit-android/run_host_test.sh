#!/bin/sh
# Host-only signature-parity test runner: NO Android SDK / Gradle required.
#
# Compiles the pure-Kotlin core (Signature/Json/Config/Engine) and the JUnit
# parity test with a standalone kotlinc, then runs it on the host JVM. This is
# the same path used to verify parity on a machine without the Android SDK.
#
# Requirements: a JDK (`java` on PATH) and a Kotlin compiler. If `kotlinc` is not
# on PATH, set KOTLINC to its location, e.g.
#   KOTLINC=/path/to/kotlinc/bin/kotlinc sh run_host_test.sh
# JUnit + Hamcrest jars are fetched to /tmp if not provided via JUNIT_JAR /
# HAMCREST_JAR.
set -e

HERE=$(cd "$(dirname "$0")" && pwd)
KOTLINC=${KOTLINC:-kotlinc}
OUT=$(mktemp -d)

JUNIT_JAR=${JUNIT_JAR:-/tmp/junit-4.13.2.jar}
HAMCREST_JAR=${HAMCREST_JAR:-/tmp/hamcrest-core-1.3.jar}
JUNIT_SHA256=8e495b634469d64fb8acfa3495a065cbacc8a0fff55ce1e31007be4c16dc57d3
HAMCREST_SHA256=66fdef91e9739348df7a096aa384a5685f4e875584cce89386a7a47251c4d8e9

# Fetch and VERIFY. `curl -sL` writes a file whether or not it got the jar, and
# `[ -f ]` then accepts a truncated or half-written one, so a bad download
# surfaced as `unresolved reference 'junit'` from kotlinc: a supply problem
# reported as a source problem. That is what happened in CI, where the jars
# arrived corrupt and the visible error blamed a test file that was correct.
# A digest also means this script cannot silently compile against a substituted
# jar, which an unverified curl into a world-writable /tmp path allows.
fetch_verified() {
  jar=$1
  url=$2
  want=$3
  if [ -f "$jar" ] && [ "$(digest_of "$jar")" = "$want" ]; then
    return 0
  fi
  rm -f "$jar"
  curl -fsSL -o "$jar" "$url" || {
    echo "FAIL could not download $url (network, not a source defect)" >&2
    exit 1
  }
  got=$(digest_of "$jar")
  [ "$got" = "$want" ] || {
    echo "FAIL $jar digest mismatch: expected $want, got $got." >&2
    echo "     The download is corrupt or substituted. This is a supply" >&2
    echo "     failure, not a compile error in the test sources." >&2
    rm -f "$jar"
    exit 1
  }
}

digest_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

fetch_verified "$JUNIT_JAR" \
  https://repo1.maven.org/maven2/junit/junit/4.13.2/junit-4.13.2.jar "$JUNIT_SHA256"
fetch_verified "$HAMCREST_JAR" \
  https://repo1.maven.org/maven2/org/hamcrest/hamcrest-core/1.3/hamcrest-core-1.3.jar \
  "$HAMCREST_SHA256"

SRC="$HERE/src/main/kotlin/com/reproit/android"
TST="$HERE/src/test/kotlin/com/reproit/android"

# NOTE: ReproIt.kt and ComposeCapture.kt are intentionally excluded: they import
# android.* / androidx.compose.* and need the Android SDK + Compose runtime. All
# testable logic is in the pure-Kotlin core below (Compose.kt is the pure Compose
# semantics-to-descriptor mapping, with no androidx import).
"$KOTLINC" -cp "$JUNIT_JAR:$HAMCREST_JAR" \
  "$SRC/Signature.kt" "$SRC/Json.kt" "$SRC/Config.kt" "$SRC/Engine.kt" \
  "$SRC/Fingerprint.kt" "$SRC/Compose.kt" \
  "$SRC/IndicatorRelation.kt" "$SRC/StructuralContracts.kt" \
  "$SRC/Exchange.kt" "$SRC/CaptureBatch.kt" "$SRC/CapsuleSpool.kt" \
  "$TST/SignatureParityTest.kt" "$TST/ComposeMappingTest.kt" "$TST/InvariantTest.kt" \
  "$TST/IndicatorRelationTest.kt" "$TST/StructuralContractsTest.kt" \
  "$TST/CausalRedactionTest.kt" "$TST/ExchangeCaptureTest.kt" \
  "$TST/CaptureBatchTest.kt" "$TST/EmitCaptureBatch.kt" \
  "$TST/BehaviorVectorsTest.kt" "$TST/CapsuleSpoolTest.kt" \
  -d "$OUT/classes.jar"

# Locate kotlin-stdlib next to the compiler. Resolve symlinks (Homebrew points
# /opt/homebrew/bin/kotlinc at a Cellar install whose libs live under libexec/lib)
# and probe both the classic `lib/` and Homebrew's `libexec/lib/` layouts.
KC_BIN=$(command -v "$KOTLINC" || echo "$KOTLINC")
# Best-effort realpath without depending on a `realpath` binary: follow symlinks
# (resolving relative link targets against the link's own directory).
while [ -L "$KC_BIN" ]; do
  link=$(readlink "$KC_BIN")
  case "$link" in
    /*) KC_BIN="$link" ;;
    *) KC_BIN="$(dirname "$KC_BIN")/$link" ;;
  esac
done
KC_HOME=$(cd "$(dirname "$(dirname "$KC_BIN")")" && pwd)
STDLIB=""
for cand in \
  "$KC_HOME/lib/kotlin-stdlib.jar" \
  "$KC_HOME/libexec/lib/kotlin-stdlib.jar"; do
  [ -f "$cand" ] && STDLIB="$cand" && break
done
if [ -z "$STDLIB" ]; then
  echo "could not locate kotlin-stdlib.jar near $KC_BIN" >&2
  exit 1
fi

java -cp "$OUT/classes.jar:$JUNIT_JAR:$HAMCREST_JAR:$STDLIB" \
  org.junit.runner.JUnitCore \
  com.reproit.android.SignatureParityTest \
  com.reproit.android.ComposeMappingTest \
  com.reproit.android.InvariantTest \
  com.reproit.android.IndicatorRelationTest \
  com.reproit.android.StructuralContractsTest \
  com.reproit.android.CausalRedactionTest \
  com.reproit.android.ExchangeCaptureTest \
  com.reproit.android.CaptureBatchTest \
  com.reproit.android.BehaviorVectorsTest \
  com.reproit.android.CapsuleSpoolTest

# Wire proof: the batch the SDK actually builds must satisfy the protocol
# validator, not just our own assertions about its shape. Skipped with a loud
# note when the Rust workspace is unavailable, never silently passed.
CLI_ROOT=$(cd "$HERE/../.." && pwd)
if [ -f "$CLI_ROOT/Cargo.toml" ] && command -v cargo >/dev/null 2>&1; then
  echo "validating the emitted capture batch against reproit-protocol"
  java -cp "$OUT/classes.jar:$JUNIT_JAR:$HAMCREST_JAR:$STDLIB" \
    com.reproit.android.EmitCaptureBatch |
    (cd "$CLI_ROOT" && cargo run -q -p reproit-protocol --bin capture-validate)
else
  echo "SKIP capture-validate: no cargo or Rust workspace at $CLI_ROOT" >&2
fi
