#!/usr/bin/env bash
# Inside-the-worker half of the native-window channel proof. Runs under
# dbus-run-session with DISPLAY already up.
set -euo pipefail

export GTK_MODULES=gail:atk-bridge
export NO_AT_BRIDGE=0

mkdir -p /tmp/pick
printf 'fixture payload\n' > /tmp/pick/chosen.txt

python3 /field/atspi-fixture.py > /tmp/fixture.log 2>&1 &
for _ in $(seq 1 150); do
  grep -q FIXTURE-READY /tmp/fixture.log && break
  sleep 0.2
done
grep -q FIXTURE-READY /tmp/fixture.log || {
  echo "fixture never started" >&2
  cat /tmp/fixture.log >&2
  exit 1
}

echo "=== windows on the bus"
python3 /field/atspi_window.py windows --timeout 30

echo "=== press the fixture button through its accessible action"
python3 /field/press-fixture-button.py

echo "=== drive the chooser"
python3 /field/atspi_window.py open-path --window "Choose a fixture file" \
  --path /tmp/pick/chosen.txt --timeout 30

for _ in $(seq 1 60); do
  grep -qE 'FIXTURE-SELECTED|FIXTURE-CANCELLED' /tmp/fixture.log && break
  sleep 0.5
done
echo "=== fixture stdout"
cat /tmp/fixture.log
grep -q 'FIXTURE-SELECTED /tmp/pick/chosen.txt' /tmp/fixture.log || {
  echo "the fixture did not confirm the driven selection" >&2
  exit 1
}
echo "native-window channel: PASS"
