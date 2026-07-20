# A2UI structural verification

The canonical proof model and concise A2UI oracle catalog live in
[`docs/oracles.md`](../../docs/oracles.md#a2ui-oracles). This page covers the implementation,
integration, and conformance gates.

ReproIt accepts an A2UI v0.9 JSON array, a wrapper with a `messages` array, or a JSONL stream
directly through its normal commands:

```sh
reproit scan generated-ui.jsonl
reproit fuzz generated-ui.jsonl
reproit fnd_...
```

The packaged runner validates the official basic-catalog schemas, renders the stream with the
official React and Lit packages, checks structural behavior, and saves an exact minimized message
stream for each confirmed finding. Users do not clone A2UI or install a separate adapter. The first
web or A2UI run provisions the pinned runner dependencies and Chromium.

The files in this directory hold the deeper conformance, ADK transport, and repair experiments
behind that CLI surface.

## Negotiation and update convergence

Before a renderer starts, ReproIt validates the complete message list with the official v0.9
server-to-client schema and every component with its official basic-catalog schema. The gate proves
the negotiated version and catalog, required properties, one operation per message, supported
component types, and surface lifecycle ordering. An update before `createSurface`, an update after
`deleteSurface`, or a second create for a live surface is a protocol finding. Repeated deletes
remain valid because the official processor defines deletion of a missing surface as a no-op.

Schema-valid data bindings are then checked against the final model when the catalog declares an
exact primitive type. Each result has one of three states:

- `VIOLATION` means the official schema, lifecycle, or resolved model contradicts the stream.
  Only this state creates a finding.
- `SATISFIED` means the official contract and observed value agree.
- `ABSTAIN` means a function, missing runtime value, custom catalog, ambiguous dynamic scope, or
  external response prevents a proof. It never creates a finding.

For every valid scan, the official processor state is compared with a canonical surface graph and
data model derived from the ordered stream. React and Lit are also compared with each other. When
repeated or split updates are present, the same stream is replayed after removing exact idempotent
duplicates and compacting consecutive component updates. A difference is a `stream-convergence`
finding with the first exact structural mismatch, the transformation, a minimized message stream,
and renderer-owned repair context.

Defaults come from the official component schemas. Omitted defaults are retained as negotiated
`SATISFIED` claims, resolved properties are compared across official renderers, and bound controls
exercise observable default behavior through the typed state and action oracle. This catches
behavior such as a renderer treating an omitted mutually-exclusive ChoicePicker variant as
multi-select without guessing from pixels or text.

The packaged runtime currently contains the official React and Lit renderers. The separate Flutter
GenUI renderer is not silently treated as equivalent; its native gate remains capability-limited as
documented below.

The proposed DynamicValue function-conformance oracle is intentionally blocked. The v0.9 schema
does not define recursive array evaluation, the official runtime treats arrays as literals, and
React and Lit share the same function implementation. The exact authority requirement, bounded
evaluator design, and red, green, and ambiguous fixture contract are documented in
[DYNAMIC_VALUE_CONFORMANCE.md](DYNAMIC_VALUE_CONFORMANCE.md).

## Bound form and action verification

For the official v0.9 basic catalog, normal `scan` and `fuzz` runs exercise an exact
bound-control-to-Button contract when the stream itself proves the relationship. ReproIt supports
TextField, CheckBox, ChoicePicker, Slider, and DateTimeInput. It requires all of the following
before it runs the check:

- The control value is one exact data binding with a current value of the catalog-declared type.
- The Button declares an event action whose context contains that same pointer.
- The edit has one deterministic valid result. ChoicePicker values come only from its declared
  options, Slider values remain inside its declared range, and date/time values use the enabled
  native input mode.
- A dynamic List binding resolves to one concrete data item and both the control and Button share
  that exact list scope.
- The control has no custom validation, function binding, external response, or other behavior whose
  result cannot be proved from the stream.
- Both official renderer controls have one unambiguous component and scope marker and are visible
  and enabled.

For each eligible pair, ReproIt renders a fresh surface, performs one typed edit, activates the
declared Button, and verifies the initial rendered and model state, edited rendered and model state,
event count, event name, surface ID, source component ID, and current context value. React and Lit
are checked independently. A failure stores the typed edit, exact list scope, and activation as part
of the finding, preserves the exact oracle during shrinking, and replays with `reproit fnd_...`.

Unsupported function bindings, literal fields, validation rules, invalid or ambiguous initial
values, nested or ambiguous list scopes, unrelated action context, ambiguous component mapping, and
unavailable controls abstain. They do not become findings. A schema-valid failure is attributed to
the affected official renderer, and the repair feedback does not suggest changing the message
stream.

The capture stores the protocol version and schema plus SHA-256, complete catalog plus SHA-256,
ordered JSONL messages plus SHA-256, renderer name/version/platform, client data-model snapshots,
user actions, agent input identity, and an exact structural oracle identity. Replay reconstructs
surfaces, component graphs, and JSON-pointer data updates without using labels or screenshots.
Shrink uses delta debugging and accepts a candidate only when it remains protocol-valid and fails
the exact same oracle identity.

The renderer matrix reports four independent causes:

- `protocolInvalidity`: the stream or capture is structurally invalid.
- `agentNondeterminism`: one agent input produced different streams.
- `rendererDivergence`: renderers observed different results for one stream and oracle.
- `appUiFailure`: every renderer observed the same exact UI failure.

Run the durable unit tests:

```sh
node --test runners/a2ui/*.test.mjs
```

## Near-zero-setup stream integration

### Official ADK/A2A adapter

The concrete adapter accepts the raw A2A event stream produced by the official ADK
`A2uiEventConverter`; the application does not write an event extractor. For observation without
withholding transport events, use `instrumentAdkA2ui`:

```js
import { instrumentAdkA2ui } from "./runners/a2ui/adk-a2a.mjs";

const observed = instrumentAdkA2ui(a2aEvents, {
  protocolVersion,
  protocolDocument,
  catalog,
  renderer,
  oracle,
  onResult: (evidence) => saveLocally(evidence),
});

for await (const event of observed.events) yieldToClient(event);
const evidence = await observed.result;
```

`instrumentAdkA2ui` is explicitly observation-only because A2A events are passed through before the
complete stream can be validated. To keep generated UI away from the renderer until it passes, use
the buffered delivery gate:

```js
import { preflightAdkA2ui } from "./runners/a2ui/adk-a2a.mjs";

await preflightAdkA2ui(a2aEvents, {
  protocolVersion,
  protocolDocument,
  catalog,
  renderer,
  oracle,
  repair: repairA2ui,
  deliver: (messages) => sendVerifiedA2uiBatch(messages),
});
```

This API consumes the complete event stream and delivers only decoded A2UI messages that pass exact
validation and replay. It never yields raw A2A events. Failed, interrupted, oversized, or
exhausted-repair streams deliver nothing.

This seam is pinned to A2UI commit `96abfdc60de0657c6322028d10c1cc7bc25c237c`, its
`samples/agent/adk/restaurant_finder` integration (`google-adk>=1.28.1`, `a2a-sdk>=0.3.0`),
`@a2a-js/sdk` 0.3.13, `@a2ui/web_core` 0.10.4, and A2UI v0.9. It recognizes `message.parts` and
`status-update.status.message.parts`, including both official A2UI MIME spellings. Exact cumulative
`createSurface` parts are deduplicated because the official client notes that status-update parts
can be cumulative. A changed duplicate is retained and therefore fails replay.

The wrapper automatically uses the official `@a2ui/web_core/v0_9` `A2uiMessageListSchema` and
`BASIC_COMPONENTS` validators installed by the renderer. The automatic path accepts only the exact
official v0.9 basic catalog and validates each component against its official component schema. A
missing or incompatible validator is an infrastructure failure, not a pass. Apps with a custom
catalog must supply its official catalog-aware validator as `validateMessages`; the callback
receives `(messages, {protocolVersion, protocolDocument,
catalog})` and returns an empty array on
success or structural error objects/strings on failure. Unknown lookalike envelopes, conflicting
MIME metadata, and A2UI-looking data without an official MIME type fail closed.

The reduced A2A event fixture in `fixtures/adk-a2a-events-v0.9.json` is derived from the official
ADK sample, A2A client middleware, and A2UI part helpers at the pinned commit. Those upstream
sources are Apache-2.0. It intentionally contains both official MIME layouts and a cumulative status
update.

`integration.mjs` wraps an existing async ADK, A2A, AG-UI, or GenUI event stream without modifying
event values or ordering. The application supplies one small extractor because framework event
envelopes change independently of A2UI:

```js
import { instrumentA2ui } from "./runners/a2ui/integration.mjs";

const observed = instrumentA2ui(adkEvents, {
  protocolVersion,
  protocolDocument,
  catalog,
  renderer,
  oracle,
  extractMessages: (event) => event.a2uiMessages ?? [],
  validateMessages: (messages, context) => exactA2uiValidation(messages, context),
  onResult: (evidence) => saveLocally(evidence),
});

for await (const event of observed.events) yieldToClient(event);
const evidence = await observed.result;
```

The source must be fully consumed. The wrapper captures already-decoded A2UI envelopes, replays them
structurally, and creates exact minimized feedback only for a valid failing capture. It does not
install or patch ADK/GenUI, call a model, alter transport, or expose a new ReproIt command.
`sanitizeMessage` is an optional capture-only transform; yielded events always remain unchanged.

For generated UI that must not reach a renderer until it is verified, use the closed preflight loop
over decoded A2UI messages:

```js
import { preflightA2ui } from "./runners/a2ui/integration.mjs";

const verified = await preflightA2ui(generatedMessages, {
  protocolVersion,
  protocolDocument,
  catalog,
  renderer,
  oracle,
  validateMessages: (messages, context) => exactA2uiValidation(messages, context),
  maxRepairs: 2,
  repair: async ({ feedback, messages, attempt }) => repairA2ui({ feedback, messages, attempt }),
  release: async (messages) => sendVerifiedBatch(messages),
});
```

Preflight buffers the complete source and calls `release` only after exact protocol and catalog
validation plus replay pass. `validateMessages` is required, may be synchronous or asynchronous, and
returns an array of structural error objects or strings. An empty array passes. A thrown error or
any returned error fails closed. A valid failure gives the caller the exact oracle, actual value,
and ddmin-confirmed message stream. Every shrink candidate must pass the same exact protocol and
catalog validator and preserve the same oracle identity and observed failure value. Each returned
replacement is captured anew and independently replayed against the same oracle; an invalid or
still-failing replacement is never released. Attempts and message count are bounded (`maxRepairs`
defaults to 2, `maxMessages` to 10,000). There is no default repair agent or provider dependency.

This is a pre-verification delivery guarantee, not a transactional transport guarantee: after
`release` receives the verified batch, partial writes caused by the caller's own transport remain
the caller's responsibility. Buffering also trades first-byte latency and memory for the
no-partial-delivery boundary.

Run the pinned live-renderer gate (network is needed on a cold checkout):

```sh
runners/a2ui/run-official-live.sh
```

For an existing exact upstream checkout, avoid the clone/install download with:

```sh
A2UI_CHECKOUT=/path/to/A2UI runners/a2ui/run-official-live.sh
```

The live gate feeds the same official login-form stream into the maintained React and Lit explorers
in Chromium, Firefox, and WebKit. It walks the rendered DOM and open shadow roots, normalizing only
heading level and interactive element kind/type/state; text, labels, CSS classes, IDs, geometry, and
screenshots are excluded. In every engine it also removes one Lit input to prove
`rendererDivergence`, then changes the shared password component from `obscured` to `shortText` to
prove `appUiFailure` when both real renderers agree on the same faulty stream.

The fixture is a reduced copy of the official Apache-2.0 A2UI v0.9.1 contact form conformance case
at `specification/v0_9_1/test/cases/contact_form_example.jsonl` in <https://github.com/google/A2UI>
(captured from commit `96abfdc60de0657c6322028d10c1cc7bc25c237c`).

The standalone experimental CLI can create a capture:

```sh
node runners/a2ui/cli.mjs capture \
  --protocol v0.9 \
  --protocol-schema server_to_client.json \
  --catalog-id https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json \
  --catalog catalog.json \
  --stream interaction.jsonl \
  --renderer renderer.json \
  --snapshots client-data.jsonl \
  --actions actions.jsonl \
  --oracle oracle.json \
  --out capture.json
```

Then use `replay capture.json`, `shrink capture.json --out minimal.json`, or
`matrix react.json lit.json`.

## Honest limits

- This validates the v0.9 envelope, surface lifecycle, generic component graph, and data-model
  semantics. It does not replace the official JSON Schema and catalog-specific validators.
- The durable live driver covers the maintained React and Lit renderers in Chromium, Firefox, and
  WebKit. A bounded native Flutter attempt used GenUI commit
  `470e194271d2def097207d0b45fd7a1b17f96f3c` and the public `SurfaceController`, `Surface`, and
  Flutter `SemanticsNode` APIs with the same unmodified login stream. GenUI exposed the two text
  fields (including password state) and button, but omitted the A2UI `h2` from its semantics tree.
  Matching the web vocabulary would therefore require inferring heading level from private widget
  styling or component IDs. That path is deliberately unsupported until GenUI exposes heading level
  structurally; it must not be described as hermetic or equivalent.
- Timing, transport, client-side function evaluation, A2A metadata, and v1.0 RPC action responses
  are not modeled yet.
- Bound action verification covers TextField, CheckBox, ChoicePicker, Slider, and DateTimeInput
  event-context bindings activated through Button, including unambiguous one-level dynamic List
  scopes. Function actions, custom validation behavior, ambiguous or nested dynamic scopes, and
  external action responses abstain.
- The embedded fixture is intentionally reduced. Validation against current maintained React, Lit,
  and Flutter packages is recorded separately and should be rerun when their protocol implementation
  changes.

## Official conformance gate

Run the complete deterministic gate against an exact A2UI checkout:

```sh
A2UI_EXPECTED_COMMIT=96abfdc60de0657c6322028d10c1cc7bc25c237c \
A2UI_ARTIFACT_DIR=/tmp/a2ui-conformance \
  runners/a2ui/run-official-conformance.sh /path/to/A2UI
```

The generator validates every fixture message with the official protocol and basic catalog
validators. It checks official examples and deterministic compacted-equivalent streams in the
maintained React and Lit explorers under Chromium, Firefox, and WebKit. It compares structural roles
and state, accessible-name/description presence, hashed accessibility snapshots, and hashed button
action identity; raw UI text is not written to the report.

`conformance-report.json` is the policy artifact. Reproductions tied to existing upstream issues are
reported under `knownIssueBackedFindings` and do not fail the gate. Any renderer divergence,
official/compacted mismatch, or minimized new finding is reported under `unexpectedFindings` and
exits nonzero. Infrastructure failures also exit nonzero. The other machine-readable artifacts are
`fixtures-report.json`, `renderer-report.json`, and `issue-report.json`.

An upstream CI job can retain artifacts on both success and failure:

```yaml
- uses: actions/checkout@v5
- name: Require a pinned ReproIt conformance revision
  env:
    REPROIT_CONFORMANCE_COMMIT: ${{ vars.REPROIT_CONFORMANCE_COMMIT }}
  run: test -n "$REPROIT_CONFORMANCE_COMMIT"
- uses: actions/checkout@v5
  with:
    repository: ReproIt/reproit
    ref: ${{ vars.REPROIT_CONFORMANCE_COMMIT }}
    path: .reproit-conformance
- uses: actions/setup-node@v6
  with:
    node-version: 24
- run: corepack enable
- run: npm ci
  working-directory: .reproit-conformance/runners/web
- run: npx playwright install --with-deps chromium firefox webkit
  working-directory: .reproit-conformance/runners/web
- name: A2UI conformance
  env:
    A2UI_EXPECTED_COMMIT: ${{ github.sha }}
    A2UI_ARTIFACT_DIR: artifacts/a2ui-conformance
  run: .reproit-conformance/runners/a2ui/run-official-conformance.sh "$GITHUB_WORKSPACE"
- uses: actions/upload-artifact@v7
  if: always()
  with:
    name: a2ui-conformance
    path: artifacts/a2ui-conformance
```

Set `REPROIT_CONFORMANCE_COMMIT` to a reviewed full ReproIt commit SHA. This keeps an upstream gate
from silently changing when ReproIt's default branch moves.

The maintained native Flutter implementation is not part of this web matrix. Pixel equivalence,
arbitrary CSS parity, media/network success, and interaction workflows beyond the isolated issue
probes are intentionally outside this gate.
