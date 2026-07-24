/*
 * JS mirror of `reproit_protocol::EventBatch::validate` (crates/reproit-protocol/src/lib.rs),
 * scoped to the event kinds the production SDKs emit. Node SDK tests use it the same way the
 * Rust backend SDK round-trips its batches through the real protocol crate: any batch a
 * backend SDK builds must pass this validator unchanged. Throws Error('<reason-code>') on the
 * first defect, mirroring the protocol reason codes.
 */
'use strict';

var MAX_BATCH_FRAMES = 5000;
var MAX_BATCH_GRAPHS = 256;
var MAX_FRAME_BYTES = 1024 * 1024;
var MAX_TOKEN_BYTES = 128;
var MAX_TEXT_BYTES = 16 * 1024;
var MAX_CONTEXT_BYTES = 64 * 1024;

function fail(reason) {
  throw new Error(reason);
}

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function onlyKeys(value, allowed, reason) {
  for (var key of Object.keys(value)) {
    if (allowed.indexOf(key) < 0) fail(reason);
  }
}

function token(value) {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    Buffer.byteLength(value) > MAX_TOKEN_BYTES ||
    !/^[A-Za-z0-9._:-]+$/.test(value)
  ) {
    fail('invalid-event');
  }
}

function lowerToken(value) {
  token(value);
  if (!/^[a-z0-9_-]+$/.test(value)) fail('invalid-event');
}

function text(value, maxBytes) {
  if (typeof value !== 'string' || Buffer.byteLength(value) > maxBytes) fail('invalid-event');
}

function optionalText(value, maxBytes) {
  if (value !== null && value !== undefined) text(value, maxBytes);
}

function valueBytes(value, maxBytes) {
  if (Buffer.byteLength(JSON.stringify(value)) > maxBytes) fail('invalid-event');
}

function validateScope(scope) {
  if (!isObject(scope)) fail('invalid-scope');
  if (scope.domain === 'shared' || scope.domain === 'backend') {
    onlyKeys(scope, ['domain'], 'invalid-scope');
    return;
  }
  if (scope.domain !== 'contract') fail('invalid-scope');
  onlyKeys(scope, ['domain', 'contractHash'], 'invalid-scope');
  if (scope.contractHash !== null && scope.contractHash !== undefined) {
    if (!/^[0-9a-f]{16}$/.test(scope.contractHash)) fail('invalid-scope');
  }
}

function validateIdentity(identity) {
  if (!isObject(identity)) fail('invalid-event');
  onlyKeys(
    identity,
    ['oracle', 'invariant', 'kind', 'message', 'frame', 'trigger', 'boundary'],
    'invalid-event',
  );
  lowerToken(identity.oracle);
  for (var field of ['invariant', 'kind', 'message', 'frame', 'trigger']) {
    text(identity[field], MAX_TEXT_BYTES);
  }
  optionalText(identity.boundary, MAX_TEXT_BYTES);
}

function validateEvent(event) {
  if (!isObject(event)) fail('invalid-event');
  switch (event.kind) {
    case 'backend':
      onlyKeys(event, ['kind', 'evidence'], 'invalid-event');
      valueBytes(event.evidence, MAX_CONTEXT_BYTES);
      return;
    case 'graph-edge':
      onlyKeys(event, ['kind', 'from', 'action', 'to'], 'invalid-event');
      text(event.from, MAX_TEXT_BYTES);
      text(event.action, MAX_TEXT_BYTES);
      text(event.to, MAX_TEXT_BYTES);
      return;
    case 'finding':
      onlyKeys(
        event,
        ['kind', 'signature', 'message', 'identity', 'path', 'context'],
        'invalid-event',
      );
      text(event.signature, MAX_TEXT_BYTES);
      text(event.message, MAX_TEXT_BYTES);
      validateIdentity(event.identity);
      if (!Array.isArray(event.path) || event.path.length > 256) fail('invalid-event');
      for (var step of event.path) {
        if (!isObject(step)) fail('invalid-event');
        onlyKeys(step, ['signature', 'action', 'label'], 'invalid-event');
        text(step.signature, MAX_TEXT_BYTES);
        text(step.action, MAX_TEXT_BYTES);
        optionalText(step.label, MAX_TEXT_BYTES);
      }
      if (!isObject(event.context)) fail('invalid-event');
      valueBytes(event.context, MAX_CONTEXT_BYTES);
      return;
    default:
      fail('invalid-event');
  }
}

function validateFrame(frame) {
  if (!isObject(frame)) fail('malformed-frame');
  onlyKeys(frame, ['runId', 'sequence', 'scope', 'event'], 'malformed-frame');
  token(frame.runId);
  if (!Number.isInteger(frame.sequence) || frame.sequence < 0) fail('invalid-sequence');
  validateScope(frame.scope);
  validateEvent(frame.event);
  if (Buffer.byteLength(JSON.stringify(frame.event)) > MAX_FRAME_BYTES) fail('frame-too-large');
}

function validateEventBatch(batch) {
  if (!isObject(batch)) fail('malformed-frame');
  onlyKeys(
    batch,
    ['version', 'batchId', 'appId', 'deployment', 'frames', 'evidence'],
    'invalid-event',
  );
  if (batch.version !== 1) fail('unsupported-version');
  token(batch.batchId);
  token(batch.appId);
  if (batch.deployment !== null && batch.deployment !== undefined) {
    if (!isObject(batch.deployment)) fail('invalid-event');
    onlyKeys(batch.deployment, ['version', 'commit'], 'invalid-event');
    if (batch.deployment.version == null && batch.deployment.commit == null) {
      fail('invalid-event');
    }
    if (batch.deployment.version != null) token(batch.deployment.version);
    if (batch.deployment.commit != null) token(batch.deployment.commit);
  }
  if (!Array.isArray(batch.frames) || !Array.isArray(batch.evidence)) fail('invalid-event');
  if (batch.frames.length > MAX_BATCH_FRAMES) fail('batch-too-large');
  if (batch.evidence.length > MAX_BATCH_GRAPHS) fail('batch-too-large');
  if (batch.frames.length === 0 && batch.evidence.length === 0) fail('invalid-event');
  var lastSequence = null;
  for (var frame of batch.frames) {
    validateFrame(frame);
    if (lastSequence !== null && frame.sequence <= lastSequence) fail('invalid-sequence');
    lastSequence = frame.sequence;
  }
}

module.exports = { validateEventBatch };
