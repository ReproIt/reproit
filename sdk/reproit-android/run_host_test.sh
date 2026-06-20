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
[ -f "$JUNIT_JAR" ] || curl -sL -o "$JUNIT_JAR" \
  https://repo1.maven.org/maven2/junit/junit/4.13.2/junit-4.13.2.jar
[ -f "$HAMCREST_JAR" ] || curl -sL -o "$HAMCREST_JAR" \
  https://repo1.maven.org/maven2/org/hamcrest/hamcrest-core/1.3/hamcrest-core-1.3.jar

SRC="$HERE/src/main/kotlin/com/reproit/android"
TST="$HERE/src/test/kotlin/com/reproit/android"

# NOTE: ReproIt.kt and ComposeCapture.kt are intentionally excluded: they import
# android.* / androidx.compose.* and need the Android SDK + Compose runtime. All
# testable logic is in the pure-Kotlin core below (Compose.kt is the pure Compose
# semantics-to-descriptor mapping, with no androidx import).
"$KOTLINC" -cp "$JUNIT_JAR:$HAMCREST_JAR" \
  "$SRC/Signature.kt" "$SRC/Json.kt" "$SRC/Config.kt" "$SRC/Engine.kt" \
  "$SRC/Fingerprint.kt" "$SRC/Compose.kt" \
  "$TST/SignatureParityTest.kt" "$TST/ComposeMappingTest.kt" \
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
  com.reproit.android.ComposeMappingTest
