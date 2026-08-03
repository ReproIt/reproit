#!/usr/bin/env bash
set -euo pipefail

ROOT="${REPROIT_SOURCE_ROOT:-/repo}"
GATE_CSV="${REPROIT_ANDROID_GATES:-compose-android,react-native-android,flutter-android}"
MAX_GATES=3
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

IFS=, read -r -a GATES <<<"$GATE_CSV"
if ((${#GATES[@]} == 0 || ${#GATES[@]} > MAX_GATES)); then
  echo "dependency preparation requires 1 to $MAX_GATES Android gates" >&2
  exit 2
fi

git config --global --add safe.directory "$ROOT"
python3 "$ROOT/validation/native/preflight.py" android
if [[ ",$GATE_CSV," == *",compose-android,"* \
    || ",$GATE_CSV," == *",react-native-android,"* ]]; then
  npm ci --prefix "$ROOT/runners/rn" --no-audit --no-fund
fi

for gate in "${GATES[@]}"; do
  case "$gate" in
    compose-android)
      (
        cd "$ROOT/fixtures/compose-fixture"
        ./gradlew --no-daemon :app:assembleDebug
      )
      ;;
    react-native-android)
      work="$(mktemp -d)"
      trap 'rm -rf "${work:-}"' EXIT
      template="/cache/react-native-template-0.76.9"
      template_sha256="/cache/react-native-template-0.76.9.sha256"
      rm -rf "$template"
      npx --yes @react-native-community/cli@15.1.3 init ReproitRnFixture \
        --version 0.76.9 --directory "$template" --skip-install --skip-git-init
      (
        cd "$template"
        tar --sort=name --mtime="@0" --owner=0 --group=0 --numeric-owner \
          -cf - .
      ) | sha256sum | awk '{print $1}' >"$template_sha256"
      cp -a "$template" "$work/app"
      cp "$ROOT/fixtures/react-native-fixture/App.tsx" "$work/app/App.tsx"
      cp "$ROOT/fixtures/react-native-fixture/index.js" "$work/app/index.js"
      sed -i 's/^newArchEnabled=true$/newArchEnabled=false/' \
        "$work/app/android/gradle.properties"
      npm install --prefix "$work/app" --no-audit --no-fund
      (
        cd "$work/app/android"
        ./gradlew --no-daemon \
          -PreactNativeArchitectures=x86_64 :app:assembleRelease
      )
      rm -rf "$work"
      trap - EXIT
      ;;
    flutter-android)
      work="$(mktemp -d)"
      trap 'rm -rf "${work:-}"' EXIT
      cargo fetch --locked --manifest-path "$ROOT/Cargo.toml"
      cargo build --locked -p reproit --manifest-path "$ROOT/Cargo.toml"
      flutter create --platforms=android \
        --project-name reproit_flutter_fixture "$work/app"
      cp "$ROOT/fixtures/flutter-fixture/lib/main.dart" "$work/app/lib/main.dart"
      (
        cd "$work/app"
        "$CARGO_TARGET_DIR/debug/reproit" init --platform flutter --force --yes
        flutter pub get
        flutter build apk --debug
      )
      rm -rf "$work"
      trap - EXIT
      ;;
    *)
      echo "gate is not assigned to the Android x86_64 lane: $gate" >&2
      exit 2
      ;;
  esac
done
