# Changelog

All notable user-facing changes are recorded here. Repro It follows semantic
versioning for the CLI surface, saved repro contract, wire protocol, and
versioned SDK source APIs documented in `docs/compatibility.md`.

## Upgrading

### Upgrade within 1.x

1. Commit or back up `reproit.yaml`, `.reproit/repros`, and authored journeys.
2. Read the entries below for behavior and prerequisite changes.
3. Run `reproit update`, or install an explicit immutable version.
4. Run `reproit doctor` in each configured application.
5. Run `reproit check` before accepting the upgrade in CI.

Repro It refreshes regenerable map state when the CLI or application inputs
change. Do not delete saved repros to resolve an upgrade problem. Report a
compatibility defect with the prior and new CLI versions.

### Pinning

CI should pin an immutable `v1.x.y` release. The `v1` GitHub Action tag moves to
the latest validated 1.x release and is intended for teams that deliberately
accept compatible updates.

SDK source dependencies must use an immutable `v1.x.y` tag. Keep the CLI and SDK
on the same minor version when practical; the version 1 wire protocol permits
independent patch updates.

## 1.0.0 - 2026-08-04

### Hardened: version 1 contract

- Added explicit `schemaVersion: 1` configuration output while preserving
  reads of existing unversioned version 1 files and rejecting future versions
  with a migration repair.
- Separated `reproit-protocol` versioning from CLI and Cloud releases, with a
  machine-readable wire ledger, conformance checks, and package validation.
- Strengthened occurrence identities, canonical timestamp handling, and
  bounded environment dimensions without making legacy version 1 evidence
  unreadable.
- Removed monitoring-vendor-specific integration contracts. Imported
  diagnostics can provide context but cannot grant replay authority.
- Added architecture, stable-toolchain, dogfood, release, and support
  evidence gates for the 1.0 promise.
- Declared 21 atomic targets, each with native gates and a generated
  evidence record.

See [the 1.0 migration guide](docs/1.0-migration.md).

### Added: reproducible debugging

- Added provider-neutral debug execution for local processes, containers,
  simulators, physical devices, and local virtual machines.
- Added reproducible Docker Compose cells with bounded readiness, reset,
  containment, cleanup, and execution receipts.
- Added source-neutral debugger profiles plus a VS Code integration that opens
  a prepared replay session without tying the CLI vocabulary to a framework.
- Added platform, clock, randomness, and readiness evidence to
  capture compilation and Cloud replay packages.
- Added explicit explanations for unavailable capabilities so a replay reaches
  an honest reproduced, clean, or not-reproducible outcome.

### Improved: backend evidence

- Added automatic effect-boundary instrumentation and backend contract support
  for richer production-to-local replay evidence.
- Added Cloud storage, retention, and replay-package joining for platform and
  execution evidence.
- Expanded doctor and capability coverage reporting for debug providers and
  capture completeness.

Every surface in this release is covered by the 1.x compatibility promise; see
`docs/compatibility.md`.

### Added: core workflow

- One CLI workflow for web (Chromium) UI apps: scan, deep interaction fuzzing,
  confirmed finding replay, minimization, saved regression suites, and evidence
  recording.
- Version 1 event batches shared by the CLI, runners, and Cloud.
- Checksummed CLI and web SDK archives with installer smoke tests.
- Hosted Cloud ingest, account, project, capture, replay-package, and CLI
  production-loop validation.
- Independent Chromium application evidence against fixed public VERT and
  Slidev issues.

### Added: backend and additional UI adapters

- Non-Chromium UI adapters: Firefox, WebKit, mobile, desktop, terminal,
  Electron, and Tauri, each with open field-evidence gates (see
  `docs/compatibility.md`).
- Backend contract oracles: findings from the backend evaluate family carry a
  per-check `backend-*` oracle id (for example `backend-data-loss`), registered
  in `oracle-registry.json` with a confidence tier and severity class. Scoped
  protocol and schema checks without a dedicated row report as `contract`.
- Backend production capture mode: error-triggered (and optionally sampled)
  capture of the full start/effects/return operation sequence, shipped as
  version 1 event batches tagged `backend-server-error`, with hard bounds on
  queue depth, batch size, payload size, and retries. Capture never blocks or
  fails the host application. The capture mechanism is proven on an owned
  fixture; no third-party production case is claimed yet.
- Backend SDKs, wire-compatible with the Rust reference adapter and pinned by
  shared event-batch and oracle-tagging contract tests: Rust (with feature-
  gated axum and actix-web middleware), Node (Express, Fastify), Python (ASGI
  for FastAPI/Starlette), Go (net/http, Fiber), Ruby (Rack), PHP (PSR-15 and a
  vanilla adapter), Java (jakarta servlet filter), and .NET (ASP.NET Core). All
  are versioned `0.0.0` and are not yet published to package registries or
  install-smoked.
- Backend workflow: `reproit init` framework detection, schema-URL init, and
  `--learn` draft-schema derivation; a first-class `--target` with precedence
  `--target` > `REPROIT_BACKEND_URL` > the new `backend.target` config field >
  the schema `servers` entry, and positional-URL routing to the backend target;
  `reproit doctor` backend checks with an adapter-tier report; scan/fuzz that
  state their verdict tier (`effect-grounded` vs `black-box`) and send
  wrong-typed input probes (a repeatable 5xx on a contract-invalid request
  surfaces as `backend-server-error`, a 2xx acceptance as
  `backend-accepted-invalid-input`); `reproit inspect` for backend findings,
  capture-bearing buckets, and capture files (live effect-diff or `--offline`);
  `reproit check <capture.json>` and `reproit debug replay-capture`. The current
  workflow is documented in `docs/cli.md`.

### Stability contract

- Existing 1.x `reproit.yaml` files, saved repros, event batches, and the command
  behavior named in `docs/compatibility.md` remain compatible throughout 1.x unless
  a security fix requires a narrowly documented exception.
- The backend and UI adapters above carry the same promise: their contracts are
  covered throughout 1.x on the same terms as every other surface.
