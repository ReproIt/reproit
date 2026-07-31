# iOS exchange capture: what was proven on a simulator

Platform: iPhone 16 Pro simulator (UDID 31900D16, iOS 18.5), Xcode 26.2,
arm64. Reproduce with `validation/simulator-e2e.sh`.

Before this run the iOS capture claims rested on host `swift test` alone,
which compiles the Foundation logic but never puts it on a device. These
numbers come from an app installed with `xcrun simctl install` and driven
with `xcrun simctl launch --console-pty`.

## What the device did

The sample app (`validation/SimHarness/main.swift`) links the SDK as a real
static library, starts it with `captureExchanges: true`, makes a genuine
`URLSession` request to a local stub dependency, and hits a planted failure
when the response's `prices` field is null instead of an array.

```
RP: phase=capture
RP: http-status=200
RP: http-body={"prices": null, "apiKey": "sk-live-SHOULD-NEVER-LEAVE-DEVICE", "tier": "gold"}
RP: planted-failure=TypeError: prices is not an array
RP: result=FAILED-AS-PLANTED
```

The capture batch the device POSTed to the stub ingest carries:

- event sequence `operation-start, trigger, checkpoint, dependency,
  operation-end, observation`, matching the backend SDKs exactly
- the dependency exchange WITH its response body, including `prices: null`,
  the value that causes the failure
- `apiKey` reduced to a `$reproit` stub (`redacted: true, type: string,
  length: 33`). The literal secret does not appear anywhere in the bytes
  that left the device, asserted by grep over every received file
- a determinism envelope with real device values:
  `arch: arm64`, `os: ios`, `tz: America/Los_Angeles`, `runtime: swift`,
  `buildDigest: 142` (from the bundle), and a replay seed
- `deployment {version, commit}`
- `capabilities: [network: complete]`

`cargo run -q -p reproit-protocol --bin capture-validate` accepts the bytes
the simulator sent: `capture-batch-v1 valid`.

## Replay, with the dependency killed

The stub server is stopped and confirmed unreachable, then the same app runs
under the runner capsule contract:

```
CAPSULE:HIT a-0-0
RP: http-status=200
RP: http-body={"tier":"gold","apiKey":{"$reproit":{...}},"prices":null}
RP: result=FAILED-AS-PLANTED
```

The recorded response was served from the capsule and the planted failure
reproduced with nothing listening on the dependency's port.

## Fail-closed

A URL the capsule does not hold does not silently reach the network:

```
CAPSULE:MISS GET http://127.0.0.1:19801/unknown-endpoint action=0
RP: result=NETWORK-FAILED
```

## Marker contract, stated precisely

Mobile replay emits BOTH markers on an unmatched call, never one instead of
the other.

`CAPSULE:HIT` and `CAPSULE:MISS` are the frozen runner contract: they predate
production capture and the fuzz harness consumes them byte-for-byte, so
nothing about them may move. `REPROIT:DIVERGENCE ` is the structured marker
the CLI's verdict path parses, written to stderr at the start of its own line
with a JSON report.

This paragraph previously said the opposite, that `REPROIT:DIVERGENCE` did
not exist here and tooling expecting it was expecting a contract that did not
exist. That was true when it was written and became false when the structured
marker was added additively to all three mobile SDKs. It is corrected rather
than deleted because the reason for the change is the point: without the
structured marker a mobile capsule replayed through `reproit check` could
never report `Diverged` at all.

Agreement across Android, iOS and React Native is measured on real hardware by
`validation/mobile/divergence-parity/run.sh`; see that directory's
`MEASUREMENT.md`.

## What is still not exercised

- A physical device. Everything above is the simulator, which runs the same
  arm64 Foundation and URLSession stack but not real radios or a real
  sandbox profile.
- Signal-based crash capture (`catchSignals`), which the harness does not
  enable.
- The bounded-body path: this run's payload is well under the 8 KiB inline
  budget, so truncation with `bodySha256` is covered by host tests only.
