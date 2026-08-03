# Security and data handling (Cloud)

The architecture is the primary control: reproduction runs on your infrastructure, so the most
sensitive material never reaches us at all. Everything below is secondary to that.

For what the SDK captures and what it refuses to capture, read [security](../security.md). This
page is only about what Cloud holds.

## What Cloud never receives

- **Your source code, builds, or simulators.** Reproduction executes in your CI or on your machine.
- **The values your users type.** SDKs fingerprint inputs structurally. There is no field for the
  value, on any platform.
- **Session recordings, screenshots, or pixels of your users' sessions.**

## Isolation

One workspace maps to one database and one artifact namespace. Isolation is structural, not a
tenant column that a missing filter could leak past. Evidence paths are confined to the configured
artifact root, and worker paths to the configured jobs root.

## Authentication

There are no passwords. Sign-in is a single-use mailbox link or your identity provider. Session and
API tokens are stored hashed. Publishable and secret keys have separate capabilities, and a
publishable key presented to a management route is rejected rather than downgraded.

## Encryption

Stated precisely, because "encrypted at rest" is usually stated imprecisely:

| what | at rest |
| --- | --- |
| Traffic to and from Cloud | TLS |
| Integration credentials and tenant connection strings | ChaCha20-Poly1305 AEAD, keyed per deployment, fresh nonce per write |
| Session and API tokens | stored hashed, never recoverable |
| Evidence blobs | the storage provider's own at-rest encryption. There is no application-layer envelope encryption and no per-workspace key today |

That last row is a real limit and we would rather write it down than imply more. If your threat
model requires a key you hold, the local path already supports it: capsules are AES-256-GCM
encrypted under a key you can supply with `REPROIT_CAPSULE_KEY`, and a capture can stay on your
machine entirely with `reproit capture --local-only`.

## Ingest validation

Ingest verifies the SDK's redaction claim and rejects a batch whose claim does not hold. Error
events without a well-formed oracle id are dropped rather than stored as untyped errors. Both are
fail-closed: the failure mode is losing an event, not storing something unclassified.

## Reporting a vulnerability

Report privately, not as a public issue. Include the affected version, deployment shape, a
reproduction, and impact, without customer credentials or evidence. See
[reproit.com/privacy](https://reproit.com/privacy) and [reproit.com/dpa](https://reproit.com/dpa)
for the processing terms and the subprocessor list.
