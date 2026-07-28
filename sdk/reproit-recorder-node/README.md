# Reproit recorder for Node

This is the source-neutral recorder used by semantic Node adapters. It records
causal facts about any software operation, including requests, commands,
messages, timers, jobs, state access, dependencies, process events, and UI
actions. It does not choose or execute reproduction commands.

```js
const { Recorder, Transport, replayable } = require('reproit-recorder');

const recorder = new Recorder({
  projectId: 'orders',
  emitter: {
    id: 'orders-api',
    kind: 'runtime-sdk',
    component: 'orders',
    runtime: 'node',
  },
  capabilities: [{ capability: 'http', completeness: 'complete' }],
});

recorder.trigger(
  'http-request',
  'POST /orders',
  replayable({ body: { sku: 'widget', quantity: 2 } }),
);
recorder.failure({
  observation: 'exception',
  authority: 'runtime-diagnosis',
  summary: 'order creation failed',
  signature: 'orders:create:unique-violation',
  artifactIds: [],
});

const transport = Transport.create({
  endpoint: 'https://cloud.example/v1/capture-batches',
  apiKey: process.env.REPROIT_API_KEY,
});
transport?.submit(recorder.finish());
```

Use `structural(...)` when only a shape may leave the process. Use
`replayable(...)` only after values are safe at the capture boundary. Use
`environmentBound(...)` when the value must remain on an authorized worker.

Recorder and transport buffers are bounded. Network work is asynchronous and
never runs on the instrumented operation's critical path. Capture rejection
does not fail the host application, but Cloud will mark missing evidence as
incomplete rather than claiming reproduction.

External session, trace, span, and actor identifiers that are not valid wire
tokens are converted to deterministic SHA-256 correlation tokens. This keeps
causal joins without putting arbitrary identifier text on the wire.

For exportable artifacts, pass a digest-keyed `Buffer` map as the second
argument to `submit`. The transport verifies every digest and byte length,
uploads the bytes first, then submits the immutable batch. Local-only and
environment-bound artifacts are never uploaded.
