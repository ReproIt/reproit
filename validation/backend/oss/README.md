# OSS backend contract dogfood

This opt-in release gate runs disposable, pinned open-source services and feeds
their authoritative schemas plus actual responses through Reproit's real backend
schema importer and evaluator. It never calls or mutates a production service.

The corpus covers:

- Swagger Petstore 3: OpenAPI 3, JSON and XML media types, request bodies,
  path/query/header parameters, arrays, maps, references, enums, and a local
  create/read/delete lifecycle.
- Countries GraphQL: aliases, fragments, variables, nested lists, nullable
  values, validation errors, input objects, and partial selections.
- GraphQL.js: interface and union fragments, mixed lists, aliases, nullability,
  and resolver errors against a local fixture using the maintained GraphQL.js
  runtime installed by Countries.
- grpc-go: the official Hello World server/client and its generated protobuf
  descriptor, plus canonical protojson 64-bit scalar fixtures.

Each clean case must produce zero findings. Controlled response/input mutations
must produce exactly one finding of the expected oracle. A clean finding is a
stop-ship false positive.

Requirements: Docker, Git, Node/npm, Go, Rust/Cargo, curl, and jq.

```sh
./validation/backend/oss/run.sh
```

The service revisions and Docker digest are pinned in `run.sh`. Captured data is
written to a temporary directory and removed on exit. This gate intentionally
stays separate from the fast hermetic backend unit tests because it downloads
and boots real third-party projects.
