# API and agents

Everything the dashboard does is an HTTP call you can make yourself. This page is the auth model
and the entry points; the router is the authority on the full route list.

## Authentication

| key | header | what it can do |
| --- | --- | --- |
| `pk_live_...` publishable | `Authorization: Bearer pk_live_...` | ingest only, write-only. Safe to ship inside an application. |
| `sk_live_...` secret | `Authorization: Bearer sk_live_...` | every `/v1/*` management route. Server-side and CI only. |

Session tokens and API tokens are stored hashed. A publishable key presented to a management route
is rejected, not downgraded.

## Where an agent starts

```http
GET /v1/work
```

The cross-project queue: open, ranked bugs across every project the key can see, in one call.
Filter with `?assignee=`, `?unassigned=true`, `?limit=`. It is a read. Claiming a bug is a POST to
that bucket's workflow route, so two agents cannot silently pick up the same work.

The intended loop is: `GET /v1/work` to choose, claim it, reproduce it locally with
`reproit occ_...`, fix, then `reproit check` before you hand it back.

## The routes that matter

```text
GET  /v1/work                                     ranked cross-project queue
GET  /v1/apps/{app}/buckets                       one project's bugs, impact ranked
GET  /v1/apps/{app}/buckets/{bucket}              one bug, with its occurrences
GET  /v1/apps/{app}/buckets/{bucket}/evidence     stored evidence
POST /v1/apps/{app}/buckets/{bucket}/reproduce    request a reproduction run
GET  /v1/occurrences/{occurrence}                 one occurrence, the replay payload
GET  /v1/apps/{app}/export                        portability export
POST /v1/events                                   SDK ingest (publishable key)
POST /v1/capture-batches                          source-neutral capture ingest
```

Ingest accepts two shapes. New SDKs send the source-neutral capture contract to
`/v1/capture-batches`. Existing SDKs send a validated `reproit-protocol` version 1 event batch to
`/v1/events`, which Cloud translates into the same occurrence model.

## Error behavior worth relying on

Ingest is oracle-gated: an error event without a present, well-formed oracle id is dropped and
counted as `droppedUntagged` rather than accepted as an untyped error. An unknown but well-formed
oracle id is accepted, because degrading gracefully beats losing a finding.

Ingest also verifies the SDK's redaction claim and rejects a batch whose claim does not hold. A
rejected batch is a bug in the sender, not a transient failure, so retrying it unchanged will fail
the same way.

## MCP

`reproit mcp` exposes this loop to a coding agent as tools over stdio, including the cross-project
queue, reproduction, and triage. `reproit skills` installs the playbook that teaches an agent to
drive it.
