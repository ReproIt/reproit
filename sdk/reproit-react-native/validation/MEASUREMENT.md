# React Native exchange capture: what was proven on a simulator

Platform: iPhone 16 Pro simulator (UDID 31900D16, iOS 18.5), Xcode 26.2,
arm64. Reproduce with `validation/simulator-e2e.sh`.

Before this run the RN capture claims rested on Jest alone. These numbers
come from the SDK's PUBLISHED dist modules (`npm run build` output for
`exchange`, `capture-batch`, `causal`) executing inside a `WKWebView` on the
booted simulator, wrapping WebKit's genuine `fetch`. No fetch shim is written
by the harness: `installCausalFetch` wraps whatever `fetch` the host provides,
which is exactly its integration point under React Native.

## The harness choice, stated plainly

A full React Native app would additionally exercise the React provider and
the NativeModules bridge, but it requires `react-native init` plus CocoaPods,
downloading roughly a gigabyte, and would mostly prove React Native's own
plumbing rather than this SDK's capture logic. The webview host was chosen as
the proportionate option.

Only `src/provider.tsx` imports `react-native`; the entire capture path
(`causal` to `exchange` to `capture-batch`) is dependency-free, which is why
it runs unmodified on an embedded host. The capsule arrives through the SDK's
own documented `globalThis.__reproit_capsule` override, which
`nativeCausalCapsule()` reads BEFORE it touches `NativeModules`, so the
replay path under test is the shipped one.

EXERCISED ON DEVICE: real device networking through WebKit's fetch, exchange
bounds, at-source redaction, envelope construction, capture-batch-v1
emission, the POST to ingest, capsule replay, and the fail-closed miss path.

NOT EXERCISED: the `ReproItProvider` React component, the `ReproItRuntime`
NativeModules bridge that injects the capsule on a real RN app, and the
Hermes/Metro runtime.

## What the device did

```
RP: phase=capture
RP: http-status=200
RP: http-body={"prices": null, "apiKey": "sk-live-SHOULD-NEVER-LEAVE-DEVICE", "tier": "gold"}
RP: planted-failure=TypeError: prices is not an array
RP: recorded-exchanges=1
RP: result=FAILED-AS-PLANTED
```

The capture batch the device POSTed carries:

- event sequence `operation-start, trigger, checkpoint, dependency,
  operation-end, observation`
- the exchange WITH its response body, including `prices: null`
- `apiKey` reduced to a `$reproit` stub; the literal secret appears nowhere
  in the bytes that left the device, asserted by grep
- an envelope with real device values: `runtime: react-native`, `os: ios`,
  `tz: America/Los_Angeles`, `locale: en-US`, `osVersion` from the real user
  agent (`iPhone OS 18_5 ... AppleWebKit/605.1.15`), and a replay seed
- `deployment {version, commit}`
- `capabilities: [network: complete, user-interface: partial]`

`capture-validate` accepts the bytes the simulator sent: `capture-batch-v1
valid`.

## Replay and fail-closed, with the dependency killed

```
RP: http-status=200
RP: http-body={"prices":null,"apiKey":{"$reproit":{...}},"tier":"gold"}
RP: result=FAILED-AS-PLANTED
```

and for a URL the capsule does not hold:

```
RP: network-error=CAPSULE:MISS GET http://127.0.0.1:19801/unknown-endpoint action=0
RP: result=NETWORK-FAILED
```

## Marker contract, stated precisely

Like iOS, RN replay uses the frozen runner contract (`CAPSULE:HIT` and
`CAPSULE:MISS`) and does NOT emit the backend SDKs' `REPROIT:DIVERGENCE`
marker; `grep -rn DIVERGENCE src/` returns nothing.

## Harness artifact worth knowing

The first device run failed with `network-error=Load failed` because the
webview page and the stub dependency are different origins and the stub sent
no CORS headers. That is a property of the harness, not the SDK: under React
Native there is no origin policy on fetch. The stub servers now send
`Access-Control-Allow-Origin` and handle preflight.
