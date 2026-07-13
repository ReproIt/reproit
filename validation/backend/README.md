# Experimental backend structural validation

This gate is intentionally separate from the public product documentation.
It validates the language-independent backend event protocol, schema importers,
function and effect graph, authoritative oracle boundary, frozen identities,
and exact replay integration.

Backend adapters emit one JSON object per line. Verdict evaluation consumes the
object after the `REPROIT:BACKEND ` marker in the runner log so it remains
attributed to the exact fuzz seed. A `backend-*.jsonl` file in the run directory
is also retained in the encrypted capsule as supporting evidence.

For web applications, enabling backend contracts makes the runner add
`x-reproit-trace`, `x-reproit-actor`, and `x-reproit-action` to first-party API
requests. Instrumented services return a base64url JSON event array in
`x-reproit-events`. The runner accepts only events bound to the exact request
trace. `sdk-node.mjs` is the zero-dependency reference adapter used by this gate.
It remains internal until the backend product passes the full release gate.
Sibling API hosts must be listed in `backend.origins`; no correlation header is
sent to any other origin. Those API hosts must allow the three request headers
through their CORS policy. The response evidence header is read by the browser
driver, so application JavaScript does not need access to it.

## Reproit Cloud dogfood boundary

Reproit Cloud owns a checked-in OpenAPI 3.1 artifact for five dogfooded JSON
operations. One Cloud endpoint registry supplies the Axum route aliases and
trace operation ids, and refuses to emit the artifact if its method, path, or
operation id drifts. `cloud-openapi.yaml` and `cloud-contract.yaml` are pinned
CLI evaluator fixtures; `cloud-schema-parity.test.mjs` checks the sibling Cloud
artifact, registry, actual route registration, and critical response keys.

Cloud's `validation/backend-contract-live.sh` starts disposable Postgres and an
opt-in local Cloud process, then exercises actual signup, create-project,
get-me, ingest, and replay-result handlers. It feeds the captured adapter events
back through the CLI schema importer/evaluator and requires a clean result. The
script never targets or mutates a production Cloud instance.

The dogfood boundary remains explicit:

- `sdk/reproit-backend-rs` is the canonical bounded Rust adapter. Cloud vendors
  that source exactly, enforces source parity, and wires it into the five routes
  only when `REPROIT_EXPERIMENTAL_BACKEND_CONTRACTS=1`; individual requests must
  also carry `x-reproit-trace`. The default server path remains inert.
- Cloud currently records request/response shape and status, but does not claim
  handler-observed tenant or persistent-effect completeness. Every captured
  return therefore carries `effectsComplete:false`; missing-effect and tenant
  findings stay silent. The adapter retains the complete tenant, effect,
  idempotency, and GraphQL-selection API for later handler-level evidence.
- Secret fields retain only redacted type and length/item-count metadata. Those
  facts safely enforce type and size constraints; pattern, enum, format, nested
  object shape, and exact identity are deliberately not claimed after redaction.

OpenAPI imports path/query/header parameters only when their serialization has
an exact decoded representation. Cookies, deep-object parameters, XML,
multipart, and binary content are ignored. JSON and vendor `+json` are imported;
plain text is imported only as a string and form-urlencoded only as an object.
GraphQL response presence becomes authoritative only when an adapter supplies a
normalized selection mapping (`schemaPath` to alias-aware `responsePath`) for
the exact invocation.

The minimum invocation is:

```json
{"sequence":1,"traceId":"trace-a","spanId":"span-a","operation":"createMessage","actor":"alice","tenant":"team-a","kind":"start","input":{"body":"hello"}}
{"sequence":2,"traceId":"trace-a","spanId":"span-a","operation":"createMessage","actor":"alice","tenant":"team-a","kind":"effect","effect":"write","resource":"messages","key":"m1","effectTenant":"team-a","after":{"id":"m1"}}
{"sequence":3,"traceId":"trace-a","spanId":"span-a","operation":"createMessage","actor":"alice","tenant":"team-a","kind":"return","status":201,"success":true,"effectsComplete":true,"output":{"id":"m1"}}
```

`effectsComplete` may be true only when the adapter observed every effect in
the operation. Missing-effect contracts are silent without that proof. Positive
evidence such as a duplicate write or cross-tenant write remains actionable.

Run `./run.sh` on macOS. Run `./run-linux-docker.sh` for a clean Linux x86
gate. Run `run.ps1` in PowerShell on Windows. `run-windows-remote.sh` transfers
the source gate through the configured lab hosts and runs it on native Windows.
