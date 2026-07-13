// Reference zero-dependency backend instrumentation for validation fixtures.
// This is intentionally not published as a package while backend mode remains
// experimental. Framework adapters only need to map their request and response
// header APIs onto beginBackendTrace and trace.header().

import { createHash } from 'node:crypto';

let sequence = 0;

const SECRET = /password|passwd|secret|token|authorization|cookie|email|phone|idempotency.?key/i;

function redact(value) {
  if (Array.isArray(value)) return value.map(redact);
  if (!value || typeof value !== 'object') return value;
  const out = {};
  for (const [key, child] of Object.entries(value)) {
    out[key] = SECRET.test(key)
      ? `<reproit:${typeof child === 'string' ? `string:length=${[...child].length}` : typeof child}>`
      : redact(child);
  }
  return out;
}

function header(headers, name) {
  if (!headers) return undefined;
  if (typeof headers.get === 'function') return headers.get(name);
  const found = Object.keys(headers).find((key) => key.toLowerCase() === name);
  return found ? headers[found] : undefined;
}

function identity(value) {
  return `sha256:${createHash('sha256').update(String(value)).digest('hex').slice(0, 24)}`;
}

export function beginBackendTrace(requestHeaders, options) {
  const traceId = header(requestHeaders, 'x-reproit-trace');
  if (!traceId) return null;
  const actor = header(requestHeaders, 'x-reproit-actor') || undefined;
  const actionIndex = Number(header(requestHeaders, 'x-reproit-action')) || 0;
  const operation = String(options.operation || '').slice(0, 256);
  if (!operation) throw new Error('backend operation is required');
  const spanId = String(options.spanId || `${traceId}:${operation}`).slice(0, 128);
  const common = {
    traceId,
    spanId,
    actionIndex,
    operation,
    ...(actor ? { actor } : {}),
    ...(options.tenant ? { tenant: String(options.tenant) } : {}),
    ...(options.idempotencyKey ? { idempotencyKey: identity(options.idempotencyKey) } : {}),
  };
  const events = [{ sequence: ++sequence, ...common, kind: 'start', input: redact(options.input ?? null) }];
  let returned = false;
  return {
    effect(effect, detail = {}) {
      if (returned) throw new Error('cannot record an effect after return');
      const safeDetail = redact(detail);
      if (Object.hasOwn(safeDetail, 'tenant')) {
        safeDetail.effectTenant = safeDetail.tenant;
        delete safeDetail.tenant;
      }
      events.push({ sequence: ++sequence, ...common, kind: 'effect', effect, ...safeDetail });
    },
    finish(output, status = 200, success = status >= 200 && status < 400, effectsComplete = false) {
      if (returned) throw new Error('backend trace already finished');
      returned = true;
      events.push({
        sequence: ++sequence,
        ...common,
        kind: 'return',
        output: redact(output),
        status,
        success,
        effectsComplete: Boolean(effectsComplete),
      });
    },
    header() {
      if (!returned) throw new Error('finish the backend trace before encoding evidence');
      const encoded = Buffer.from(JSON.stringify(events), 'utf8').toString('base64url');
      return encoded.length <= 60000 ? encoded : null;
    },
    events,
  };
}
