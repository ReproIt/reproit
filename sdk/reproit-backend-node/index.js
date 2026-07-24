/*!
 * reproit-backend-node, experimental backend trace adapter (v0.0.0)
 *
 * Node port of sdk/reproit-backend-rs. Scan-time: services activate this
 * adapter only when a trusted request carries `x-reproit-trace`. The resulting
 * response header (`x-reproit-events`) contains bounded, trace-bound,
 * structurally redacted events. Production: the optional, config-gated capture
 * mode (capture.js) self-samples finished traces (always on 5xx / failure,
 * optional healthy baseline) and posts them to Cloud ingest. It is not a
 * public compatibility surface while backend contracts remain experimental.
 *
 * Wire parity with the Rust adapter: events serialize as compact JSON with
 * recursively sorted keys (serde_json's BTreeMap order), and the header is
 * unpadded base64url of that encoding.
 */
'use strict';

const crypto = require('crypto');

const MAX_EVENTS = 256;
const MAX_HEADER_BYTES = 60000;
const EFFECT_KINDS = ['read', 'write', 'delete', 'emit', 'call'];

let sequenceCounter = 1;

class TraceError extends Error {
  constructor(code) {
    super('reproit trace rejected input: ' + code);
    this.name = 'TraceError';
    this.code = code; // InvalidOperation | AlreadyFinished | TooManyEvents | HeaderTooLarge
  }
}

// Trimmed, non-empty, at most `maximum` code points; null otherwise.
function bounded(value, maximum) {
  if (typeof value !== 'string') return null;
  const trimmed = value.trim();
  if (trimmed.length === 0 || [...trimmed].length > maximum) return null;
  return trimmed;
}

// `get(name)` returns the request header value (or undefined). Returns null
// when no valid `x-reproit-trace` is present: the adapter stays inert.
function traceContextFromHeaders(get) {
  const raw = get('x-reproit-trace');
  const traceId = raw === undefined || raw === null ? null : bounded(String(raw), 128);
  if (traceId === null) return null;
  const header = (name, maximum) => {
    const value = get(name);
    return value === undefined || value === null ? null : bounded(String(value), maximum);
  };
  const action = get('x-reproit-action');
  const parsed = action === undefined || action === null ? NaN : Number(String(action).trim());
  const actionIndex =
    Number.isInteger(parsed) && parsed >= 0 && parsed <= 0xffffffff ? parsed : 0;
  return {
    traceId,
    actor: header('x-reproit-actor', 32),
    actionIndex,
    build: header('x-reproit-build', 128),
    configContract: header('x-reproit-config-contract', 128),
  };
}

// GraphQL selection mapping (parser-produced only). Returns null on an
// invalid path, matching the Rust constructor.
function selection(schemaPath, responsePath, typeCondition) {
  if (!validPath(schemaPath) || !validPath(responsePath)) return null;
  const value = { schemaPath, responsePath };
  if (typeCondition !== undefined && typeCondition !== null) {
    const invalid =
      !validPath(typeCondition) || typeCondition.includes('.') || typeCondition.includes('[]');
    if (invalid) return null;
    value.typeCondition = typeCondition;
  }
  return value;
}

function validPath(path) {
  if (typeof path !== 'string' || path.length === 0) return false;
  return path.split('.').every((segment) => {
    const name = segment.endsWith('[]') ? segment.slice(0, -2) : segment;
    return /^[A-Za-z_][A-Za-z0-9_]*$/.test(name);
  });
}

// Canonical decoded OpenAPI input. Framework adapters must provide decoded
// values (including arrays for repeated query/header parameters), never raw
// query strings whose serialization style is ambiguous.
function httpInput(parts) {
  const value = {};
  if (parts.body !== undefined && parts.body !== null) value.body = parts.body;
  for (const name of ['path', 'query', 'headers']) {
    const fields = parts[name];
    if (!fields || typeof fields !== 'object') continue;
    const entries = Object.entries(fields).map(([key, field]) =>
      name === 'headers' ? [key.toLowerCase(), field] : [key, field],
    );
    if (entries.length > 0) value[name] = Object.fromEntries(entries);
  }
  return value;
}

class BackendTrace {
  // opts: { spanId, tenant, idempotencyKey, input, selections }
  static begin(context, operation, opts = {}) {
    const name = bounded(String(operation), 256);
    if (name === null) throw new TraceError('InvalidOperation');
    const spanId = bounded(String(opts.spanId ?? context.traceId + ':' + name), 128);
    if (spanId === null) throw new TraceError('InvalidOperation');
    const common = {
      traceId: context.traceId,
      spanId,
      actionIndex: context.actionIndex,
      operation: name,
    };
    if (context.actor) common.actor = context.actor;
    if (context.build) common.build = context.build;
    if (context.configContract) common.configContract = context.configContract;
    const tenant = opts.tenant == null ? null : bounded(String(opts.tenant), 128);
    if (tenant !== null) common.tenant = tenant;
    if (opts.idempotencyKey != null) common.idempotencyKey = identity(String(opts.idempotencyKey));
    if (Array.isArray(opts.selections) && opts.selections.length > 0) {
      common.selections = opts.selections.slice(0, MAX_EVENTS);
    }
    const trace = new BackendTrace(common);
    trace._push('start', { input: redact(opts.input ?? null) });
    return trace;
  }

  constructor(common) {
    this._common = common;
    this._events = [];
    this._finished = false;
  }

  // opts: { resource, key, tenant, event, detail }
  effect(kind, opts = {}) {
    if (this._finished) throw new TraceError('AlreadyFinished');
    if (!EFFECT_KINDS.includes(kind)) throw new TraceError('InvalidOperation');
    const fields = { effect: kind };
    for (const [name, value] of [
      ['resource', opts.resource],
      ['key', opts.key],
      ['effectTenant', opts.tenant],
      ['event', opts.event],
    ]) {
      if (value !== undefined && value !== null) {
        fields[name] = [...String(value)].slice(0, 256).join('');
      }
    }
    if (opts.detail !== undefined && opts.detail !== null) {
      const detail = redact(opts.detail);
      if (detail && typeof detail === 'object' && !Array.isArray(detail)) {
        for (const key of ['before', 'after', 'payload']) {
          if (key in detail) fields[key] = detail[key];
        }
      }
    }
    this._push('effect', fields);
  }

  finish(output, status, success, effectsComplete) {
    if (this._finished) throw new TraceError('AlreadyFinished');
    this._push('return', {
      output: redact(output ?? null),
      status,
      success: success === true,
      effectsComplete: effectsComplete === true,
    });
    this._finished = true;
  }

  header() {
    if (!this._finished) throw new TraceError('AlreadyFinished');
    const encoded = Buffer.from(canonicalJson(this._events)).toString('base64url');
    if (encoded.length > MAX_HEADER_BYTES) throw new TraceError('HeaderTooLarge');
    return encoded;
  }

  events() {
    return this._events;
  }

  get finished() {
    return this._finished;
  }

  _push(kind, fields) {
    if (this._events.length >= MAX_EVENTS) throw new TraceError('TooManyEvents');
    this._events.push({ ...this._common, sequence: sequenceCounter++, kind, ...fields });
  }
}

// Compact JSON with recursively sorted object keys: byte-identical to the
// Rust adapter's serde_json (BTreeMap) encoding of the same events.
function canonicalJson(value) {
  if (value === null || value === undefined) return 'null';
  if (Array.isArray(value)) return '[' + value.map(canonicalJson).join(',') + ']';
  if (typeof value === 'object') {
    const body = Object.keys(value)
      .filter((key) => value[key] !== undefined)
      .sort()
      .map((key) => JSON.stringify(key) + ':' + canonicalJson(value[key]));
    return '{' + body.join(',') + '}';
  }
  return JSON.stringify(value);
}

// Hashed identity for idempotency keys: never ship the raw key.
function identity(value) {
  const digest = crypto.createHash('sha256').update(value, 'utf8').digest();
  return 'sha256:' + digest.subarray(0, 12).toString('hex');
}

const SECRET_PARTS = [
  'password',
  'passwd',
  'secret',
  'token',
  'authorization',
  'cookie',
  'email',
  'phone',
  'apikey',
  'publishablekey',
  'privatekey',
  'accesskey',
  'signingkey',
  'idempotencykey',
];

function secretField(name) {
  const folded = name.replace(/[^A-Za-z0-9]/g, '').toLowerCase();
  return SECRET_PARTS.some((part) => folded.includes(part));
}

// Recursive structural redaction: secret-named fields are replaced with a
// `$reproit` metadata stub (type + length), everything else recurses.
function redact(value) {
  if (Array.isArray(value)) return value.map(redact);
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, field]) => [
        key,
        secretField(key) ? metadata(field) : redact(field),
      ]),
    );
  }
  return value === undefined ? null : value;
}

function metadata(value) {
  let kind = 'null';
  let length = null;
  if (typeof value === 'boolean') kind = 'boolean';
  else if (typeof value === 'number') kind = Number.isInteger(value) ? 'integer' : 'number';
  else if (typeof value === 'string') {
    kind = 'string';
    length = [...value].length;
  } else if (Array.isArray(value)) {
    kind = 'array';
    length = value.length;
  } else if (value !== null && typeof value === 'object') kind = 'object';
  return { $reproit: { redacted: true, type: kind, length } };
}

const capture = require('./capture.js');

module.exports = {
  MAX_EVENTS,
  MAX_HEADER_BYTES,
  BackendTrace,
  TraceError,
  traceContextFromHeaders,
  selection,
  httpInput,
  canonicalJson,
  redact,
  Capture: capture.Capture,
  CAPTURE_FORMAT: capture.CAPTURE_FORMAT,
  CAPTURE_VERSION: capture.CAPTURE_VERSION,
  SERVER_ERROR_ORACLE: capture.SERVER_ERROR_ORACLE,
};
