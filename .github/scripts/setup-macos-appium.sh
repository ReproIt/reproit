#!/usr/bin/env bash
set -euo pipefail

: "${RUNNER_TEMP:?RUNNER_TEMP must name the temporary directory}"
: "${GITHUB_PATH:?GITHUB_PATH must name the GitHub Actions path file}"

APPIUM_ROOT="${REPROIT_APPIUM_ROOT:-$RUNNER_TEMP/reproit-appium}"
APPIUM_HOME_DIR="${APPIUM_HOME:-$RUNNER_TEMP/reproit-appium-home}"
RUNNER_MODULES="${REPROIT_RN_RUNNER_MODULES:-runners/rn/node_modules}"
APPIUM_BIN="$APPIUM_ROOT/node_modules/.bin/appium"

case "$APPIUM_ROOT" in
  "$RUNNER_TEMP"/*) ;;
  *) echo "REPROIT_APPIUM_ROOT must be below RUNNER_TEMP" >&2; exit 2 ;;
esac
case "$APPIUM_HOME_DIR" in
  "$RUNNER_TEMP"/*) ;;
  *) echo "APPIUM_HOME must be below RUNNER_TEMP" >&2; exit 2 ;;
esac

appium_cache_is_valid() {
  [[ -x "$APPIUM_BIN" ]] || return 1
  [[ "$($APPIUM_BIN --version)" == "3.5.2" ]] || return 1
  APPIUM_HOME="$APPIUM_HOME_DIR" "$APPIUM_BIN" \
    driver list --installed --json 2>/dev/null | python3 -c '
import json
import sys

raw = sys.stdin.read()
start = raw.find("{")
if start < 0:
    raise SystemExit(1)
drivers, _ = json.JSONDecoder().raw_decode(raw[start:])
driver = drivers.get("xcuitest")
version = driver.get("version") if isinstance(driver, dict) else None
raise SystemExit(0 if version == "11.16.2" else 1)
'
}

if appium_cache_is_valid; then
  echo "Appium cache hit: 3.5.2 with XCUITest 11.16.2"
else
  rm -rf "$APPIUM_ROOT" "$APPIUM_HOME_DIR"
  mkdir -p "$APPIUM_ROOT" "$APPIUM_HOME_DIR"
  npm install --prefix "$APPIUM_ROOT" --no-audit --no-fund appium@3.5.2
  APPIUM_HOME="$APPIUM_HOME_DIR" "$APPIUM_BIN" \
    driver install xcuitest@11.16.2
fi

if [[ -d "$RUNNER_MODULES/webdriverio" ]]; then
  echo "React Native runner dependency cache hit"
else
  npm ci --prefix runners/rn --no-audit --no-fund
fi

printf '%s\n' "$APPIUM_ROOT/node_modules/.bin" >> "$GITHUB_PATH"
