/*!
 * Reproit universal causal recorder.
 *
 * The recorder stores observed facts only. It cannot store executable commands
 * or grant a reproduction provider authority. Capture failures never escape
 * into the instrumented application.
 */
'use strict';

const crypto = require('node:crypto');

const CAPTURE_VERSION = 1;
const MAX_EVENTS = 5000;
const MAX_ARTIFACTS = 256;
const MAX_QUEUE_BATCHES = 64;
const MAX_BATCH_BYTES = 4 * 1024 * 1024;
const MAX_TEXT_BYTES = 16 * 1024;
const MAX_RETRIES = 5;
const MIN_FLUSH_INTERVAL_MS = 100;

const TOKEN = /^[A-Za-z0-9._:-]{1,128}$/;
const CAPABILITIES = new Set([
  'process-tree', 'commands', 'standard-streams', 'filesystem', 'environment',
  'network', 'http', 'rpc', 'database', 'cache', 'queue', 'object-store',
  'jobs', 'timers', 'user-interface', 'device', 'crash-diagnostics',
  'resource-pressure', 'clock', 'randomness', 'concurrency',
  'imported-diagnostics',
]);

function token(value, name) {
  if (typeof value !== 'string' || !TOKEN.test(value)) {
    throw new TypeError(name + ' must be a bounded protocol token');
  }
  return value;
}

function text(value, name) {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    Buffer.byteLength(value) > MAX_TEXT_BYTES ||
    value.includes('\0')
  ) {
    throw new TypeError(name + ' must be non-empty bounded text');
  }
  return value;
}

function cloneJson(value, name) {
  let encoded;
  try {
    encoded = JSON.stringify(value);
  } catch {
    throw new TypeError(name + ' must be JSON serializable');
  }
  if (encoded === undefined || Buffer.byteLength(encoded) > 64 * 1024) {
    throw new TypeError(name + ' exceeds the captured-value bound');
  }
  return JSON.parse(encoded);
}

function structural(shape) {
  return { representation: 'structural', shape: cloneJson(shape, 'shape') };
}

function replayable(value, redaction = 'redacted-at-source') {
  if (!['not-required', 'redacted-at-source', 'redacted-before-storage'].includes(redaction)) {
    throw new TypeError('replayable values must be safe before capture');
  }
  return { representation: 'replayable', value: cloneJson(value, 'value'), redaction };
}

function environmentBound(reference) {
  return { representation: 'environment-bound', reference: text(reference, 'reference') };
}

class Recorder {
  constructor(config) {
    if (!config || typeof config !== 'object') throw new TypeError('config is required');
    const maxEvents = config.maxEvents ?? 1024;
    const maxArtifacts = config.maxArtifacts ?? 32;
    if (!Number.isInteger(maxEvents) || maxEvents < 2 || maxEvents > MAX_EVENTS) {
      throw new RangeError('maxEvents is outside the protocol bound');
    }
    if (!Number.isInteger(maxArtifacts) || maxArtifacts < 1 || maxArtifacts > MAX_ARTIFACTS) {
      throw new RangeError('maxArtifacts is outside the protocol bound');
    }
    this._config = {
      batchId: token(config.batchId ?? randomId('cb'), 'batchId'),
      projectId: token(config.projectId, 'projectId'),
      sessionId: correlationToken(config.sessionId ?? randomId('session'), 'sessionId'),
      emitter: validateEmitter(config.emitter),
      deployment: validateDeployment(config.deployment),
      observedAt: config.observedAt ?? new Date().toISOString(),
      policy: validatePolicy(config.policy),
      capabilities: validateCapabilities(config.capabilities ?? []),
      maxEvents,
      maxArtifacts,
    };
    this._events = [];
    this._artifacts = [];
    this._artifactIds = new Set();
    this._droppedEventIds = new Set();
    this._droppedEvents = 0;
    this._droppedArtifacts = 0;
    this._sequence = 1;
    this._lastMonotonicNs = 0;
    this._finished = false;
  }

  record(event, context = {}) {
    if (this._finished) return null;
    try {
      const sequence = this._sequence++;
      const monotonicNs = boundedInteger(context.monotonicNs ?? sequence, 'monotonicNs', 0);
      this._lastMonotonicNs = Math.max(this._lastMonotonicNs, monotonicNs);
      const id = 'evt_' + this._config.emitter.id + '_' + sequence;
      const parents = Array.isArray(context.causalParentIds)
        ? context.causalParentIds.slice(0, 32).map((parent) => token(parent, 'causal parent'))
        : [];
      const captured = {
        id,
        sequence,
        monotonicNs,
        causalParentIds: parents,
        event: validateEvent(event),
      };
      optionalCorrelation(captured, 'actor', context.actor);
      optionalCorrelation(captured, 'traceId', context.traceId);
      optionalCorrelation(captured, 'spanId', context.spanId);
      optionalInteger(captured, 'processId', context.processId);
      optionalInteger(captured, 'threadId', context.threadId);
      if (context.wallTime != null) captured.wallTime = text(context.wallTime, 'wallTime');
      if (this._events.length === this._config.maxEvents) this._dropOldest();
      this._events.push(captured);
      return id;
    } catch {
      return null;
    }
  }

  operationStart(name, context) {
    return this.record({ kind: 'operation-start', name }, context);
  }

  operationEnd(name, outcome, context) {
    return this.record({ kind: 'operation-end', name, outcome }, context);
  }

  trigger(trigger, subject, value, context) {
    const event = { kind: 'trigger', trigger, subject };
    if (value != null) event.value = value;
    return this.record(event, context);
  }

  input(name, value, context) {
    return this.record({ kind: 'input', name, value }, context);
  }

  state(state, operation, subject, value, context) {
    const event = { kind: 'state-access', state, operation, subject };
    if (value != null) event.value = value;
    return this.record(event, context);
  }

  dependency(system, operation, subject, value, context) {
    const event = { kind: 'dependency', system, operation, subject };
    if (value != null) event.value = value;
    return this.record(event, context);
  }

  effect(effect, subject, value, context) {
    const event = { kind: 'effect', effect, subject };
    if (value != null) event.value = value;
    return this.record(event, context);
  }

  checkpoint(name, attributes = {}, context) {
    return this.record({ kind: 'checkpoint', name, attributes }, context);
  }

  failure(failure, context) {
    return this.record({ kind: 'observation', failure }, context);
  }

  defect(defect, detail, artifactId, context) {
    const event = { kind: 'defect', defect, detail };
    if (artifactId != null) event.artifactId = artifactId;
    return this.record(event, context);
  }

  addArtifact(artifact) {
    if (this._finished) return false;
    try {
      validateArtifact(artifact);
      if (this._artifactIds.has(artifact.id)) return false;
      if (this._artifacts.length === this._config.maxArtifacts) {
        this._droppedArtifacts += 1;
        return false;
      }
      this._artifactIds.add(artifact.id);
      this._artifacts.push(cloneJson(artifact, 'artifact'));
      return true;
    } catch {
      return false;
    }
  }

  finish() {
    if (this._finished) return null;
    this._finished = true;
    this._removeDroppedParents();
    if (this._droppedEvents > 0 || this._droppedArtifacts > 0) {
      if (this._events.length === this._config.maxEvents) {
        this._dropOldest();
        this._removeDroppedParents();
      }
      this._events.push({
        id: 'evt_' + this._config.emitter.id + '_' + this._sequence,
        sequence: this._sequence,
        monotonicNs: this._lastMonotonicNs + 1,
        causalParentIds: [],
        event: {
          kind: 'defect',
          defect: 'dropped',
          detail:
            this._droppedEvents + ' event(s) and ' +
            this._droppedArtifacts + ' artifact(s) exceeded recorder bounds',
        },
      });
    }
    const batch = {
      version: CAPTURE_VERSION,
      batchId: this._config.batchId,
      projectId: this._config.projectId,
      sessionId: this._config.sessionId,
      emitter: this._config.emitter,
      observedAt: this._config.observedAt,
      policy: this._config.policy,
      capabilities: this._config.capabilities,
      events: this._events,
      artifacts: this._artifacts,
    };
    if (this._config.deployment != null) batch.deployment = this._config.deployment;
    return batch;
  }

  _dropOldest() {
    const dropped = this._events.shift();
    if (dropped != null) {
      this._droppedEventIds.add(dropped.id);
      this._droppedEvents += 1;
    }
  }

  _removeDroppedParents() {
    for (const event of this._events) {
      event.causalParentIds = event.causalParentIds.filter(
        (parent) => !this._droppedEventIds.has(parent),
      );
    }
  }
}

class Transport {
  static create(config) {
    try {
      return new Transport(config);
    } catch {
      return null;
    }
  }

  constructor(config) {
    this._endpoint = text(config.endpoint, 'endpoint');
    this._apiKey = text(config.apiKey, 'apiKey');
    this._flushIntervalMs = Math.max(
      MIN_FLUSH_INTERVAL_MS,
      boundedInteger(config.flushIntervalMs ?? 3000, 'flushIntervalMs', 1),
    );
    this._requestTimeoutMs = boundedInteger(
      config.requestTimeoutMs ?? 5000,
      'requestTimeoutMs',
      1,
    );
    this._retryLimit = Math.min(
      MAX_RETRIES,
      boundedInteger(config.retryLimit ?? 2, 'retryLimit', 0),
    );
    this._queue = [];
    this._sending = false;
    this._timer = null;
    this._idle = [];
    this._stats = { queuedBatches: 0, droppedBatches: 0, sentBatches: 0, failedBatches: 0 };
  }

  submit(batch, artifactBytes = {}) {
    try {
      const body = canonicalJson(batch);
      if (Buffer.byteLength(body) > MAX_BATCH_BYTES) return false;
      const artifacts = validateArtifactBytes(batch, artifactBytes);
      if (this._queue.length === MAX_QUEUE_BATCHES) {
        this._queue.shift();
        this._stats.droppedBatches += 1;
      }
      this._queue.push({
        body,
        projectId: token(batch.projectId, 'batch.projectId'),
        artifacts,
      });
      this._stats.queuedBatches += 1;
      this._arm(this._flushIntervalMs);
      return true;
    } catch {
      return false;
    }
  }

  flush(timeoutMs = 5000) {
    this._arm(0);
    if (this._queue.length === 0 && !this._sending) return Promise.resolve(true);
    return new Promise((resolve) => {
      const timer = setTimeout(() => resolve(false), timeoutMs);
      if (timer.unref) timer.unref();
      this._idle.push(() => {
        clearTimeout(timer);
        resolve(true);
      });
    });
  }

  stats() {
    return { ...this._stats };
  }

  _arm(delayMs) {
    if (this._sending) return;
    if (this._timer != null) {
      if (delayMs > 0) return;
      clearTimeout(this._timer);
    }
    this._timer = setTimeout(() => {
      this._timer = null;
      void this._drain();
    }, delayMs);
    if (this._timer.unref) this._timer.unref();
  }

  async _drain() {
    if (this._sending) return;
    this._sending = true;
    try {
      while (this._queue.length > 0) {
        const pending = this._queue.shift();
        if (await this._send(pending)) this._stats.sentBatches += 1;
        else this._stats.failedBatches += 1;
      }
    } finally {
      this._sending = false;
      const idle = this._idle.splice(0);
      for (const resolve of idle) resolve();
    }
  }

  async _send(pending) {
    for (let attempt = 0; attempt <= this._retryLimit; attempt++) {
      try {
        let artifactsUploaded = true;
        for (const artifact of pending.artifacts) {
          const response = await fetch(
            artifactEndpoint(this._endpoint, pending.projectId, artifact.digest),
            {
              method: 'PUT',
              headers: {
                authorization: 'Bearer ' + this._apiKey,
                'content-type': 'application/octet-stream',
              },
              body: artifact.bytes,
              signal: AbortSignal.timeout(this._requestTimeoutMs),
            },
          );
          if (!response.ok) {
            artifactsUploaded = false;
            if (response.status >= 400 && response.status < 500) return false;
            break;
          }
        }
        if (!artifactsUploaded) throw new Error('artifact upload failed');
        const response = await fetch(this._endpoint, {
          method: 'POST',
          headers: {
            authorization: 'Bearer ' + this._apiKey,
            'content-type': 'application/json',
          },
          body: pending.body,
          signal: AbortSignal.timeout(this._requestTimeoutMs),
        });
        if (response.ok) return true;
        if (response.status >= 400 && response.status < 500) return false;
      } catch {
        // Network failures retry within the explicit budget.
      }
      if (attempt < this._retryLimit) await boundedDelay(200 * (attempt + 1));
    }
    return false;
  }
}

function validateArtifactBytes(batch, artifactBytes) {
  if (
    artifactBytes == null ||
    typeof artifactBytes !== 'object' ||
    Array.isArray(artifactBytes)
  ) {
    throw new TypeError('artifactBytes must be a digest keyed object');
  }
  const required = new Map(
    (batch.artifacts ?? [])
      .filter((artifact) => artifact.policy === 'exportable')
      .map((artifact) => [artifact.id, artifact]),
  );
  const uploads = [];
  for (const [digest, value] of Object.entries(artifactBytes)) {
    const artifact = required.get(digest);
    if (artifact == null || !Buffer.isBuffer(value)) {
      throw new TypeError('artifact bytes do not match exportable metadata');
    }
    const actual = 'sha256:' + crypto.createHash('sha256').update(value).digest('hex');
    if (actual !== digest || value.length !== artifact.bytes) {
      throw new TypeError('artifact bytes failed digest or length validation');
    }
    uploads.push({ digest, bytes: value });
    required.delete(digest);
  }
  if (required.size > 0) throw new TypeError('exportable artifact bytes are missing');
  return uploads;
}

function artifactEndpoint(endpoint, projectId, digest) {
  const url = new URL(endpoint);
  const suffix = '/v1/capture-batches';
  if (!url.pathname.endsWith(suffix)) {
    throw new TypeError('capture endpoint must end with /v1/capture-batches');
  }
  url.pathname =
    url.pathname.slice(0, -suffix.length) +
    '/v1/capture-artifacts/' +
    encodeURIComponent(projectId) +
    '/' +
    encodeURIComponent(digest);
  return url.toString();
}

function validateEmitter(emitter) {
  if (!emitter || typeof emitter !== 'object') throw new TypeError('emitter is required');
  const value = {
    id: token(emitter.id, 'emitter.id'),
    kind: token(emitter.kind, 'emitter.kind'),
    component: token(emitter.component, 'emitter.component'),
  };
  optionalToken(value, 'runtime', emitter.runtime);
  optionalToken(value, 'parentId', emitter.parentId);
  return value;
}

function validateDeployment(deployment) {
  if (deployment == null) return null;
  const value = {};
  optionalToken(value, 'version', deployment.version);
  optionalToken(value, 'commit', deployment.commit);
  if (Object.keys(value).length === 0) throw new TypeError('deployment has no identity');
  return value;
}

function validatePolicy(policy) {
  const value = policy ?? {};
  return {
    consent: token(value.consent ?? 'application-telemetry', 'policy.consent'),
    retentionClass: token(value.retentionClass ?? 'standard', 'policy.retentionClass'),
  };
}

function validateCapabilities(capabilities) {
  const seen = new Set();
  return capabilities.map((entry) => {
    if (!entry || !CAPABILITIES.has(entry.capability) || seen.has(entry.capability)) {
      throw new TypeError('invalid or duplicate capture capability');
    }
    seen.add(entry.capability);
    const value = {
      capability: entry.capability,
      completeness: token(entry.completeness, 'capability.completeness'),
    };
    if (entry.detail != null) value.detail = text(entry.detail, 'capability.detail');
    if (value.completeness !== 'complete' && value.detail == null) {
      throw new TypeError('incomplete capabilities require detail');
    }
    return value;
  });
}

function validateEvent(event) {
  if (!event || typeof event !== 'object') throw new TypeError('event is required');
  const value = cloneJson(event, 'event');
  token(value.kind, 'event.kind');
  return value;
}

function validateArtifact(artifact) {
  if (!artifact || typeof artifact !== 'object') throw new TypeError('artifact is required');
  if (!/^sha256:[a-f0-9]{64}$/.test(artifact.id)) throw new TypeError('invalid artifact digest');
  boundedInteger(artifact.bytes, 'artifact.bytes', 0);
  token(artifact.kind, 'artifact.kind');
  text(artifact.mediaType, 'artifact.mediaType');
  token(artifact.policy, 'artifact.policy');
  token(artifact.redaction, 'artifact.redaction');
  token(artifact.collection, 'artifact.collection');
}

function optionalToken(target, name, value) {
  if (value != null) target[name] = token(value, name);
}

function optionalCorrelation(target, name, value) {
  if (value == null) return;
  target[name] = correlationToken(value, name);
}

function correlationToken(value, name) {
  if (typeof value !== 'string' || value.length === 0) {
    throw new TypeError(name + ' must be a non-empty string');
  }
  try {
    return token(value, name);
  } catch {
    const digest = crypto.createHash('sha256').update(value, 'utf8').digest('hex').slice(0, 32);
    return name.toLowerCase() + ':' + digest;
  }
}

function optionalInteger(target, name, value) {
  if (value != null) target[name] = boundedInteger(value, name, 1);
}

function boundedInteger(value, name, minimum) {
  if (!Number.isSafeInteger(value) || value < minimum) {
    throw new TypeError(name + ' must be a bounded integer');
  }
  return value;
}

function randomId(prefix) {
  return prefix + '_' + crypto.randomBytes(8).toString('hex');
}

function canonicalJson(value) {
  if (Array.isArray(value)) return '[' + value.map(canonicalJson).join(',') + ']';
  if (value !== null && typeof value === 'object') {
    return '{' + Object.keys(value).sort().map(
      (key) => JSON.stringify(key) + ':' + canonicalJson(value[key]),
    ).join(',') + '}';
  }
  return JSON.stringify(value);
}

function boundedDelay(milliseconds) {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, milliseconds);
    if (timer.unref) timer.unref();
  });
}

module.exports = {
  CAPTURE_VERSION,
  MAX_EVENTS,
  MAX_ARTIFACTS,
  Recorder,
  Transport,
  structural,
  replayable,
  environmentBound,
  canonicalJson,
};
