# ReproIt backend adapter for Rust

This crate is an internal validation surface, not a published compatibility API. It is inactive
unless a trusted request contains `x-reproit-trace`.

Framework integrations pass their header lookup into `TraceContext::from_header_fn`, start an
operation with `BackendTrace::begin`, record only effects actually observed by the adapter, then
call `finish` and return `header()` as `x-reproit-events`. Set `effects_complete` only when the
adapter observed every persistent effect in the operation. Tenant and resource identifiers must be
non-secret structural identifiers.

The adapter enforces bounded identifiers, 256 events, a 60 KB encoded header, typed effects, one
return, no effects after return, hashed idempotency identity, and recursive structural redaction.
GraphQL callers may attach parser-produced `Selection` mappings; never infer selections from
response content.
