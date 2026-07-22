#!/bin/sh
set -eu

version=${1:?usage: check-version-contract.sh VERSION}

require_literal() {
  path=$1
  literal=$2
  grep -Fqx "$literal" "$path" || {
    printf 'release version mismatch: %s does not contain %s\n' "$path" "$literal" >&2
    exit 1
  }
}

for path in \
  crates/llm/Cargo.toml \
  crates/reproit/Cargo.toml \
  crates/reproit-protocol/Cargo.toml \
  crates/tui-sig/Cargo.toml \
  sdk/reproit-tauri/Cargo.toml \
  sdk/reproit-tui-rs/Cargo.toml
do
  require_literal "$path" "version = \"$version\""
done

for path in \
  runners/package.json \
  runners/rn/package.json \
  runners/web/package.json \
  sdk/reproit-react-native/package.json \
  sdk/reproit-tui-ts/package.json
do
  require_literal "$path" "  \"version\": \"$version\","
done

for path in sdk/reproit-linux/pyproject.toml sdk/reproit-tui-py/pyproject.toml
do
  require_literal "$path" "version = \"$version\""
done

require_literal sdk/reproit-android/build.gradle.kts "version = \"$version\""
require_literal sdk/reproit_flutter/pubspec.yaml "version: $version"
require_literal sdk/reproit-windows/src/ReproIt.Core/ReproIt.Core.csproj \
  "    <Version>$version</Version>"
require_literal sdk/reproit-windows/src/ReproIt.Windows/ReproIt.Windows.csproj \
  "    <Version>$version</Version>"

require_literal sdk/reproit_flutter/README.md "      ref: v$version"
require_literal sdk/reproit-linux/README.md \
  "  'reproit-linux @ git+https://github.com/ReproIt/reproit.git@v$version#subdirectory=sdk/reproit-linux'"
require_literal sdk/reproit-ios/README.md \
  "git -C Vendor/reproit checkout v$version"
for path in \
  sdk/reproit-android/README.md \
  sdk/reproit-react-native/README.md \
  sdk/reproit-windows/README.md
do
  require_literal "$path" "git -C vendor/reproit checkout v$version"
done

printf 'release version contract: %s\n' "$version"
