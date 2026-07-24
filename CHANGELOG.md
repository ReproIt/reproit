# Changelog

All notable user-facing changes are recorded here. ReproIt follows semantic
versioning for the stable CLI surface, saved repro contract, wire protocol, and
versioned SDK source APIs documented in `docs/stability.md`.

## Unreleased

### Added

- `reproit inspect` now covers the backend platform: it steps through a backend
  finding, a capture-bearing production bucket, or a captured-production
  payload file one operation at a time, live against the configured target by
  default (with the adapter effect trail and a recorded-vs-live effect diff)
  or offline over the recorded events (`--offline`, or automatically when no
  target is reachable). The verdict comes from the unchanged backend check
  engine over event-sequence prefixes, and the session writes bounded
  `inspect-transcript.md` / `inspect-transcript.json` evidence without ever
  touching saved guards.
- `reproit check <capture.json>` accepts a captured-production backend payload
  file (the `reproit-backend-capture` JSON that `debug replay-capture` takes)
  and re-evaluates it under check's verdict contract: still reproduces is a
  real regression (outcome `fail`, exit 1), no longer reproduces is `pass`
  (exit 0), with the matching `--json` outcome object. A saved repro or
  finding whose name matches the file path still resolves as the saved
  artifact. `debug replay-capture` is unchanged and shares the same
  re-evaluation.
- `reproit repro list`, an alias of the top-level `reproit repros` (which
  remains the primary form), so the `repro` subcommand family can also list
  the saved repros it operates on.
- Backend fuzz now sends wrong-typed input probes: each declared body field of
  an HTTP operation is sent once with a wrong JSON type (present-but-wrong
  optional fields included), deterministic per seed and capped at 8 probes per
  operation. A 4xx rejection stays silent; a repeatable 5xx surfaces as
  `backend-server-error` (the contract requires a 4xx rejection, and the
  finding records the probed mismatch), and a 2xx acceptance surfaces as
  `backend-accepted-invalid-input`. Probe findings replay as a single
  request, so discovery confirmation and `reproit check` are the same check.

## 1.0.0 - 2026-07-24

### Added

- Backend contract checks are first-class oracle categories: findings from the
  backend evaluate family carry a per-check `backend-*` oracle id (for example
  `backend-data-loss`), registered in `oracle-registry.json` with a confidence
  tier and severity class. Existing artifacts stamped with the legacy umbrella
  id `backend-contract` remain readable; scoped protocol and schema checks
  still report under it.
- Production capture mode in the backend SDKs: error-triggered (and optionally
  sampled) capture of the full start/effects/return operation sequence, shipped
  as version 1 event batches tagged `backend-server-error`, with hard bounds on
  queue depth, batch size, payload size, and retries. Capture never blocks or
  fails the host application.
- `reproit debug replay-capture` deterministically re-evaluates a captured
  production sequence locally with the backend oracles.
- Node backend SDK (`reproit-backend-node`, Express middleware and Fastify
  plugin) and Python backend SDK (`reproit-backend-py`, ASGI middleware for
  FastAPI/Starlette), wire-compatible with the Rust reference adapter and
  pinned by shared event-batch and oracle-tagging contract tests.
- Go backend SDK (`reproit-backend-go`, net/http middleware and a Fiber v2
  adapter module) held to the same shared contract tests, and first-class
  feature-gated axum and actix-web middleware in the Rust backend adapter.
- Ruby (`reproit-backend-rb`, Rack middleware for Rails and Sinatra), PHP
  (`reproit-backend-php`, PSR-15 middleware and a vanilla adapter), Java
  (`reproit-backend-java`, jakarta servlet filter covering Spring Boot), and
  .NET (`reproit-backend-dotnet`, ASP.NET Core middleware) backend SDKs, all
  wire-compatible with the reference adapter and held to the shared
  event-batch and oracle-tagging contract tests.
- One CLI workflow across web, mobile, desktop, terminal, Electron, and Tauri
  targets.
- Confirmed finding replay, minimization, saved regression suites, evidence
  recording, and production bug replay.
- Version 1 event batches shared by the CLI, runners, Cloud, and production SDKs.
- Checksummed CLI and SDK archives, plus installer smoke tests, for every 1.0
  platform.
- Hosted Cloud ingest, account, project, capture, replay-package, and CLI
  production-loop validation.
- Independent Chromium application evidence against fixed public VERT and
  Slidev issues.

### Stability contract

- Existing 1.x `reproit.yaml` files, saved repros, event batches, and the command
  behavior named in `docs/stability.md` remain compatible throughout 1.x unless
  a security fix requires a narrowly documented exception.
- Backend contract oracles, the backend trace adapters, and capture mode are
  part of the stable surface. Experimental specialist features remain outside
  the stable API and are identified as experimental where documented.
