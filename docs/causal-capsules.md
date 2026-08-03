# Causal capsules

A causal capsule is the executable input artifact behind a confirmed ReproIt finding. It combines
the minimized structural UI path with only the external inputs required to reproduce the exact
original failure.

Users do not manage capsules directly:

```sh
reproit fuzz
reproit bkt_...
reproit fnd_...
reproit keep fnd_...
```

## Trust lifecycle

An observation becomes public only after:

1. a clean live replay returns the exact finding identity;
2. action ddmin preserves that identity;
3. a final live capture aligns all evidence to the minimized action clock;
4. dependency-aware causal reduction preserves that identity;
5. environment reduction preserves that identity or records `ABSTAIN`;
6. the causal exchanges are replayed with live access fail-closed; and
7. a final clean capsule replay returns the identity again.

An unmatched request is `CAPSULE:MISS` and classifies as stale/incomplete, even if aborting it
produces a secondary application exception. A schema difference without an observed consumer failure
is a candidate, not a bug.

## Artifact

Capsule schema version 2 records actors, 1-based UI actions, bootstrap phase `0`, HTTP/event
exchanges, build identities, environment, capabilities, redaction manifest, the exact oracle
identity, a causal event DAG, and an environment-minimization envelope. IDs are SHA-256 content
addresses. Version 1 capsules are deliberately rejected rather than interpreted with weaker proof
semantics.

## Causal graph and reduction

The bounded causal DAG contains action, response, timer, state-write, callback, permission,
actor-event, backend-event, environment, and finding nodes. Its edges have five explicit meanings:

- `happens-before` records observed order;
- `data-dependency` connects a producer to evidence that consumes its value;
- `state-prerequisite` connects state establishment to a dependent event;
- `actor-ownership` connects actor events to their actor's actions and observations; and
- `contract-scope` connects evidence to the exact finding it supports.

The graph is validated for version, size, unique node identity, valid endpoints, and acyclicity in
both the CLI and Cloud protocol packages. Causal reduction removes candidate nodes in bounded
chunks. Removing an action also removes the response or state evidence that depends on it, while a
mere ordering edge does not imply deletion. Every accepted candidate must reproduce the exact
finding identity. The graph is rebuilt from the reduced capsule and compared during loading, so a
stored graph cannot drift from the executable artifact.

## Environment minimization

After causal reduction, Reproit tests captured, non-secret runtime defines one at a time by omitting
them from a cold replay. The work is bounded to 32 dimensions and eight replay attempts. A dimension
is relaxed only when the same finding identity reproduces. If the candidate does not reproduce, the
baseline is replayed again before the dimension may be called required. Runner errors, incomplete
capsule replay, unavailable mutation adapters, and exhausted budgets produce `ABSTAIN` instead of a
portability claim.

The accepted relaxed dimensions are then tested together. If that final combined replay is not an
exact reproduction, all relaxed claims are cleared. Captured values remain pinned by default during
normal replay; explicit command-line overrides still take precedence. `reproit proof <id>` displays
the DAG size, environment replay count, portable dimensions, non-reproducing trials, and abstentions.

Multi-actor capsules use actor-local 1-based action clocks. The authored checkpoint remains
immutable, generated actions shrink across the shared schedule, and capability aggregation uses the
least capable actor. Final confirmation boots every actor with the same guarded capsule.

Capsules are encrypted at rest with AES-256-GCM under `.reproit/capsules/<id>/capsule.enc`. The
random local key is `.reproit/capsule.key`. Both are ignored by `reproit init`. A runner receives a
mode-0600 plaintext scratch file that is removed by an RAII guard on success, error, or
cancellation.

The local key rotates automatically after 90 days. Rotation stages every new ciphertext first and
retains rollback copies until the atomic key swap succeeds. `REPROIT_CAPSULE_KEY_ROTATION_DAYS`
changes the interval; `0` rotates on the next write.

Referenced findings and kept repros pin their capsule indefinitely. Abandoned candidate capsules are
pruned after 30 days or beyond 200 retained candidates. `REPROIT_CAPSULE_RETENTION_DAYS` and
`REPROIT_CAPSULE_MAX_UNREFERENCED` override those local limits.

## Privacy

Capture adapters redact authorization/cookie headers and recursively replace credential and common
identity fields with typed structural placeholders before persistence. The Rust host repeats
redaction defensively and records every path changed in the manifest. Non-JSON bodies are
represented by structural length, not content. A redacted capsule must independently reproduce;
otherwise it stays quarantined.

## Framework adapter protocol

Playwright web capture is automatic. Framework SDK transports use the same two diagnostic markers:

```text
REPROIT:CAPABILITIES {"http":{"status":"captured"},"http_replay":{"status":"captured"}}
REPROIT:EXCHANGE { ... canonical Exchange JSON ... }
```

The host validates and re-redacts exchange markers. Every runner receives `REPROIT_NETWORK_FILE`,
`REPROIT_CAPABILITIES_FILE`, and, during replay, `REPROIT_CAPSULE`. Missing required capture or
replay capabilities prevent a confirmed verdict. `reproit doctor` reports whether capture is
automatic or an SDK transport hook is required.

## Current adapters

- Web/Playwright: automatic cross-origin fetch/XHR capture and fail-closed fulfillment, plus ordered
  JSON WebSocket frames and JSON SSE streams. Opaque streaming frames downgrade capability rather
  than persisting unsafe content.
- Electron: automatic renderer fetch/XHR capture and fail-closed Playwright fulfillment, including
  file-backed applications calling remote APIs.
- React Native: the SDK wraps global `fetch` and direct `XMLHttpRequest`; Appium relays markers from
  logcat/syslog. Autolinked iOS/Android runtime modules expose Appium's guarded capsule to
  JavaScript before the SDK installs the fail-closed wrapper.
- Flutter: `ReproIt.run` installs a zone-wide `package:http` client; the orchestrator embeds the
  guarded capsule into the simulator build and Flutter logs carry the universal markers back to the
  host.
- Native iOS/macOS: the Swift SDK automatically registers a Foundation `URLProtocol` only during a
  ReproIt causal run. Appium injects the guarded capsule and actor into the app process; Foundation
  requests are captured or fulfilled fail-closed without application-specific harness code.
- Terminal apps: the TypeScript, Go, Python, and Rust SDKs ship causal transport adapters.
  TypeScript, Go, and Python install them from the normal Reporter constructor; Rust exposes a
  library-neutral `CausalTransport` because the Rust ecosystem has no single global HTTP client.
- Native Linux: the Reporter installs a process-wide `urllib` adapter during a ReproIt run and
  restores it on disposal; GTK and Qt share this transport.
- Native Android: `ReproIt.causalHttp` is dependency-free. Appium injects the capsule and drives an
  actor-local system-property action clock.
- Windows: `ReproItClient.CreateHttpClient()` installs the .NET causal handler.
- Tauri: `tauri-plugin-reproit` installs its fetch and `XMLHttpRequest` transports through a
  document-start initialization script, before application HTML is parsed. Rust commands read the
  action clock and append validated redacted exchanges.
