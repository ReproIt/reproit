/*!
 * Outbound-exchange capture for reproit-backend-node.
 *
 * `install()` wraps the process's outbound HTTP clients (`http`/`https`
 * request and get, plus `globalThis.fetch`) so every dependency call made
 * while a request trace is ambient (`traceStorage`, set by the framework
 * adapters) is recorded on that trace as an `effect` event carrying an
 * `exchange`: the request the app sent and the response the dependency
 * returned. `wrapPg(pg)` does the same for the `pg` driver at the
 * `Client.prototype.query` boundary, which covers `Pool` too.
 *
 * The exchange is what deterministic local replay stubs, so responses are
 * captured verbatim up to a fixed inline budget; an over-budget body keeps
 * its byte count and sha256 and is marked truncated (replay fails closed on
 * it with a named reason instead of guessing). Every path fails closed the
 * other way at capture time: an instrumentation defect must never break the
 * host app's request.
 */
'use strict';

const crypto = require('crypto');
const { EventEmitter } = require('events');
const { Readable } = require('stream');
const { currentTrace } = require('./index.js');
const replay = require('./replay.js');

// Inline body budget per exchange side. Beyond it the body is dropped and
// only provable identity (byte count + sha256) remains.
const MAX_EXCHANGE_BODY_BYTES = 8 * 1024;
// Recorded response headers are capped to keep events bounded.
const MAX_EXCHANGE_HEADERS = 32;
// Rows recorded per pg result; beyond it the result is marked truncated.
const MAX_PG_ROWS = 64;

const state = {
  installed: false,
  // Hermetic replay session, present only when REPROIT_REPLAY names a
  // capture payload. In that mode the wrappers SERVE recorded exchanges
  // instead of recording live ones.
  session: null,
  stats: { capturedExchanges: 0, truncatedBodies: 0, failedCaptures: 0 },
};

// Bound one exchange body. `body` is a Buffer or string; JSON bodies are
// parsed so structural redaction in the trace layer sees fields, not text.
function boundedBody(body, contentType) {
  if (body === null || body === undefined) return {};
  const buffer = Buffer.isBuffer(body) ? body : Buffer.from(String(body), 'utf8');
  if (buffer.length === 0) return {};
  if (buffer.length > MAX_EXCHANGE_BODY_BYTES) {
    state.stats.truncatedBodies += 1;
    return {
      bodyBytes: buffer.length,
      bodySha256: crypto.createHash('sha256').update(buffer).digest('hex'),
      truncated: true,
    };
  }
  const text = buffer.toString('utf8');
  if (typeof contentType === 'string' && contentType.includes('application/json')) {
    try {
      return { body: JSON.parse(text) };
    } catch (ignored) {
      // Declared JSON that does not parse is recorded as text below.
    }
  }
  return { body: text };
}

function boundedHeaders(headers) {
  const entries = Object.entries(headers ?? {})
    .slice(0, MAX_EXCHANGE_HEADERS)
    .map(([name, value]) => [
      String(name).toLowerCase(),
      Array.isArray(value) ? value.join(', ') : String(value),
    ]);
  return entries.length === 0 ? {} : { headers: Object.fromEntries(entries) };
}

function recordHttpExchange(trace, request, response) {
  try {
    trace.effect('call', {
      resource: request.host,
      key: request.method + ' ' + request.path,
      exchange: {
        protocol: 'http',
        request: {
          method: request.method,
          url: request.url,
          ...boundedHeaders(request.headers),
          ...boundedBody(request.body, request.contentType),
        },
        response: {
          status: response.status,
          ...boundedHeaders(response.headers),
          ...boundedBody(response.body, response.contentType),
        },
      },
    });
    state.stats.capturedExchanges += 1;
  } catch (ignored) {
    // The trace may have finished or overflowed; the host request goes on.
    state.stats.failedCaptures += 1;
  }
}

// Collect a stream's chunks up to one byte past the inline budget; enough to
// know the true size class without holding unbounded memory. The sha256 runs
// over EVERY byte so truncated identity stays provable.
function bodyCollector() {
  const chunks = [];
  let bytes = 0;
  const hash = crypto.createHash('sha256');
  return {
    push(chunk) {
      const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk), 'utf8');
      bytes += buffer.length;
      hash.update(buffer);
      if (bytes <= MAX_EXCHANGE_BODY_BYTES) chunks.push(buffer);
    },
    result() {
      if (bytes === 0) return null;
      if (bytes > MAX_EXCHANGE_BODY_BYTES) {
        state.stats.truncatedBodies += 1;
        return { bodyBytes: bytes, bodySha256: hash.digest('hex'), truncated: true };
      }
      return Buffer.concat(chunks);
    },
  };
}

function headerValue(headers, name) {
  for (const [key, value] of Object.entries(headers ?? {})) {
    if (String(key).toLowerCase() === name) {
      return Array.isArray(value) ? value[0] : String(value);
    }
  }
  return null;
}

// Wrap one `http.request`-shaped function. Node accepts (url, options,
// callback) and (options, callback); normalizing everything through the
// module's own argument handling keeps this wrapper argument-agnostic: it
// only observes the returned ClientRequest and its response.
function wrapClientRequest(original, protocol) {
  return function reproitRequest(...args) {
    const clientRequest = original.apply(this, args);
    try {
      const trace = currentTrace();
      if (trace === null) return clientRequest;
      const requestBody = bodyCollector();
      const write = clientRequest.write.bind(clientRequest);
      clientRequest.write = function (chunk, ...rest) {
        try {
          if (chunk) requestBody.push(chunk);
        } catch (ignored) {
          state.stats.failedCaptures += 1;
        }
        return write(chunk, ...rest);
      };
      const end = clientRequest.end.bind(clientRequest);
      clientRequest.end = function (chunk, ...rest) {
        try {
          if (chunk && typeof chunk !== 'function') requestBody.push(chunk);
        } catch (ignored) {
          state.stats.failedCaptures += 1;
        }
        return end(chunk, ...rest);
      };
      clientRequest.on('response', (response) => {
        try {
          const responseBody = bodyCollector();
          response.on('data', (chunk) => {
            try {
              responseBody.push(chunk);
            } catch (ignored) {
              state.stats.failedCaptures += 1;
            }
          });
          response.on('end', () => {
            const host = clientRequest.getHeader('host') ?? clientRequest.host;
            const path = clientRequest.path ?? '/';
            const collected = requestBody.result();
            const collectedResponse = responseBody.result();
            recordHttpExchange(trace, {
              method: clientRequest.method,
              host: String(host ?? ''),
              path,
              url: protocol + '//' + String(host ?? '') + path,
              headers: clientRequest.getHeaders(),
              body: collected,
              contentType: String(clientRequest.getHeader('content-type') ?? ''),
            }, {
              status: response.statusCode,
              headers: response.headers,
              body: collectedResponse,
              contentType: headerValue(response.headers, 'content-type') ?? '',
            });
          });
        } catch (ignored) {
          state.stats.failedCaptures += 1;
        }
      });
    } catch (ignored) {
      state.stats.failedCaptures += 1;
    }
    return clientRequest;
  };
}

function wrapFetch(originalFetch) {
  return async function reproitFetch(input, init) {
    const trace = currentTrace();
    if (trace === null) return originalFetch(input, init);
    let requestMeta = null;
    try {
      const request = new Request(input, init);
      const url = new URL(request.url);
      let body = null;
      if (init && typeof init.body === 'string') body = init.body;
      requestMeta = {
        method: request.method,
        host: url.host,
        path: url.pathname + url.search,
        url: request.url,
        headers: Object.fromEntries(request.headers.entries()),
        body,
        contentType: request.headers.get('content-type') ?? '',
      };
    } catch (ignored) {
      state.stats.failedCaptures += 1;
    }
    const response = await originalFetch(input, init);
    if (requestMeta !== null) {
      try {
        const clone = response.clone();
        const text = await clone.text();
        recordHttpExchange(trace, requestMeta, {
          status: response.status,
          headers: Object.fromEntries(response.headers.entries()),
          body: text,
          contentType: response.headers.get('content-type') ?? '',
        });
      } catch (ignored) {
        state.stats.failedCaptures += 1;
      }
    }
    return response;
  };
}

// Effect kind for a SQL statement: reads stay reads so state oracles keep
// their meaning; everything else is a write.
function pgEffectKind(text) {
  const verb = String(text ?? '').trimStart().slice(0, 8).toUpperCase();
  return verb.startsWith('SELECT') || verb.startsWith('SHOW') ? 'read' : 'write';
}

function recordPgExchange(trace, text, values, outcome) {
  try {
    trace.effect(pgEffectKind(text), {
      resource: 'pg',
      key: String(text ?? '').slice(0, 256),
      exchange: {
        protocol: 'pg',
        request: {
          text: String(text ?? ''),
          ...(Array.isArray(values) && values.length > 0 ? { values } : {}),
        },
        response: outcome,
      },
    });
    state.stats.capturedExchanges += 1;
  } catch (ignored) {
    state.stats.failedCaptures += 1;
  }
}

function pgOutcome(result) {
  if (!result || typeof result !== 'object') return { rowCount: 0 };
  const rows = Array.isArray(result.rows) ? result.rows : [];
  const outcome = {
    command: typeof result.command === 'string' ? result.command : null,
    rowCount: Number.isInteger(result.rowCount) ? result.rowCount : rows.length,
    rows: rows.slice(0, MAX_PG_ROWS),
  };
  if (rows.length > MAX_PG_ROWS) outcome.truncated = true;
  return outcome;
}

// Patch `pg.Client.prototype.query`. Covers Pool (delegates to Client) and
// both promise and callback forms. Only the (text, values?) and ({text,
// values}) shapes are recorded; exotic forms (Query objects, cursors) pass
// through unrecorded rather than half-recorded.
function wrapPg(pg) {
  if (!pg || !pg.Client || !pg.Client.prototype || pg.Client.prototype.__reproitWrapped) {
    return pg;
  }
  const query = pg.Client.prototype.query;
  pg.Client.prototype.query = function reproitQuery(config, values, callback) {
    const trace = currentTrace();
    const text = typeof config === 'string' ? config : config && config.text;
    const params = Array.isArray(values) ? values : config && config.values;
    // Hermetic replay: serve the recorded result without touching a live
    // database. Divergence and recorded errors both surface as rejections.
    if (state.session !== null && typeof text === 'string') {
      const recorded = state.session.match('pg', {
        text,
        ...(Array.isArray(params) && params.length > 0 ? { values: params } : {}),
      });
      const outcome = recorded === null ? null : (recorded.response ?? {});
      let settle;
      if (outcome === null) {
        settle = Promise.reject(new Error('reproit: pg call diverged from the capture'));
      } else if (outcome.error) {
        const error = new Error(String(outcome.error.message ?? 'recorded pg error'));
        if (outcome.error.code) error.code = outcome.error.code;
        settle = Promise.reject(error);
      } else {
        settle = Promise.resolve({
          command: outcome.command ?? null,
          rowCount: outcome.rowCount ?? 0,
          rows: Array.isArray(outcome.rows) ? outcome.rows : [],
        });
      }
      const done = typeof callback === 'function' ? callback : values;
      if (typeof done === 'function') {
        settle.then(
          (result) => done(null, result),
          (error) => done(error),
        );
        return undefined;
      }
      return settle;
    }
    if (trace === null || typeof text !== 'string') {
      return query.call(this, config, values, callback);
    }
    const record = (error, result) => {
      if (error) {
        recordPgExchange(trace, text, params, {
          error: { message: String(error.message ?? error), code: error.code ?? null },
        });
      } else {
        recordPgExchange(trace, text, params, pgOutcome(result));
      }
    };
    const usesCallback = typeof callback === 'function' || typeof values === 'function';
    if (usesCallback) {
      const original = typeof callback === 'function' ? callback : values;
      const wrapped = (error, result) => {
        try {
          record(error, result);
        } catch (ignored) {
          state.stats.failedCaptures += 1;
        }
        return original(error, result);
      };
      return typeof callback === 'function'
        ? query.call(this, config, values, wrapped)
        : query.call(this, config, wrapped);
    }
    const promise = query.call(this, config, values, callback);
    if (promise && typeof promise.then === 'function') {
      promise.then(
        (result) => record(null, result),
        (error) => record(error, null),
      );
    }
    return promise;
  };
  pg.Client.prototype.__reproitWrapped = true;
  return pg;
}

// Normalize http.request-style arguments to {method, url, headers, body:
// null, callback}. Handles (url[, options][, cb]) and (options[, cb]).
function normalizeRequestArgs(protocol, args) {
  let url = null;
  let options = {};
  let callback = null;
  for (const arg of args) {
    if (typeof arg === 'string' || arg instanceof URL) url = new URL(String(arg));
    else if (typeof arg === 'function') callback = arg;
    else if (arg && typeof arg === 'object') options = arg;
  }
  if (url === null) {
    const host = options.hostname ?? options.host ?? 'localhost';
    const port = options.port ? ':' + options.port : '';
    url = new URL(protocol + '//' + host + port + (options.path ?? '/'));
  }
  return {
    method: String(options.method ?? 'GET').toUpperCase(),
    url: url.toString(),
    headers: options.headers ?? {},
    callback,
  };
}

// Replay-mode stand-in for ClientRequest: collects the written body, matches
// the recorded exchange on end(), and emits a synthesized IncomingMessage.
// No sockets are involved, so replay needs no live network at all.
class ReplayClientRequest extends EventEmitter {
  constructor(meta) {
    super();
    this._meta = meta;
    this._headers = { ...meta.headers };
    this._body = [];
    if (meta.callback) this.on('response', meta.callback);
  }

  setHeader(name, value) {
    this._headers[String(name).toLowerCase()] = value;
    return this;
  }

  getHeader(name) {
    return this._headers[String(name).toLowerCase()];
  }

  getHeaders() {
    return { ...this._headers };
  }

  write(chunk, encoding, done) {
    if (chunk) this._body.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk)));
    if (typeof encoding === 'function') encoding();
    else if (typeof done === 'function') done();
    return true;
  }

  end(chunk, encoding, done) {
    if (this._ended) return this;
    this._ended = true;
    if (chunk && typeof chunk !== 'function') this.write(chunk);
    const finish = [chunk, encoding, done].find((arg) => typeof arg === 'function');
    const bodyText = Buffer.concat(this._body).toString('utf8');
    const contentType = String(this.getHeader('content-type') ?? '');
    const served = replay.serveHttp(state.session, {
      method: this._meta.method,
      url: this._meta.url,
      ...(bodyText.length > 0 ? { body: replay.tryJson(bodyText, contentType) } : {}),
    });
    setImmediate(() => {
      const response = new Readable({ read() {} });
      response.statusCode = served.status;
      response.statusMessage = served.status === 599 ? 'Reproit Diverged' : 'OK';
      response.headers = served.headers;
      response.rawHeaders = Object.entries(served.headers).flat();
      this.emit('response', response);
      response.push(served.bodyText);
      response.push(null);
      if (finish) finish();
      this.emit('finish');
    });
    return this;
  }

  abort() {}
  destroy() {
    return this;
  }
  setTimeout() {
    return this;
  }
  once(name, listener) {
    return super.once(name, listener);
  }
}

function replayRequest(protocol, autoEnd = false) {
  return function reproitReplayRequest(...args) {
    const request = new ReplayClientRequest(normalizeRequestArgs(protocol, args));
    // `http.get` ends the request itself in real Node; mirror that, but let
    // an explicit earlier end() win (the _ended guard makes this idempotent).
    if (autoEnd) setImmediate(() => request.end());
    return request;
  };
}

function replayFetch() {
  return async function reproitReplayFetch(input, init) {
    const request = new Request(input, init);
    let body;
    if (init && typeof init.body === 'string') {
      body = replay.tryJson(init.body, request.headers.get('content-type') ?? '');
    }
    const served = replay.serveHttp(state.session, {
      method: request.method,
      url: request.url,
      ...(body === undefined ? {} : { body }),
    });
    return new Response(served.bodyText, {
      status: served.status,
      headers: served.headers,
    });
  };
}

// Install the outbound wrappers once, process-wide. Idempotent. With
// REPROIT_REPLAY set the wrappers serve the named capture instead of
// recording, and the process clock/RNG/TZ pin to the capture envelope.
function install() {
  if (state.installed) return state;
  const http = require('http');
  const https = require('https');
  const replayPath = process.env.REPROIT_REPLAY;
  if (typeof replayPath === 'string' && replayPath.length > 0) {
    state.session = replay.ReplaySession.load(replayPath);
    replay.pinEnvelope(state.session.envelope);
    http.request = replayRequest('http:');
    http.get = replayRequest('http:', true);
    https.request = replayRequest('https:');
    https.get = replayRequest('https:', true);
    globalThis.fetch = replayFetch();
  } else {
    http.request = wrapClientRequest(http.request, 'http:');
    http.get = wrapClientRequest(http.get, 'http:');
    https.request = wrapClientRequest(https.request, 'https:');
    https.get = wrapClientRequest(https.get, 'https:');
    if (typeof globalThis.fetch === 'function') {
      globalThis.fetch = wrapFetch(globalThis.fetch.bind(globalThis));
    }
  }
  state.installed = true;
  return state;
}

module.exports = {
  install,
  wrapPg,
  MAX_EXCHANGE_BODY_BYTES,
  MAX_EXCHANGE_HEADERS,
  // Pure bounds helpers, exported so the shared behavioral vectors in
  // sdk/capture-behavior-v1.json can exercise them directly. Node is the
  // reference implementation for those vectors.
  boundedBody,
  boundedHeaders,
  stats: () => ({ ...state.stats }),
};
