# ReproIt capture SDKs

ReproIt has one source-neutral capture contract for all software. A runtime SDK is one emitter.
Host collectors, crash reporters, OpenTelemetry adapters, browser SDKs, device adapters, and
offline support bundles emit the same causal facts.

The SDK does not promise that telemetry alone can recreate every failure. It records what happened,
marks every incomplete capability, and lets Cloud compile the highest-fidelity reproduction that
the evidence and authorized execution environment permit.

## The complete path

1. Create a project at [cloud.reproit.com](https://cloud.reproit.com).
2. Copy its write-only `pk_live_...` SDK key.
3. Add the SDK for your platform and initialize it in the release build.
4. Deploy normally. ReproIt groups genuine failure occurrences.
5. On a development machine, run:

```sh
reproit login
reproit occ_...
```

`reproit occ_...` downloads the immutable occurrence and exportable resources into the current
checkout. It then binds only checkout-owned execution providers and runs locally, in Compose, on a
device or VM, in customer CI, or on an authorized private worker. ReproIt never accepts executable
commands from evidence and never downloads source from Cloud.

## Universal recorder

The reference recorder core is available for Rust in
`crates/reproit-recorder` and for Node in
[`reproit-recorder-node`](reproit-recorder-node/README.md). Semantic adapters record:

- process start and exit;
- operation start and end;
- command, request, RPC, message, timer, job, installer, migration, device, or UI triggers;
- replayable inputs or explicit structural-only and environment-bound values;
- filesystem, registry, database, cache, queue, object, application, and device state;
- dependency calls and returns;
- persistent effects and observation checkpoints;
- exact failure observations and capture defects.

Recorder buffers, queues, artifacts, retries, payloads, and work per flush are bounded. Values are
classified at capture time as `structural`, `replayable`, `artifact`, or `environment-bound`.
Unredacted restricted data cannot be marked exportable.

## Choose your platform

Every 1.0 platform SDK ships as a checksummed archive on the matching GitHub
release. The web SDK also ships as JavaScript and an npm-compatible tarball.
Native registry publication is separate from GitHub release availability and
is not implied by a `1.0.0` package manifest.

| Platform                    | 1.0 release | Compatibility | Guide                                              | Installation |
| --------------------------- | ----------- | ------------- | -------------------------------------------------- | ------------ |
| Web                         | Released    | Stable        | [Web SDK](reproit-web.README.md)                   | Checksummed JS and npm assets |
| Electron and Tauri frontend | Released    | Preview       | [Web SDK](reproit-web.README.md)                   | Checksummed desktop-webview archive |
| iOS, iPadOS, macOS          | Released    | Preview       | [Apple SDK](reproit-ios/README.md)                 | Checksummed Swift package archive |
| Android Views and Compose   | Released    | Preview       | [Android SDK](reproit-android/README.md)           | Checksummed Gradle project archive |
| React Native                | Released    | Preview       | [React Native SDK](reproit-react-native/README.md) | Checksummed npm-source archive |
| Flutter                     | Released    | Preview       | [Flutter SDK](reproit_flutter/README.md)           | Checksummed Flutter package archive |
| Windows WPF and WinUI 3     | Released    | Preview       | [Windows SDK](reproit-windows/README.md)           | Checksummed .NET project archive |
| Linux GTK and Qt            | Released    | Preview       | [Linux SDK](reproit-linux/README.md)               | Checksummed Python package archive |

The release job verifies every archive's package manifest and checksum. The web
release job additionally installs its generated tarball into a clean Node
project and verifies the global API before publication. Package names are not
presented as registry installs until those registry packages exist and are
release-smoked.

## Credentials

Use the key intended for the environment:

- `pk_live_...` is write-only and project-bound. Put this key in browser and client application SDK
  configuration.
- `sk_live_...` can read and manage project data. Keep it in the CLI, CI secret store, or trusted
  server code. Never ship it in a browser or mobile binary.
- `reproit login` is preferred for developer machines and removes the need to copy either key into a
  shell command.

Universal capture batches use:

```text
https://ingest.reproit.com/v1/capture-batches
```

Exportable artifact bytes are digest-verified and uploaded before the batch. Local-analysis and
environment-bound bytes never leave their authorized worker. Existing platform SDKs may continue
using `/v1/events`; Cloud translates that v1 stream into the universal model while they migrate.

Platform SDKs that append their own route receive the base URL:

```text
https://ingest.reproit.com
```

Each platform guide shows the correct form. Self-hosted installations replace the hosted origin with
their own deployment.

## Production configuration

Debug-only convenience starters are useful for local inspection, but production capture must be
explicitly enabled and must include the Cloud project id, full ingest endpoint, publishable key, and
build identity. Build identity is how ReproIt distinguishes a regression from an older occurrence.

The equivalent configuration on every platform is:

```text
appId:         project id from Cloud
endpoint:      the platform guide's hosted endpoint value
publishableKey: pk_live_...
build.version: version shown to users
build.commit:  source revision deployed to production
redactLabels:  true when visible control labels must not leave the app
```

Each platform guide provides the native spelling for these fields.

## Universal wire protocol

New SDKs normalize their records into the strict universal capture batch:

```json
{
  "version": 1,
  "batchId": "cb_...",
  "projectId": "app_...",
  "sessionId": "session_...",
  "emitter": {
    "id": "orders-api",
    "kind": "runtime-sdk",
    "component": "orders",
    "runtime": "node"
  },
  "deployment": { "version": "1.0.0", "commit": "abc123" },
  "observedAt": "2026-07-27T12:00:00Z",
  "policy": {
    "consent": "application-telemetry",
    "retentionClass": "standard"
  },
  "capabilities": [
    { "capability": "http", "completeness": "complete" }
  ],
  "events": [
    {
      "id": "evt_orders-api_1",
      "sequence": 1,
      "monotonicNs": 1,
      "causalParentIds": [],
      "event": {
        "kind": "trigger",
        "trigger": "http-request",
        "subject": "POST /orders",
        "value": {
          "representation": "replayable",
          "value": { "body": { "sku": "widget" } },
          "redaction": "redacted-at-source"
        }
      }
    }
  ],
  "artifacts": []
}
```

Unknown or unrepresentable facts become explicit defect events. They are never silently dropped or
treated as clean evidence. The canonical fixture is
[`capture-batch-v1.json`](capture-batch-v1.json), which the shared Rust protocol parses and
compiles in its test suite. [`event-batch-v1.json`](event-batch-v1.json) remains the compatibility
fixture for SDKs still on the earlier UI and backend event stream.

## What is captured

The SDK records only values allowed by the application's capture policy. Inputs can be replayable
after source redaction, structural-only, content-addressed artifacts, or environment-bound
references. Passwords, credentials, hidden values, and restricted customer data must never be
classified as replayable or exportable.

Read [data handling and privacy](../docs/data-handling.md) for the complete wire contract and
[structural signatures](../docs/signature.md) for the cross-platform identity contract.

## Verify the integration

After deploying the SDK:

1. Open the application and complete one ordinary interaction. This confirms that clean production
   traffic reaches the project without creating a bug.
2. Use a development or staging build with a deliberate uncaught test crash. Do not add a synthetic
   crash to the production build.
3. Confirm that one `bkt_...` bug appears in Cloud.
4. Run `reproit bkt_...` inside the app checkout and confirm the same failure.
5. Remove the deliberate crash before shipping.

Only genuine oracle failures become bugs. Clean sessions are used for build traffic and resolution
confidence, not displayed as findings.
