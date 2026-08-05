# Platform collectors

Application SDKs capture behavior. Platform collectors capture where that
behavior ran. They join through the same bounded `REPROIT_SESSION_ID`; the SDK
does not need to know whether it runs in Kubernetes, ECS, Compose, CI, a native
service, or a device environment.

Run the collector before the application sends its failure batch:

```bash
reproit platform-collect \
  --project "$REPROIT_PROJECT_ID" \
  --session "$REPROIT_SESSION_ID" \
  --component checkout
```

With Cloud credentials, the collector uploads to `/v1/platform-evidence`.
Cloud stores the bounded snapshot independently, then merges matching evidence
into the immutable capture before occurrence compilation. `--local-only` keeps
the snapshot local. `--output FILE` writes a mode-0600 JSON snapshot.

Only documented identity and resource variables are read. Secret values are
not included.

## Shared build and resource identity

All adapters accept these optional values:

```text
REPROIT_IMAGE_DIGEST=sha256:<64 hex characters>
REPROIT_ARTIFACT_DIGEST=sha256:<64 hex characters>
REPROIT_CPU_LIMIT_MILLIS=<positive integer>
REPROIT_MEMORY_LIMIT_BYTES=<positive integer>
REPROIT_STORAGE_LIMIT_BYTES=<positive integer>
```

Absent values become explicit fidelity gaps. They are never guessed.

## Kubernetes

Use the downward API so the collector can prove workload identity without a
Kubernetes API token:

```yaml
env:
  - name: REPROIT_SESSION_ID
    valueFrom: { fieldRef: { fieldPath: metadata.labels['dev.reproit.session'] } }
  - name: REPROIT_K8S_NAMESPACE
    valueFrom: { fieldRef: { fieldPath: metadata.namespace } }
  - name: REPROIT_K8S_POD_UID
    valueFrom: { fieldRef: { fieldPath: metadata.uid } }
  - name: REPROIT_K8S_WORKLOAD_KIND
    value: deployment
  - name: REPROIT_K8S_WORKLOAD_NAME
    value: checkout
  - name: REPROIT_K8S_CONTAINER
    value: api
```

`REPROIT_K8S_CLUSTER` is optional because Kubernetes does not expose a stable
cluster identity through the downward API.

## Docker Compose

Compose supplies `COMPOSE_PROJECT_NAME`. Add the stable service and session:

```yaml
services:
  api:
    environment:
      REPROIT_SESSION_ID: ${REPROIT_SESSION_ID}
      REPROIT_COMPOSE_SERVICE: api
      REPROIT_CONTAINER_ID: ${HOSTNAME}
```

Application ports remain internal. Repro It publishes only dynamically assigned
loopback debugger ports.

## Amazon ECS

ECS tasks expose `ECS_CONTAINER_METADATA_URI_V4`. Repro It fetches its `/task`
document with a bounded request and accepts only the documented link-local
address or loopback test endpoints. It never forwards task credentials.

## Serverless

The collector recognizes:

- AWS Lambda: `AWS_LAMBDA_FUNCTION_NAME`, `AWS_REGION`, and
  `AWS_LAMBDA_LOG_STREAM_NAME`.
- Google Cloud Run: `K_SERVICE`, `K_REVISION`, and optional
  `GOOGLE_CLOUD_REGION`.
- Azure Functions: `WEBSITE_SITE_NAME`, `WEBSITE_INSTANCE_ID`, and optional
  `REGION_NAME`.

Identity capture does not claim that a compatible local executor exists.

## Native services

- systemd uses `INVOCATION_ID` plus `REPROIT_SYSTEMD_UNIT`.
- launchd uses `XPC_SERVICE_NAME`.
- Windows services use `REPROIT_WINDOWS_SERVICE_NAME` and optional
  `REPROIT_WINDOWS_SERVICE_INSTANCE`.

Explicit service names prevent attribution based only on executable name.

## CI

GitHub Actions, GitLab CI, Buildkite, and CircleCI are detected through their
documented provider variables. CI evidence can coexist with a container or
device identity in the same bounded deployment record.

## Android

`ANDROID_SERIAL` identifies the target. Complete replay evidence also supplies:

```text
REPROIT_ANDROID_API_LEVEL
REPROIT_ANDROID_ARCH
REPROIT_ANDROID_APPLICATION_ID
```

The acquisition worker still records reset evidence, permissions, network
policy, and AVD identity.

## iOS

Xcode supplies `SIMULATOR_UDID`, `SIMULATOR_RUNTIME_VERSION`, and
`SIMULATOR_MODEL_IDENTIFIER`. External workers may provide:

```text
REPROIT_IOS_UDID
REPROIT_IOS_RUNTIME
REPROIT_IOS_DEVICE_TYPE
REPROIT_IOS_BUNDLE_ID
```

Simulator reset and permission receipts remain executor responsibilities.

## Honest outcomes

If a platform marker is present but stable identity is incomplete, the
collector submits a named capability gap instead of an invented value. Cloud
cannot label the occurrence debug-ready until the required platform evidence
and a trusted local executor both exist.
