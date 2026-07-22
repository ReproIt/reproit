#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?usage: package-platform-sdks.sh VERSION OUTPUT_DIR}"
OUTPUT_DIR="${2:?usage: package-platform-sdks.sh VERSION OUTPUT_DIR}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "invalid release version: $VERSION" >&2
  exit 2
}

mkdir -p "$OUTPUT_DIR"

checksum() {
  local asset=$1
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$asset" > "$asset.sha256"
  else
    shasum -a 256 "$asset" > "$asset.sha256"
  fi
}

verify_member() {
  local asset=$1
  local member=$2
  tar -tzf "$asset" | awk -v expected="$member" '
    $0 == expected { found = 1 }
    END { exit !found }
  ' || {
    echo "release archive $asset is missing $member" >&2
    exit 1
  }
}

package_tree() {
  local name=$1
  local source=$2
  local marker=$3
  local prefix="reproit-${name}-v${VERSION}"
  local asset="$OUTPUT_DIR/${prefix}.tar.gz"

  git -C "$ROOT" archive --format=tar --prefix="$prefix/" "HEAD:$source" \
    | gzip -n > "$asset"
  verify_member "$asset" "$prefix/$marker"
  checksum "$asset"
}

package_paths() {
  local name=$1
  local marker=$2
  shift 2
  local prefix="reproit-${name}-v${VERSION}"
  local asset="$OUTPUT_DIR/${prefix}.tar.gz"

  git -C "$ROOT" archive --format=tar --prefix="$prefix/" HEAD "$@" \
    | gzip -n > "$asset"
  verify_member "$asset" "$prefix/$marker"
  checksum "$asset"
}

package_tree apple-sdk sdk/reproit-ios Package.swift
package_tree android-sdk sdk/reproit-android build.gradle.kts
package_tree react-native-sdk sdk/reproit-react-native package.json
package_tree flutter-sdk sdk/reproit_flutter pubspec.yaml
package_tree windows-sdk sdk/reproit-windows src/ReproIt.Core/ReproIt.Core.csproj
package_tree linux-sdk sdk/reproit-linux pyproject.toml
package_paths desktop-webview-sdk sdk/reproit-tauri/Cargo.toml \
  sdk/reproit-tauri sdk/reproit-web.js sdk/reproit-web.README.md
package_paths native-ui-sdk runners/reproit_imgui.h \
  runners/reproit_causal.h runners/reproit_clay.h runners/reproit_imgui.h
package_paths tui-sdks sdk/reproit-tui-rs/Cargo.toml \
  sdk/reproit-tui-go sdk/reproit-tui-py sdk/reproit-tui-rs sdk/reproit-tui-ts

test "$(find "$OUTPUT_DIR" -maxdepth 1 -type f -name '*.tar.gz' | wc -l)" -eq 9
test "$(find "$OUTPUT_DIR" -maxdepth 1 -type f -name '*.sha256' | wc -l)" -eq 9
echo "platform SDK release archives: 9 packages for v$VERSION"
