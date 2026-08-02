# native-gates flake hardening, measured 2026-08-02

Three distinct defects made native-gates chronically red over the last 31
completed runs (macos-flutter 7 failures, macos-swiftui 3, linux-containers 1).
Each was root-caused from run logs before any change.

## 1. flutter-ios: gate budget killed the last retry tier mid-attempt

Runs 30729645597 and 30731343826: tier 1 stalls (the tool echoes the Dart
VM-service URI, then the connect hangs; exit 121 at the 75s bound), tier 2
stalls, and tier 3 (fresh simulator, rebuilt app) is then SIGTERMed by
gate.py's 1500s budget before its own bounded window expires. The ladder never
got its final full try. The green run 30729162949 shows the same tier-1 stall
saved by tier 2, so the stall shape is chronic, not commit-correlated: the
diff between the last green and first red is a pure rustfmt sweep.

Fix: flutter-ios timeoutSeconds 1500 -> 2400 so three full tiers fit, job
timeout-minutes 30 -> 50, retry tiers run with a 150s idle bound (no build to
wait for), and retry tiers pass --no-dds since the measured hang is in the
VM-service/DDS attach after successful URI discovery. The per-attempt evidence
line now records the dds flag per tier, so CI accumulates the A/B data for the
mitigation instead of hiding it. Proven by
validation/backends/test_flutter_drive_retry_contract.py (3/3 with the new
tier shapes) and a full local flutter-ios gate run on a real simulator.

## 2. swiftui-ios: 900s budget expired 14 seconds after the smoke passed

Run 30729490321: every smoke assertion passed at 02:58:33+881s; gate.py TERMed
the runner at exactly 02:58:33+900s during teardown and the gate reported
failure. A green run takes ~374s for the same gate; the failing runner was
2.4x slower, so 900s has no variance headroom. Fix: swiftui-ios
timeoutSeconds 900 -> 1800.

## 3. linux-containers: one docker registry i/o timeout fails the whole job

Run 30729160248: `failed to resolve source metadata for ubuntu:24.04: dial
tcp: i/o timeout` (DeadlineExceeded) before any layer built. Fix: the four
container-gate builds (run-tauri.sh, atspi-scenario-e2e.sh, qt-atspi-e2e.sh,
checkpoint-e2e.sh) go through docker-build-retry.sh: 3 bounded attempts,
retrying ONLY on transient network signatures, immediate fail-closed exit on
any real build error. Proven with a stubbed docker: transient-then-pass
retries once and exits 0; always-transient stops at 3 attempts; a real build
failure exits 1 after exactly 1 attempt.
