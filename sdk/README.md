# ReproIt production SDKs

ReproIt SDKs capture the structural path to a real production crash so the same bug can be replayed
locally with one command. They do not send typed values, passwords, hidden fields, or source code.

## The complete path

1. Create a project at [cloud.reproit.com](https://cloud.reproit.com).
2. Copy its write-only `pk_live_...` SDK key.
3. Add the SDK for your platform and initialize it in the release build.
4. Deploy normally. ReproIt groups genuine production crashes into `bkt_...` bugs.
5. On a development machine, run:

```sh
reproit login
reproit bugs
reproit bkt_...
reproit bkt_... --record-video
```

`reproit login` opens the browser and discovers every project you can access. The bucket command
downloads the structural actions and failure signature, then runs them against the app configuration
in the current directory. ReproIt never downloads your source.

## Choose your platform

| Platform                      | Guide                                              | Supported installation before registry publication |
| ----------------------------- | -------------------------------------------------- | -------------------------------------------------- |
| Web, Electron, Tauri frontend | [Web SDK](reproit-web.README.md)                   | Vendor one JavaScript file from this repository    |
| iOS, iPadOS, macOS            | [Apple SDK](reproit-ios/README.md)                 | Swift package from a local checkout                |
| Android Views and Compose     | [Android SDK](reproit-android/README.md)           | Gradle project from a local checkout               |
| React Native                  | [React Native SDK](reproit-react-native/README.md) | npm file dependency from a local checkout          |
| Flutter                       | [Flutter SDK](reproit_flutter/README.md)           | pub git dependency with the SDK subdirectory       |
| Windows WPF and WinUI 3       | [Windows SDK](reproit-windows/README.md)           | .NET project reference from a local checkout       |
| Linux GTK and Qt              | [Linux SDK](reproit-linux/README.md)               | pip git dependency with the SDK subdirectory       |

The package names reserved in the individual guides are not presented as registry installs until
those packages exist. Every command shown in the active quickstarts works against the public
repository today.

## Credentials

Use the key intended for the environment:

- `pk_live_...` is write-only and project-bound. Put this key in browser and client application SDK
  configuration.
- `sk_live_...` can read and manage project data. Keep it in the CLI, CI secret store, or trusted
  server code. Never ship it in a browser or mobile binary.
- `reproit login` is preferred for developer machines and removes the need to copy either key into a
  shell command.

The web SDK accepts the exact POST target:

```text
https://ingest.reproit.com/v1/events
```

Native SDKs append `/v1/events` and therefore receive the base URL:

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

## Wire protocol

Every SDK normalizes its platform capture records into the same strict version 1 event batch:

```json
{
  "version": 1,
  "batchId": "sdk-1717939200123-1",
  "appId": "app_...",
  "frames": [
    {
      "runId": "sdk-1717939200123-1",
      "sequence": 1,
      "scope": { "domain": "shared" },
      "event": { "kind": "graph-edge", "from": "a", "action": "tap", "to": "b" }
    }
  ],
  "evidence": []
}
```

The allowed event kinds are `action`, `observation`, `backend`, `graph-edge`, `finding`, and
`stream-defect`. A finding contains its identity, minimized path, and PII-safe context. Unknown or
unrepresentable capture records become an explicit `stream-defect`; they are never silently
dropped or treated as clean evidence. The shared protocol implementation owns validation, size
limits, reason codes, and tri-state evaluation semantics. The canonical complete fixture is
[`event-batch-v1.json`](event-batch-v1.json), and the shared Rust protocol parses and validates it
in its test suite.

## What is captured

The SDK records structural state signatures, stable control selectors, the action path, the finding
identity, build identity, and bounded derived input properties such as length or Unicode class. It
does not record raw input values.

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
