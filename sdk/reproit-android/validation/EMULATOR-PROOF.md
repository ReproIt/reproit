# Android SDK: what was proven on a real emulator

Device: `Pixel_9a` AVD, API 37, booted with
`emulator -avd Pixel_9a -no-snapshot -no-boot-anim -gpu swiftshader_indirect`.
Toolchain: the module's own committed wrapper (Gradle 8.11.1, AGP 8.7.3,
Kotlin 2.0.21). Every number below came from that device, not from the host
JUnit runner.

## The build, which was broken before this

`gradle assembleDebug` failed before touching any source:

```
> Plugin 'com.android.internal.library' relies on
  'org.gradle.api.problems.internal.InternalProblems', a Gradle internal API
  that was removed in Gradle 9.6.0. Update the plugin to a version that no
  longer uses Gradle internal APIs, or use Gradle 9.5.
```

The module had no committed wrapper, so it inherited whatever system Gradle
happened to be installed (9.6.1 here). A wrapper is now committed, pinned to
the SAME toolchain the composite build in `examples/compose-fixture` already
uses. One toolchain per module regardless of entry point: a green
`./gradlew test` compiles the same bytes the composite and device builds
compile, so a standalone pass is evidence about the AAR that ships.

```
./gradlew assembleDebug   BUILD SUCCESSFUL
./gradlew test            BUILD SUCCESSFUL   tests=69 failures=0 errors=0
```

## 1. Capture, on the device

A sample app mounts the SDK with `captureExchanges = true`, calls a stub
upstream through `ReproIt.causalHttp`, and crashes on the response:

```
I ReproItProof: reproit mounted captureExchanges=true
I ReproItProof: upstream status=200 body={"prices":null,"symbol":"ACME"}
E AndroidRuntime: org.json.JSONException: Value null at prices of type
                  org.json.JSONObject$1 cannot be converted to JSONArray
INGEST POST /v1/capture-batches
```

The shipped batch carries deployment identity, the envelope, and the exchange
with its response:

```
deployment:   {"version": "1.0.0", "commit": "abc123def456"}
capabilities: ["user-interface", "network"]
event kinds:  operation-start, trigger, checkpoint, dependency,
              operation-end, observation
envelope:     {"arch":"arm64-v8a","observedAtMs":...,"os":"17",
               "replaySeed":"951fa14736b2970f","runtime":"android",
               "tz":"America/Los_Angeles"}
request.body: {"symbol":"ACME","apiKey":{"$reproit":{"redacted":true,
               "type":"string","length":24}}}
response.body:{"prices":null,"symbol":"ACME"}
```

Validator round trip against the shipped batch:

```
cargo run -q -p reproit-protocol --bin capture-validate < batch.json
capture-batch-v1 valid
```

## 2. Replay, with the dependency gone

The upstream port is unmapped (`adb reverse --remove`) and the stub server is
killed, so the app CANNOT reach the network. The capsule is supplied through
`debug.reproit.capsule`:

```
D reproit : CAPSULE:HIT device-proof-0
I ReproItProof: upstream status=200 body={"prices":null,"symbol":"ACME"}
E AndroidRuntime: org.json.JSONException: Value null at prices of type
                  org.json.JSONObject$1 cannot be converted to JSONArray
```

The exception is IDENTICAL to the production one. That identity is the whole
point, and getting there required fixing a real defect (below).

## 3. Divergence, fail closed

A capsule whose recorded URL no longer matches the call the app makes:

```
E AndroidRuntime: java.lang.IllegalStateException:
                  CAPSULE:MISS POST http://127.0.0.1:39991/prices action=0
```

The call fails closed. It does not fall through to the live network.

## The defect this run caught, twice

`Json` omits null map values, which is correct for the event model's optional
fields (`from?`, `labels?`) and matches the other SDKs. It is wrong for
captured payloads, where a null is data. An upstream answering
`{"prices": null}` is frequently the CAUSE of the failure being captured.

The first replay attempt proved it, because the served body and the crash both
changed:

```
served:  {"symbol":"ACME"}                       (the null silently dropped)
crash:   org.json.JSONException: No value for prices
expected:org.json.JSONException: Value null at prices ... cannot be converted
```

A capsule that loses a null describes a response the dependency never sent, so
replay reproduces a DIFFERENT error than production. That is the unfaithful
capsule this product exists to avoid, and it was invisible to the host tests.

Fixed with a `JsonNull` sentinel that the encoder writes as `null`, applied on
all three paths that carry captured payloads: the capture body
(`boundedBody`), the replay serve path (`CausalHttp`), and the runner's
recorded marker (`bodyValue`). The event model's optional-field behavior is
unchanged, pinned by a test.

## 4. Capsule delivery on the crash path, measured and fixed

The first pass shipped the capture batch with a synchronous POST on the
crashing thread and reported delivery as "not 100 percent deterministic".
That was not a limitation to accept: the capsule is the artifact hermetic
replay depends on, so losing it to a race loses the product's whole claim.
Measured on this device, the race is real on both sides:

```
window between FATAL EXCEPTION and process death   168 ms .. 768 ms
first HTTP POST on a cold process (to LOCALHOST)    40 ms .. 316 ms
```

Those ranges overlap, which is exactly the intermittency. Adding a realistic
2 s ingest latency makes the loss deterministic, which is the honest way to
state the defect: on any real network the synchronous POST cannot win.

```
BEFORE, localhost ingest:      4 of 6 confirmed crashes delivered
BEFORE, 2 s ingest latency:    0 of 2 confirmed crashes delivered
AFTER,  2 s ingest latency:    9 of 9 confirmed crashes delivered
```

The fix is the standard crash-reporter shape, now in `CapsuleSpool`: during the
crash do only a bounded LOCAL write (temp file plus atomic rename, milliseconds,
no network), then upload on the next launch. The spool is bounded (8 capsules,
1 MiB total, oldest dropped, an oversized capsule refused rather than
truncated), delivery is at-most-once (claimed by rename, deleted only after the
POST is accepted), and a claim orphaned by a process that died mid-upload is
recovered by the next drain rather than stranded. That last property was itself
found on device: the first post-fix run lost one capsule to exactly that case.

Verified end to end, with the upstream unreachable during replay:

```
phase 1 (crash)   ingest=[]            spool: 1785495148657.capsule.json
phase 2 (next)    drain: pending=1     upload ok=true    ingest=[/v1/capture-batches]
                  spool after: (empty)
capture-validate < delivered-batch.json   capture-batch-v1 valid
replay from that capsule, upstream removed:
  D reproit : CAPSULE:HIT spooled-0
  E AndroidRuntime: org.json.JSONException: Value null at prices ... cannot be converted
```

The delivered batch still carries `deployment {version, commit}`, the envelope
(`arch, observedAtMs, os, replaySeed, runtime, tz`), the response body with its
`prices: null` intact, and `apiKey` reduced to a `$reproit` stub. The
measurement is reproducible: `validation/capsule-delivery.sh [runs]`, which
fails closed when no run reaches a confirmed crash so it can never report a
false pass.

## Honest limits
- The proof used one AVD (Pixel_9a, API 37) and one architecture (arm64-v8a).
- Delivery is at-most-once and eventually complete, not guaranteed: a capsule
  written to the spool is uploaded on a LATER launch, so an app that is never
  opened again keeps its capsule on disk until the spool's bound evicts it, and
  an uninstall discards it (correct: a reinstall is a different install).
- The spool survives a crash because filesDir does. It does not survive a
  device running out of storage mid-write; that write fails and the SDK falls
  back to the racing POST rather than silently reporting a delivery.
- Capture covers calls made through `ReproIt.causalHttp`. Kotlin cannot
  monkeypatch, so OkHttp or Retrofit traffic is invisible unless routed
  through it. Stated, not implied.
