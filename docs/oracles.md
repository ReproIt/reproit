# Oracle reference

An oracle is a rule ReproIt uses to decide whether an observation is a bug. ReproIt separates
finding unusual behavior from proving incorrect behavior. This is the boundary that keeps normal
product behavior out of the confirmed bug list.

## Evidence evaluation and reproduction are separate

An oracle evaluates one rule against one authoritative evidence channel. These are evidence
outcomes, not finding or reproduction statuses:

| Outcome     | Meaning                                                       |
| ----------- | ------------------------------------------------------------- |
| `VIOLATION` | Authoritative evidence violates an exact rule.                |
| `SATISFIED` | The evidence proves that exact rule currently holds.          |
| `ABSTAIN`   | Evidence cannot support either claim, so Reproit stays silent. |

A violation is still not public until a clean replay returns the same exact identity. Reproduction
has its own statuses: `REPRODUCED`, `NOT_REPRODUCED`, `FLAKY`, `STALE`, and `COULD_NOT_REPLAY`.
Only `REPRODUCED` promotes a discovered violation into a confirmed finding. `ABSTAIN` is not success
or failure and never becomes a finding. A failing test, unusual screenshot, timing spike, or
different implementation is not automatically a bug.

## What runs by default

The default confirmed set is intentionally small:

| Oracle               | What proves the bug                                                                                                                 |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `crash`              | An uncaught exception, fatal assertion, signal, or native crash occurred.                                                           |
| `detached-indicator` | An application-declared indicator escaped its owner/container relationship in two settled samples.                                  |
| `contract`           | An application-declared structural or temporal contract failed with the same contract identity and violation fingerprint on replay. |

Other detectors are specialist observations or require explicit configuration. They can be selected
with `--only`, but selecting one does not make a heuristic authoritative:

```sh
reproit fuzz --only crash,jank
reproit fuzz --no visual,occlusion
```

## UI oracles

The IDs below are the canonical CLI and Cloud categories. Confidence describes what a result means,
not how interesting it looks.

| ID                   | Confidence            | Detects                                                                                | Primary support                                                               |
| -------------------- | --------------------- | -------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| `unclassified`       | telemetry only        | An unregistered or newer marker                                                        | all, never confirmed                                                          |
| `crash`              | confirmed             | Unhandled exceptions and process crashes                                               | all runners                                                                   |
| `detached-indicator` | declared proof        | An opted-in badge or indicator detached from its owner                                 | web, React Native, Flutter, iOS, Android                                      |
| `accessibility-state` | experimental specialist | Native checked/selected/expanded state contradicts the accessibility tree; explicit ARIA overrides and disabled-state parity abstain without a third authority | web |
| `contract`           | declared proof        | A failed structural or temporal contract                                               | every instrumented SDK                                                        |
| `invariant`          | declared proof        | An application predicate returned false or threw                                       | every SDK with a state hook                                                   |
| `visual`             | baseline proof        | Pixels differ from an approved, pinned baseline beyond its tolerance                   | screenshot-capable runners                                                    |
| `jank`               | environment-dependent | Dropped or excessively late frames                                                     | browser, Electron, Tauri, Android, Flutter simulator, instrumented ImGui/Clay |
| `leak`               | environment-dependent | Retained memory grows across a repeated workload                                       | precise heap or attributable process sampling                                 |
| `flicker`            | environment-dependent | A presented frame diverges and then resolves                                           | runners with a frame or DOM presentation stream                               |
| `divergence`         | specialist            | The same flow differs across targets or engines                                        | multi-target runs                                                             |
| `content-bug`        | heuristic             | Visible stringify or template artifacts such as `[object Object]`                      | DOM, accessibility labels, TUI grid, instrumented labels                      |
| `hang`               | environment-dependent | An action makes no progress beyond a high threshold                                    | runners with an attributable progress signal                                  |
| `occlusion`          | heuristic             | A visible control's hit target is covered by a foreign element                         | geometry-capable UI runners                                                   |
| `choice-anomaly`     | heuristic             | One sibling choice changes global layout unlike the others                             | browser, Electron, Tauri                                                      |
| `broken-route`       | policy-dependent      | A real document navigation returns HTTP 404 or 410                                     | web and HTTP-backed Electron                                                  |
| `security`           | specialist            | Deterministic client markup hazards such as reverse tabnabbing or mixed content        | web                                                                           |
| `stuck-keyboard`     | environment-dependent | The soft keyboard remains visible without an editable focus owner                      | native mobile                                                                 |
| `duplicate-submit`   | specialist            | An opt-in double activation causes the same first-party mutation twice                 | web                                                                           |
| `focus-loss`         | specialist            | An already-focused control survives an action but keyboard focus falls to the document | web, Electron, Tauri                                                          |
| `blank-screen`       | specialist            | An empty route is corroborated by a first-party exception or renderer crash             | web                                                                           |
| `broken-asset`       | specialist            | A required same-origin image, stylesheet, script, or imported dependency failed        | web                                                                           |
| `zoom-reflow`        | specialist            | At 200% zoom, content requires two-dimensional scrolling or a control collapses        | web                                                                           |
| `rotation`           | environment-dependent | A round trip through orientation permanently changes the screen structure              | mobile and browser-backed surfaces                                            |
| `background-restore` | environment-dependent | Background and foreground changes the restored screen                                  | mobile and browser-backed surfaces                                            |
| `scroll-round-trip`  | environment-dependent | Returning to a pinned list offset yields different structural content                  | web and Flutter                                                               |
| `wakelock`           | environment-dependent | An Android wakelock remains held after leaving its owning screen                       | Android                                                                       |
| `safe-area`          | environment-dependent | An interactive control intersects an authoritative device inset                        | native mobile                                                                 |
| `permission-walk`    | environment-dependent | A controlled permission denial leaves no working forward exit                          | native mobile                                                                 |

Heuristic and environment-dependent categories are not promoted into confirmed bugs merely because
they repeat. Repetition proves repeatability, not product intent. `scan` still reports every enabled
state-present oracle whose predicate held and preserves this classification in its output; the
classification is policy metadata, not a reason to discard the finding.

### Structural contract identities

Several exact contracts are reported through the top-level `contract` category and retain a more
specific identity:

| Identity                             | Proof                                                                                                                           |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------- |
| `focused-input-obscured:<field>`     | An explicitly focused editable remains fully unusable after the framework-standard reveal request and two settled measurements. |
| `state-preservation:<boundary>:<id>` | An authoritative state token changed across rotation, background/foreground, navigation round trip, or process recreation.      |
| `action-effect:<id>:route`           | The observed route differs from the route declared for the action.                                                              |
| `action-effect:<id>:state`           | The observed application state differs from the declared exact or changed state.                                                |
| `detached-indicator:<id>`            | The declared owner, container, maximum gap, and global rectangles prove detachment.                                             |

These contracts never infer intent from visible language, color, proximity, handler names, or
screenshots. Missing ownership, duplicate identities, animation, unresolved geometry, missing state
samples, or unsupported lifecycle boundaries produce `ABSTAIN`.

### Detached indicator example

Web applications opt in with stable structural IDs:

```html
<nav id="bottom-nav" data-reproit-indicator-container>
  <button id="liked-you" data-reproit-indicator-owner
    data-reproit-indicator-max-gap="8">
    Liked You
  </button>
  <span id="liked-you-badge"
    data-reproit-indicator-for="liked-you"></span>
</nav>
```

React Native, Flutter, iOS, and Android expose the same relationship through their SDKs using stable
keys and global rectangles. Every implementation requires two identical settled samples.

## Backend oracles

Backend support is experimental. A finding requires a schema-owned or authored contract plus a
runtime event correlated to the exact operation. Framework names and function names are not evidence
of intent.

OpenAPI, GraphQL, and protobuf describe shapes. Stronger behavior such as idempotency,
authorization, transactionality, ordering, or consistency must be declared explicitly.

### Request and response

| Finding                        | Proves                                                                                                                               |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------ |
| `openapi-parameter-uniqueness` | One OpenAPI Path Item or Operation declares the same `(name, in)` parameter more than once after local reference resolution.         |
| `server-error`                 | A contract-valid request produced a repeatable 5xx response.                                                                         |
| `response-status`              | A successful result used a status outside the declared success set.                                                                  |
| `accepted-invalid-input`       | A successful operation accepted input outside its declared domain.                                                                   |
| `response-shape`               | A successful response contradicted its schema or closed response contract.                                                           |
| `response-selection`           | A GraphQL response contradicted the exact normalized selection mapping.                                                              |
| `http-byte-range`              | A single byte-range response contradicts an authored exact representation in its status, `Content-Range`, length, or raw body bytes. |
| `http-redirect-transition`     | A captured redirect hop violates the method and body transition required by its HTTP status.                                         |
| `http-response-media-type`     | Exact response bytes use a media type outside an authored allowlist, or declare JSON while containing invalid JSON.                  |
| `http-conditional-cache`       | A matched conditional GET returns a body with 304, contradicts the matched ETag, or reuses a strong ETag for different exact bytes.  |
| `runtime-memory-safety`        | An instrumented build emitted a structured AddressSanitizer, HWASan, or MemorySanitizer diagnosis.                                   |
| `runtime-memory-leak`          | An instrumented build emitted a structured LeakSanitizer diagnosis.                                                                 |
| `runtime-data-race`            | An instrumented build emitted a structured ThreadSanitizer diagnosis.                                                               |
| `runtime-undefined-behavior`   | An instrumented build emitted a structured UndefinedBehaviorSanitizer diagnosis.                                                    |

### WebSockets

| Finding                   | Proves                                                                                       |
| ------------------------- | -------------------------------------------------------------------------------------------- |
| `websocket-authorization` | A principal explicitly declared as allowed or denied received the opposite handshake result. |
| `websocket-message`       | A captured client or server message contradicted every authored schema for that direction.   |
| `websocket-close`         | A captured connection used a close code explicitly forbidden by the contract.                |

OpenAPI parameter uniqueness and HTTP redirect transitions are standards-backed. Byte-range
validation also requires exact authoritative representation bytes. Response media-type checks
require a non-bodyless response, a syntactically valid `Content-Type`, and at least one valid
application-authored allowed media type. Matching JSON types are `SATISFIED` only when the exact
body parses as JSON. HEAD, 1xx, 204, 304, empty bodies, missing or malformed headers, and empty or
invalid allowlists produce `ABSTAIN` for this check.

Conditional-cache checks require two GET exchanges for the same target: an initial 200 response
with one valid ETag, followed by a 200 or 304 request carrying the matching single
`If-None-Match`. Every request header named by `Vary` must match. A valid 304 has no response body
and cannot return a contradictory ETag. A 200 response cannot reuse the same strong ETag and
content encoding for different exact bytes. Compound or malformed validators, `Vary: *`, changed
vary dimensions, incomplete exchanges, and weak tags where byte identity is required produce
`ABSTAIN`, not a clean-byte claim.

These HTTP checks consume captured wire values. The media-type allowlist is application-authored;
the conditional-cache contradictions are standards-backed. Neither depends on Express, FastAPI,
Go, Axum, Spring, ASP.NET, Django, Flask, or any other framework name. WebSocket checks require an
authored route, principal, message, or close-code contract. Missing raw bytes, unresolved
references, unlisted principals, and undeclared message directions produce no finding.

### Effects, tenancy, and resources

| Finding                   | Proves                                                                                          |
| ------------------------- | ----------------------------------------------------------------------------------------------- |
| `read-only-mutation`      | A declared read-only operation performed a durable write or delete.                             |
| `missing-effect`          | Complete effect telemetry omitted a required authored effect.                                   |
| `excess-effect`           | An operation exceeded the declared maximum effect count.                                        |
| `tenant-isolation`        | An operation wrote across its declared tenant boundary.                                         |
| `resource-create-missing` | A strongly consistent resource was absent after a successful create.                            |
| `resource-delete-visible` | A strongly consistent resource remained readable after delete.                                  |
| `resource-identity`       | A read returned a different identity from the one requested.                                    |
| `resource-state`          | A read contradicted an exact field written by create or update.                                 |
| `resource-round-trip`     | A strong write/read pair contradicted an authored exact value, hash, size, or media-type check. |
| `codec-round-trip`        | A successful operation's decoded output contradicted an authored exact projection from its input. |

`codec-round-trip` requires a declared operation contract, at least one authored input/output
projection, a successful correlated invocation, both projected JSON paths, and unredacted values.
Exact JSON equality across every observable projection is `SATISFIED`; an exact mismatch is
`VIOLATION`. Missing operations, inferred authority, unsuccessful or uncorrelated invocations,
missing paths, redaction, and an absence of any complete projection set produce `ABSTAIN`. The
proof applies to any transport or framework that emits the same typed backend events.

### Protocol lifecycle

| Finding                     | Proves                                                                                       |
| --------------------------- | -------------------------------------------------------------------------------------------- |
| `lifecycle-precedence`      | An authored event that must precede another occurred after at least one required later event. |
| `lifecycle-forbid-after`    | An authored event occurred after the exact boundary that forbids it.                          |
| `lifecycle-cardinality`     | A complete scope trace contained fewer or more occurrences than the authored bounds allow.    |

Lifecycle rules use producer-assigned sequence numbers, not wall-clock time or log order. A
complete trace with one stable, non-empty scope kind and scope identity can establish
`SATISFIED` when all valid rules hold. Incomplete traces, mixed or empty scope identities,
duplicate sequence numbers, invalid event names, self-precedence, and contradictory or empty
cardinality bounds produce `ABSTAIN` for the affected contract or rule. These checks model any
framework lifecycle whose adapter emits a complete scoped event trace; method names, callbacks,
framework diagnostics, and timing alone are not authority.

### Queries, invariants, and deployment

| Finding                      | Proves                                                                                                                      |
| ---------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `authored-invariant`         | A declared range, equality, uniqueness, filter, sort, limit, conservation, bound, or transition rule failed.                |
| `query-pagination`           | A complete pinned cursor chain repeated an identity or repeated a nonterminal cursor without progress.                      |
| `query-pagination-reference` | Complete pinned pages differed from the declared reference operation for the same snapshot.                                 |
| `idempotency`                | Repeating the same authored request and idempotency key changed the declared durable final effect or exact replay response. |
| `fleet-consistency`          | One evidence set mixed declared build or configuration-contract identities.                                                 |

### Authorization, transactions, and concurrency

| Finding                   | Proves                                                                                                         |
| ------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `authorization-matrix`    | A principal explicitly declared as denied received protected data for the same resource identity and snapshot. |
| `transaction-atomicity`   | A controlled failed operation left a declared durable value different from its exact before value.             |
| `concurrent-update`       | Two overlapping updates using the same authored version both committed to the same resource.                   |
| `concurrent-conservation` | Overlapping committed updates contradicted an authored conservation transition.                                |

Backend evaluation returns `ABSTAIN` when it lacks strong consistency, a stable operation or
resource identity, complete effects, an exact snapshot, an authoritative schema, or an authored
behavioral contract. It does not derive semantics from names such as `admin`, `sort`, `cursor`,
`balance`, or `submitOrder`.

Detailed event and configuration examples live in
[`validation/backend/README.md`](../validation/backend/README.md).

## A2UI oracles

An A2UI target is validated as a v0.9 message stream and rendered through the official React and Lit
integrations. ReproIt checks protocol structure, renderer behavior, and equivalence under
transformations that should preserve meaning.

| Finding                  | Proves                                                                                                                   |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------ |
| `protocol-invalid`       | A message violates the official schema, catalog, operation count, component shape, or surface lifecycle.                 |
| `renderer-error`         | A schema-valid stream causes a captured renderer exception.                                                              |
| `unlabeled-input`        | A rendered visible form control has no accessible name.                                                                  |
| `unlabeled-button`       | A rendered visible button has no accessible name.                                                                        |
| `stream-convergence`     | Official message replay, cross-renderer replay, or idempotent update normalization produces different structural state.  |
| `default-conformance`    | React and Lit resolve an official schema default differently.                                                            |
| `bound-action-coherence` | A declared input binding, edited model value, action identity, or action context fails an exact edit-and-activate trace. |

Every A2UI finding stores the minimized message stream, structural signature, renderer identity,
exact repair context, and replay predicate. Unsupported catalog behavior and ambiguous component
mapping abstain instead of guessing.

DynamicValue function results currently `ABSTAIN`. The v0.9 schema does not define recursive array
evaluation, and React and Lit share the same executable function implementation, so ReproIt does
not treat that implementation as independent proof of its own semantics. The implementation gate
and fixture contract are documented in
[`runners/a2ui/DYNAMIC_VALUE_CONFORMANCE.md`](../runners/a2ui/DYNAMIC_VALUE_CONFORMANCE.md).

Detailed integration, conformance, and CI examples live in
[`runners/a2ui/README.md`](../runners/a2ui/README.md).

## Scan, fuzz, shrink, and replay

The command changes how ReproIt obtains evidence, not what an oracle means:

| Command                | Role                                                                                                          |
| ---------------------- | ------------------------------------------------------------------------------------------------------------- |
| `reproit scan`         | Walk each reachable state or operation once and evaluate applicable oracles.                                  |
| `reproit fuzz`         | Explore deeper sequences and generated structural inputs, then evaluate the same oracles.                     |
| `reproit <finding-id>` | Rebuild the saved setup, replay the minimized sequence, and require the same oracle identity and fingerprint. |

Shrinking may remove actions, messages, or requests only while the same proof still reproduces. A
shorter sequence that produces a different failure is not accepted as the same bug.

## Platform limitations

ReproIt never fabricates a signal a platform does not expose. Important limits include:

- Firefox and WebKit do not expose Chromium's precise heap domain, so browser leak confirmation uses
  Chromium.
- Tauri WebDriver cannot provide authoritative document HTTP status, so it does not emit
  `broken-route`.
- Accessibility trees and terminal grids do not expose a frame timeline, so they cannot prove jank.
- iOS simulators do not expose an attributable per-app animation-hitch stream through the current
  out-of-process driver.
- Out-of-process Windows UI Automation cannot attribute compositor frame statistics to one window.
- Backend eventual consistency remains `ABSTAIN` without an authored observation boundary.

## Source of truth

The top-level CLI and Cloud category registry is
[`crates/reproit/oracle-registry.json`](../crates/reproit/oracle-registry.json). Backend and A2UI
finding subtypes retain their exact subtype inside the saved contract evidence. Registry drift is
tested so Cloud must handle every category and must preserve unclassified future categories
rather than dropping them.
