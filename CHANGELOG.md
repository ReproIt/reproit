# Changelog

All notable user-facing changes are recorded here. ReproIt follows semantic
versioning for the CLI, saved repro contract, wire protocol, and published SDKs.

## 1.0.0 - Unreleased

### Added

- One CLI workflow across web, mobile, desktop, terminal, Electron, Tauri,
  Dear ImGui, and Clay targets.
- Confirmed finding replay, minimization, saved regression suites, evidence
  recording, and production bug replay.
- Version 1 event batches shared by the CLI, runners, Cloud, and production SDKs.
- Checksummed native archives and installer smoke tests for supported release
  platforms.

### Stability contract

- Existing 1.x `reproit.yaml` files, saved repros, event batches, and documented
  command behavior remain compatible throughout 1.x unless a security fix
  requires a narrowly documented exception.
- Experimental specialist or backend-contract features are outside the stable
  API and are identified as experimental where documented.
