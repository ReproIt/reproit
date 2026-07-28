#!/usr/bin/env bash
set -euo pipefail

: "${RUNNER_TEMP:?RUNNER_TEMP must name the evidence directory parent}"

export REPROIT_GATE_OUTPUT_DIR="${RUNNER_TEMP}/native-gates"

python3 validation/backends/gate.py compose-android
python3 validation/backends/gate.py flutter-android
python3 validation/backends/gate.py react-native-android
