# Experimental backend structural validation

The canonical proof model and concise backend oracle catalog live in
[`docs/oracles.md`](../../docs/oracles.md#backend-oracles). This page covers the experimental event
protocol, configuration, and validation gates.

This implementation gate is separate from the public quick start because backend support remains
experimental. It validates the language-independent event protocol, schema importers, function and
effect graph, authoritative oracle boundary, frozen identities, and exact replay integration.

Backend adapters emit one JSON object per line. Verdict evaluation consumes the object after the
`REPROIT:BACKEND` marker in the runner log so it remains attributed to the exact fuzz seed. A
`backend-*.jsonl` file in the run directory is also retained in the encrypted capsule as supporting
evidence.

For web applications, enabling backend contracts makes the runner add `x-reproit-trace`,
`x-reproit-actor`, `x-reproit-action`, and optional bounded `x-reproit-build` and
`x-reproit-config-contract` identities to first-party API requests. Instrumented services return a
base64url JSON event array in `x-reproit-events`. The runner accepts only events bound to the exact
request trace. `sdk-node.mjs` is the zero-dependency reference adapter used by this gate. It remains
internal until the backend product passes the full release gate. Sibling API hosts must be listed in
`backend.origins`; no correlation header is sent to any other origin. Those API hosts must allow the
correlation headers through their CORS policy. The response evidence header is read by the browser
driver, so application JavaScript does not need access to it.

## ReproIt Cloud dogfood boundary

ReproIt Cloud owns a checked-in OpenAPI 3.1 artifact for five dogfooded JSON operations. One Cloud
endpoint registry supplies the Axum route aliases and trace operation ids, and refuses to emit the
artifact if its method, path, or operation id drifts. `cloud-openapi.yaml` and `cloud-contract.yaml`
are pinned CLI evaluator fixtures; `cloud-schema-parity.test.mjs` checks the sibling Cloud artifact,
registry, actual route registration, and critical response keys.

Cloud's `validation/backend-contract-live.sh` starts disposable Postgres and an opt-in local Cloud
process, then exercises actual signup, create-project, get-me, ingest, and replay-result handlers.
It feeds the captured adapter events back through the CLI schema importer/evaluator and requires a
clean result. The script never targets or mutates a production Cloud instance.

The dogfood boundary remains explicit:

- `sdk/reproit-backend-rs` is the canonical bounded Rust adapter. Cloud vendors that source exactly,
  enforces source parity, and wires it into the five routes only when
  `REPROIT_EXPERIMENTAL_BACKEND_CONTRACTS=1`; individual requests must also carry `x-reproit-trace`.
  The default server path remains inert.
- Cloud currently records request/response shape and status, but does not claim handler-observed
  tenant or persistent-effect completeness. Every captured return therefore carries
  `effectsComplete:false`; missing-effect and tenant findings stay silent. The adapter retains the
  complete tenant, effect, idempotency, and GraphQL-selection API for later handler-level evidence.
- Secret fields retain only redacted type and length/item-count metadata. Those facts safely enforce
  type and size constraints; pattern, enum, format, nested object shape, and exact identity are
  deliberately not claimed after redaction.

OpenAPI imports path/query/header parameters only when their serialization has an exact decoded
representation. Cookies, deep-object parameters, XML, multipart, and binary content are ignored.
JSON and vendor `+json` are imported; plain text is imported only as a string and form-urlencoded
only as an object. GraphQL response presence becomes authoritative only when an adapter supplies a
normalized selection mapping (`schemaPath` to alias-aware `responsePath`) for the exact invocation.

The minimum invocation is:

```json
{
  "sequence": 1,
  "traceId": "trace-a",
  "spanId": "span-a",
  "operation": "createMessage",
  "actor": "alice",
  "tenant": "team-a",
  "kind": "start",
  "input": { "body": "hello" }
}
```

```json
{
  "sequence": 2,
  "traceId": "trace-a",
  "spanId": "span-a",
  "operation": "createMessage",
  "actor": "alice",
  "tenant": "team-a",
  "kind": "effect",
  "effect": "write",
  "resource": "messages",
  "key": "m1",
  "effectTenant": "team-a",
  "after": { "id": "m1" }
}
```

```json
{
  "sequence": 3,
  "traceId": "trace-a",
  "spanId": "span-a",
  "operation": "createMessage",
  "actor": "alice",
  "tenant": "team-a",
  "kind": "return",
  "status": 201,
  "success": true,
  "effectsComplete": true,
  "output": { "id": "m1" }
}
```

`cli-e2e/run.sh` boots a disposable instrumented HTTP fixture and runs the real `reproit scan`
command. The gate requires the CLI to report the schema-backed response violation and exit with
regression status. This keeps the public scan path, not only the model evaluator, covered end to
end.

An OpenAPI file is also a direct CLI target. `reproit scan openapi.yaml` calls read-only GET
operations without launching a browser. `reproit fuzz
openapi.yaml` orders creates before reads,
harvests structural resource IDs, and feeds those values into later operations. Confirmed findings
save their setup requests and failing request together, so `reproit fnd_...` can rebuild the state
it needs. Set `REPROIT_BACKEND_RESET_URL` to a same-origin disposable reset endpoint when replay
requires a clean service. Remote mutating fuzzing requires `--yes`; scan never performs mutations.

Schema checks run before network execution. Duplicate OpenAPI parameters are confirmed from the
document alone and can be replayed with `reproit fnd_...` even when the schema has no server URL. A
parameter is duplicated only when the same `(name, in)` pair appears twice in one Path Item or
Operation parameter list. An Operation overriding a Path Item parameter is valid. Unresolved,
external, or cyclic references produce no finding.

Transport checks consume complete protocol evidence. Redirect validation uses the captured redirect
and next request. Byte-range validation additionally requires exact pinned representation bytes.
WebSocket validation requires an authored route and the applicable principal, message, or close-code
rules. Incomplete evidence remains silent.

Local multi-file OpenAPI references are resolved hermetically. Remote references are rejected until
pinned locally. Authored backend invariants can enforce numeric output ranges, equality between
input and output paths, and unique output collections. They remain silent unless a successful
runtime witness exists, and removing the authored invariant removes the oracle.

Backend-only projects need no UI scaffold. `reproit init` detects a conventional OpenAPI, GraphQL
introspection, or protobuf descriptor filename and writes the smallest backend-only `reproit.yaml`.
Bare `reproit scan` and `reproit fuzz` then resolve that schema automatically. Framework names are
never part of the runtime contract.

Financial and deployment invariants use structural paths and values:

```yaml
backend:
  enabled: true
  schemas: [openapi.yaml]
  invariants:
    - unique: order.id
    - idempotent: submitOrder
    - conserved: ledger.debits == ledger.credits
    - bounded: account.exposure <= account.limit
    - transition: pending -> accepted | rejected
  fleet:
    same_build: true
    same_config_contract: true
```

`REPROIT_BUILD` and `REPROIT_CONFIG_CONTRACT` attach bounded deployment identities to correlated
events. Fleet rules report only when one captured evidence set contains conflicting declared
identities. Values such as account contents, credentials, email addresses, and tokens remain
structurally redacted.

`effectsComplete` may be true only when the adapter observed every effect in the operation.
Missing-effect contracts are silent without that proof. Positive evidence such as a duplicate write
or cross-tenant write remains actionable.

## Query semantics

Filter, sort, limit, and pagination checks require an authored contract or an equivalent
schema-owned operation. ReproIt never guesses semantics from names such as `sort`, `limit`, or
`cursor`:

```yaml
backend:
  enabled: true
  invariants:
    - kind: query-semantics
      operation: listOrders
      itemsPath: $.items
      identityPath: $.id
      consistency: strong
      filters:
        - inputPath: $.query.status
          itemPath: $.status
          comparison: equal
      sort:
        itemPath: $.createdAt
        direction: descending
        valueType: string
      limitInputPath: $.query.limit
      pagination:
        cursorInputPath: $.query.cursor
        nextCursorOutputPath: $.nextCursor
        snapshotInputPath: $.query.revision
        referenceOperation: listAllOrders
```

Equality requires every returned scalar at `itemPath` to equal the authored input scalar. Sort
requires every value to have the declared type and uses a total monotonic order. Limit proves only
that the returned item count does not exceed the supplied unsigned integer.

Pagination findings require strong consistency, a shared explicit snapshot or revision, identical
actor and tenant identities, identical non-cursor query input, scalar item identities, and a
complete cursor chain beginning at an explicit null cursor. ReproIt can then prove duplicate
identities or a repeated nonterminal cursor. When `referenceOperation` is declared, it also compares
the complete concatenated pages with a successful reference response for the same snapshot and
authored filter inputs.

An inferred operation, eventual or unspecified consistency, a missing path, an unsupported value
type, a missing snapshot or identity, mixed query inputs, a concurrent or incomplete observation, an
unclosed cursor chain, or an ambiguous reference response produces no finding. Frozen replay retains
the query contract and requires the same structural fingerprint after shrinking.

## Resource lifecycle consistency

Authored resource contracts let backend fuzzing prove that a successful create, update, or delete
agrees with a later read. They are language and framework independent because operation ids and
structural JSON paths are the complete interface:

```yaml
backend:
  enabled: true
  schemas: [openapi.yaml]
  resources:
    - name: order
      consistency: strong
      create:
        operation: createOrder
        outputIdentityPath: $.id
      read:
        operation: getOrder
        inputIdentityPath: $.path.id
        outputIdentityPath: $.id
        absentStatuses: [404]
      update:
        operation: updateOrder
        inputIdentityPath: $.path.id
      delete:
        operation: deleteOrder
        inputIdentityPath: $.path.id
      fields:
        - createOutputPath: $.status
          updateInputPath: $.body.status
          readOutputPath: $.status
```

`reproit fuzz` exercises and shrinks create/read, create/update/read, and create/delete/read
sequences. Publication requires a same-origin disposable reset endpoint and an exact clean-state
replay of the final structural fingerprint. Removing any required setup step or reducing the failing
request is accepted only when that same fingerprint still reproduces. Generated resource identities
are captured as structural bindings, not frozen values. If reset creates a different id, replay
rebinds that id into later path, query, header, body, GraphQL-variable, or gRPC inputs before
verification.

The oracle is deliberately incomplete. It returns no finding when consistency is unspecified or
eventual, an operation id is missing or ambiguous, an identity path is absent or not a scalar, setup
does not succeed cleanly, an update field cannot be bound, an absence status is not declared, or
clean-state replay changes the result. The initial vertical slice does not poll eventually
consistent services. A future observation-boundary contract can add that without weakening the
current zero false-positive boundary.

Run `./run.sh` on macOS. Run `./run-linux-docker.sh` for a clean Linux x86 gate. Run `run.ps1`
directly in PowerShell on native Windows.
