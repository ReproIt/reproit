# Mobile divergence marker parity, measured

Ledger gap 3. Android, iOS and React Native each emit the structured
`REPROIT:DIVERGENCE` marker the CLI's verdict path parses ALONGSIDE the frozen
`CAPSULE:MISS` runner contract the fuzz harness consumes byte for byte. Until
this run nothing asserted the two are emitted together, and nothing compared the
three platforms against each other: the simulator and emulator scripts were
per-platform and manual, and the only cross-platform check was a grep for the
marker string in source text.

That mattered because mobile had already shipped emitting `CAPSULE:MISS` alone.
A mobile capsule replayed through `reproit check` could not report `Diverged` at
all, which is precisely the verdict the whole marker exists to produce.

`run.sh` asks all three platforms one question and compares the answers.

## What ran

| platform | how it executed | not |
| --- | --- | --- |
| Android | the real `CausalHttp`, compiled with `kotlinc` against `android.jar`, dexed with `d8`, and run under ART on a booted Pixel_9a emulator via `app_process` | a host JVM approximation |
| iOS | the real `ReproItCausalURLProtocol`, compiled for the simulator runtime and run with `xcrun simctl spawn` on a booted iPhone 16 Pro | a macOS host build |
| React Native | the real `installCausalFetch` from `src/causal.ts`, compiled with the SDK's own TypeScript and run under node | a source-text grep |

The capsule and the live call come from `sdk/capture-behavior-v1.json`
(`vocabularies.divergenceMarkers.parityScenario`), so the three platforms are
asked the same question rather than three similar ones.

## Result

```
divergence-parity: one capsule, one unmatched call, three platforms
  live call: http://svc.internal/unknown
  building the Android probe from the real SDK sources
  building the iOS probe from the real SDK sources (iOS 18.5 runtime)
  building the React Native probe from the real SDK sources
divergence-parity: comparing what the three platforms said
  structured marker, identical on all three: {"protocol": "http", "got": {"method": "GET", "url": "http://svc.internal/unknown"}, "action": 0}
  runner contract, identical on all three:   CAPSULE:MISS GET http://svc.internal/unknown action=0
  [1] android emits REPROIT:DIVERGENCE at the start of a stderr line
  [2] ios emits REPROIT:DIVERGENCE at the start of a stderr line
  [3] react native emits REPROIT:DIVERGENCE at the start of a stderr line
  [4] all three still throw the frozen CAPSULE:MISS runner contract
  [5] the structured payload is identical across all three platforms
  [6] the runner contract is identical across all three platforms
  [7] both markers were emitted together, never one instead of the other
divergence-parity: PASS (7 cases)
```

## Negative controls

Each defect below was reintroduced into production source, the harness was run,
and the source was reverted. Every one is a shape that has actually shipped
somewhere in this repository.

| control | change | harness said |
| --- | --- | --- |
| wrong stream | Android `System.err.println` became `System.out.println` | `android: no line STARTS with 'REPROIT:DIVERGENCE ' on stderr; it went to stdout, and the CLI's verdict path reads stderr` |
| not at line start | iOS prefixed the marker with `Causal.swift:141: warning: `, the shape Ruby's `warn(uplevel:)` produced | `ios: ... it appears mid-line on stderr, and the CLI matches the line start` |
| silently dropped | React Native stopped writing the structured marker and threw `CAPSULE:MISS` alone, the exact defect this addition existed to fix | `rn: ... it was not emitted at all, so this replay can never report Diverged` |
| platforms disagree | Android uppercased the URL in its report | `android: structured marker is {... 'HTTP://SVC.INTERNAL/UNKNOWN' ...} but the vector says {... 'http://svc.internal/unknown' ...}` and `the three platforms disagree on the structured marker` |

The fourth control is the one no per-platform script could have caught: every
platform emitted both markers, on the right stream, at the start of the line,
and the run was still wrong.

## Two failure shapes found while building this

Both looked exactly like a platform that emitted no marker, and both are now
hard failures in `run.sh` rather than silence:

- A probe built against the newest installed SDK aborts in dyld before `main`
  when the simulator runs an older runtime (SDK 26.2, runtime 18.5). The harness
  now resolves the device's runtime version and targets that.
- `xcrun simctl spawn` passes only `SIMCTL_CHILD_` prefixed variables to the
  child, stripping the prefix. Setting them unprefixed left the probe with no
  capsule at all.

## Cost

Requires a booted Android emulator and a booted iOS simulator, plus `kotlinc`
and node. CI does not re-run it, which is stated rather than implied; the
device-free half of the same invariant is asserted in each mobile SDK's own
behavior-vectors suite.
