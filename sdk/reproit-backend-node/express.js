/*!
 * Express middleware for reproit-backend-node.
 *
 * Scan-time: inert unless the request carries `x-reproit-trace`; the finished
 * trace is returned as the `x-reproit-events` response header. Production:
 * pass a Capture and every request is traced and handed to the sampler
 * instead. Handlers record observed effects via `req.reproit`. Every adapter
 * path fails closed: instrumentation errors never reach the host app.
 *
 * Mount AFTER body parsing (express.json()) so the start event sees the
 * decoded body. `req.params` is not populated before routing, so path
 * parameters are not part of the canonical input here.
 */
'use strict';

const { BackendTrace, traceContextFromHeaders, httpInput } = require('./index.js');

// options: { capture, operation(req), tenant(req), effectsComplete }
function reproitExpress(options = {}) {
  const capture = options.capture ?? null;
  return function reproit(req, res, next) {
    try {
      instrument(req, res, options, capture);
    } catch (ignored) {
      // Fail closed: an instrumentation defect must not break the request.
    }
    next();
  };
}

function instrument(req, res, options, capture) {
  const header = (name) => {
    const value = req.headers[name];
    return Array.isArray(value) ? value[0] : value;
  };
  const scanContext = traceContextFromHeaders(header);
  const context = scanContext ?? (capture !== null ? capture.context() : null);
  if (context === null) return;
  const operation =
    typeof options.operation === 'function'
      ? options.operation(req)
      : req.method + ' ' + req.path;
  const trace = BackendTrace.begin(context, operation, {
    tenant: typeof options.tenant === 'function' ? options.tenant(req) : null,
    input: httpInput({
      body: req.body,
      query: req.query,
      headers: req.headers,
    }),
  });
  req.reproit = trace;

  let output = null;
  const json = res.json.bind(res);
  res.json = function (body) {
    output = body;
    return json(body);
  };

  let finalized = false;
  const finalize = (status) => {
    if (finalized || trace.finished) return;
    finalized = true;
    trace.finish(output, status, status < 500, options.effectsComplete === true);
    if (scanContext !== null) {
      res.setHeader('x-reproit-events', trace.header());
    } else {
      capture.record(trace);
    }
  };
  const writeHead = res.writeHead.bind(res);
  res.writeHead = function (...args) {
    try {
      finalize(typeof args[0] === 'number' ? args[0] : res.statusCode);
    } catch (ignored) {
      // Oversized or over-long traces drop their header; the response ships.
    }
    return writeHead(...args);
  };
}

module.exports = reproitExpress;
module.exports.reproitExpress = reproitExpress;
