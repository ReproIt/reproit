#!/usr/bin/env bash
set -euo pipefail

: "${RUNNER_TEMP:?RUNNER_TEMP must name the temporary directory}"

python3 validation/backends/gate.py swiftui-ios
python3 validation/backends/gate.py react-native-ios
