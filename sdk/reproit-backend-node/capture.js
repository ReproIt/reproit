/*!
 * Production capture mode: config-gated self-sampling upload of finished
 * operation traces to the Reproit Cloud ingest endpoint (`/v1/events`).
 *
 * Node port of sdk/reproit-backend-rs/src/capture.rs. Scan-time tracing stays
 * untouched: this module only adds a place to hand a finished BackendTrace
 * when no `x-reproit-trace` header exists. Operations that end in a server
 * error (HTTP 5xx) or report `success == false` are always captured; healthy
 * operations only under an optional per-mille baseline sample (default 0).
 *
 * Everything is bounded and capture failure is invisible to the host app:
 * a fixed-depth queue drops oldest on overflow, batches and retries are
 * capped, uploads run off the request path on unref'd timers, and `record`
 * never blocks or throws.
 */
'use strict';

// Payload format identifier of the replayable capture object attached to the
// finding context (`context.reproitCapture`).
const CAPTURE_FORMAT = 'reproit-backend-capture';
const CAPTURE_VERSION = 1;
// First-class registry oracle id for an operation that returned HTTP 5xx.
const SERVER_ERROR_ORACLE = 'backend-server-error';

// Bounds. Queue overflow drops the OLDEST pending operation; an oversized
// capture payload drops trailing effect events before it drops itself.
const MAX_QUEUE_OPERATIONS = 64;
const MAX_BATCH_OPERATIONS = 16;
const MAX_CAPTURE_JSON_BYTES = 48 * 1024;
const MIN_FLUSH_INTERVAL_MS = 100;
const MAX_RETRY_LIMIT = 5;

// Lazy to avoid a require cycle with index.js; resolved before first use.
let core = null;
function canonicalJson(value) {
  if (core === null) core = require('./index.js');
  return core.canonicalJson(value);
}

// The ingest protocol token charset (`validate_token` in reproit-protocol).
function validToken(value) {
  return (
    typeof value === 'string' && value.length > 0 && /^[A-Za-z0-9._:-]{1,128}$/.test(value)
  );
}

class Capture {
  // config: { endpoint, apiKey, appId, build, healthySamplePerMille,
  //           flushIntervalMs, requestTimeoutMs, retryLimit }
  // Returns null (capture disabled, host unaffected) when the config is
  // unusable: empty endpoint/key or identifiers the ingest protocol rejects.
  static create(config) {
    if (!config || typeof config !== 'object') return null;
    if (typeof config.endpoint !== 'string' || config.endpoint.trim() === '') return null;
    if (typeof config.apiKey !== 'string' || config.apiKey.trim() === '') return null;
    if (!validToken(config.appId)) return null;
    if (config.build != null && !validToken(config.build)) return null;
    return new Capture(config);
  }

  constructor(config) {
    this._config = {
      endpoint: config.endpoint,
      apiKey: config.apiKey,
      appId: config.appId,
      build: config.build ?? null,
      healthySamplePerMille: Math.max(0, Math.floor(config.healthySamplePerMille ?? 0)),
      flushIntervalMs: Math.max(MIN_FLUSH_INTERVAL_MS, config.flushIntervalMs ?? 3000),
      requestTimeoutMs: config.requestTimeoutMs ?? 5000,
      retryLimit: Math.min(MAX_RETRY_LIMIT, Math.max(0, config.retryLimit ?? 2)),
    };
    this._queue = [];
    this._sending = false;
    this._timer = null;
    this._idle = [];
    this._traceSeq = 1;
    this._batchSeq = 1;
    this._stats = {
      capturedOperations: 0,
      droppedOperations: 0,
      sentBatches: 0,
      failedBatches: 0,
    };
  }

  // Synthesized trace context for capture-mode operations, replacing the
  // scan-time `x-reproit-trace` header requirement.
  context() {
    return {
      traceId: 'cap-' + Date.now() + '-' + this._traceSeq++,
      actor: null,
      actionIndex: 0,
      build: this._config.build,
      configContract: null,
    };
  }

  // Hand a finished trace to the sampler. Unfinished traces are ignored.
  // Never blocks and never fails visibly; overflow drops the oldest queued
  // operation.
  record(trace) {
    try {
      const events = trace.events();
      const returned = [...events]
        .reverse()
        .find((event) => event && event.kind === 'return');
      if (!returned) return;
      const success = typeof returned.success === 'boolean' ? returned.success : true;
      const status =
        Number.isInteger(returned.status) && returned.status >= 0 && returned.status <= 0xffff
          ? returned.status
          : null;
      const error = !success || (status !== null && status >= 500);
      if (!error && !this._sampleHealthy()) return;
      const operation = events[0] && typeof events[0].operation === 'string'
        ? events[0].operation
        : null;
      if (operation === null) return;
      this._stats.capturedOperations += 1;
      this._queue.push({ operation, status, events: events.slice() });
      if (this._queue.length > MAX_QUEUE_OPERATIONS) {
        this._queue.shift();
        this._stats.droppedOperations += 1;
      }
      this._arm(this._config.flushIntervalMs);
    } catch (ignored) {
      // Capture must never surface errors into the host app.
    }
  }

  // Resolve true once every queued operation has been sent (or dropped),
  // false on timeout. Intended for tests, examples, and graceful shutdown.
  flush(timeoutMs) {
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

  _sampleHealthy() {
    const perMille = this._config.healthySamplePerMille;
    if (perMille <= 0) return false;
    if (perMille >= 1000) return true;
    return Math.random() * 1000 < perMille;
  }

  _arm(delayMs) {
    if (this._sending) return;
    if (this._timer !== null) {
      if (delayMs > 0) return;
      clearTimeout(this._timer);
    }
    this._timer = setTimeout(() => {
      this._timer = null;
      this._drain();
    }, delayMs);
    if (this._timer.unref) this._timer.unref();
  }

  async _drain() {
    if (this._sending) return;
    this._sending = true;
    try {
      while (this._queue.length > 0) {
        const operations = this._queue.splice(0, MAX_BATCH_OPERATIONS);
        const sent = await this._send(this._buildBatch(operations));
        if (sent) {
          this._stats.sentBatches += 1;
        } else {
          this._stats.failedBatches += 1;
          this._stats.droppedOperations += operations.length;
        }
      }
    } catch (ignored) {
      // Fail closed: drop, never crash the host.
    } finally {
      this._sending = false;
      if (this._queue.length === 0) {
        const idle = this._idle.splice(0);
        for (const resolve of idle) resolve();
      } else {
        this._arm(0);
      }
    }
  }

  // Build one event-batch-v1 payload: every captured event ships as a
  // `backend` frame, and each 5xx operation additionally ships a `finding`
  // frame tagged `backend-server-error` whose context carries the full
  // replayable capture object.
  _buildBatch(operations) {
    const batchId = 'cap-' + Date.now() + '-' + this._batchSeq++;
    const frames = [];
    let sequence = 0;
    const frame = (event) => {
      sequence += 1;
      frames.push({ runId: batchId, sequence, scope: { domain: 'shared' }, event });
    };
    for (const operation of operations) {
      for (const event of operation.events) {
        frame({ kind: 'backend', evidence: event });
      }
      if (operation.status === null || operation.status < 500) continue;
      const signature = 'backend:' + operation.operation;
      const message =
        'backend operation ' + operation.operation + ' returned HTTP ' + operation.status;
      const context = { capture: 'reproit-backend-node' };
      if (this._config.build !== null) context.build = { version: this._config.build };
      const payload = capturePayload(operation);
      if (payload === null) {
        context.captureOmitted = true;
      } else {
        context.reproitCapture = payload.value;
        if (payload.droppedEffects > 0) context.captureDroppedEffects = payload.droppedEffects;
      }
      frame({
        kind: 'finding',
        signature,
        message,
        identity: {
          oracle: SERVER_ERROR_ORACLE,
          invariant: 'backend:server-error',
          kind: 'server-error',
          message,
          frame: '',
          trigger: signature,
          boundary: signature,
        },
        path: [],
        context,
      });
    }
    const batch = {
      version: 1,
      batchId,
      appId: this._config.appId,
      frames,
      evidence: [],
    };
    if (this._config.build !== null) batch.deployment = { version: this._config.build };
    return batch;
  }

  async _send(batch) {
    const body = canonicalJson(batch);
    for (let attempt = 0; attempt <= this._config.retryLimit; attempt++) {
      try {
        const response = await fetch(this._config.endpoint, {
          method: 'POST',
          headers: {
            authorization: 'Bearer ' + this._config.apiKey,
            'content-type': 'application/json',
          },
          body,
          signal: AbortSignal.timeout(this._config.requestTimeoutMs),
        });
        if (response.ok) return true;
        // A definitive client-side rejection cannot improve on retry.
        if (response.status >= 400 && response.status < 500) return false;
      } catch (ignored) {
        // Network failure: retry below.
      }
      if (attempt < this._config.retryLimit) {
        await sleep(200 * attempt + 200);
      }
    }
    return false;
  }
}

// The replayable capture object (`reproit debug replay-capture` input).
// Trailing effect events are dropped first when the payload exceeds the
// context budget; a payload that stays oversized with only start/return
// left is omitted entirely (null).
function capturePayload(operation) {
  const events = operation.events.slice();
  let droppedEffects = 0;
  for (;;) {
    const value = {
      format: CAPTURE_FORMAT,
      version: CAPTURE_VERSION,
      operation: operation.operation,
      oracle: SERVER_ERROR_ORACLE,
      events,
    };
    if (Buffer.byteLength(canonicalJson(value)) <= MAX_CAPTURE_JSON_BYTES) {
      return { value, droppedEffects };
    }
    let lastEffect = -1;
    for (let index = events.length - 1; index >= 0; index--) {
      if (events[index] && events[index].kind === 'effect') {
        lastEffect = index;
        break;
      }
    }
    if (lastEffect < 0) return null;
    events.splice(lastEffect, 1);
    droppedEffects += 1;
  }
}

function sleep(ms) {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    if (timer.unref) timer.unref();
  });
}

module.exports = { Capture, CAPTURE_FORMAT, CAPTURE_VERSION, SERVER_ERROR_ORACLE };
