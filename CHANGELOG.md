# Changelog

All notable user-facing changes are recorded here. ReproIt follows semantic
versioning for the stable CLI surface, saved repro contract, wire protocol, and
versioned SDK source APIs documented in `docs/stability.md`.

## 1.0.0 - 2026-07-24

1.0 is tiered. The web (Chromium) UI workflow is the stable surface under the
1.x compatibility promise. The backend pillar and the non-Chromium UI adapters
ship in this release as opt-in previews outside that promise; see
`docs/stability.md` and `docs/compatibility.md`.

### Added (stable)

- One CLI workflow for web (Chromium) UI apps: scan, deep interaction fuzzing,
  confirmed finding replay, minimization, saved regression suites, and evidence
  recording.
- Version 1 event batches shared by the CLI, runners, and Cloud.
- Checksummed CLI and web SDK archives with installer smoke tests.
- Hosted Cloud ingest, account, project, capture, replay-package, and CLI
  production-loop validation.
- Independent Chromium application evidence against fixed public VERT and
  Slidev issues.

### Added (preview, opt-in, outside the 1.x compatibility promise)

- Non-Chromium UI adapters: Firefox, WebKit, mobile, desktop, terminal,
  Electron, and Tauri, each with open field-evidence gates (see
  `docs/compatibility.md`).
- Backend contract oracles: findings from the backend evaluate family carry a
  per-check `backend-*` oracle id (for example `backend-data-loss`), registered
  in `oracle-registry.json` with a confidence tier and severity class. Existing
  artifacts stamped with the legacy umbrella id `backend-contract` remain
  readable; scoped protocol and schema checks still report under it.
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
  `reproit check <capture.json>` and `reproit debug replay-capture`; and the
  `reproit repro list` alias. See `docs/backend-quickstart.md`.

### Stability contract

- Existing 1.x `reproit.yaml` files, saved repros, event batches, and the command
  behavior named in `docs/stability.md` remain compatible throughout 1.x unless
  a security fix requires a narrowly documented exception.
- The preview surfaces above (backend and non-Chromium UI adapters) are opt-in
  and outside the 1.x compatibility promise: their contracts may change before
  they are promoted. They are identified as preview where documented.
