/*!
 * Fastify plugin for reproit-backend-node.
 *
 * Scan-time: inert unless the request carries `x-reproit-trace`; the finished
 * trace is returned as the `x-reproit-events` response header. Production:
 * pass a Capture in the plugin options and every request is traced and handed
 * to the sampler instead. Handlers record observed effects via
 * `request.reproit`. Every adapter path fails closed.
 *
 * The trace begins in a preHandler hook so the decoded body, path params, and
 * query are all part of the canonical start input.
 */
'use strict';

const { BackendTrace, traceContextFromHeaders, httpInput } = require('./index.js');

// opts: { capture, operation(request), tenant(request), effectsComplete }
function reproitFastify(fastify, opts, done) {
  const capture = opts.capture ?? null;
  fastify.decorateRequest('reproit', null);

  fastify.addHook('preHandler', (request, reply, next) => {
    try {
      const header = (name) => {
        const value = request.headers[name];
        return Array.isArray(value) ? value[0] : value;
      };
      const scanContext = traceContextFromHeaders(header);
      const context = scanContext ?? (capture !== null ? capture.context() : null);
      if (context !== null) {
        const operation =
          typeof opts.operation === 'function'
            ? opts.operation(request)
            : request.method + ' ' + (request.routeOptions?.url ?? request.url.split('?')[0]);
        const trace = BackendTrace.begin(context, operation, {
          tenant: typeof opts.tenant === 'function' ? opts.tenant(request) : null,
          input: httpInput({
            body: request.body,
            path: request.params,
            query: request.query,
            headers: request.headers,
          }),
        });
        trace._scan = scanContext !== null;
        request.reproit = trace;
      }
    } catch (ignored) {
      // Fail closed: an instrumentation defect must not break the request.
    }
    next();
  });

  fastify.addHook('onSend', (request, reply, payload, next) => {
    try {
      const trace = request.reproit;
      if (trace && !trace.finished) {
        const status = reply.statusCode;
        let output = null;
        if (typeof payload === 'string') {
          const type = String(reply.getHeader('content-type') ?? '');
          if (type.includes('application/json')) {
            try {
              output = JSON.parse(payload);
            } catch (ignored) {
              output = null;
            }
          }
        }
        trace.finish(output, status, status < 500, opts.effectsComplete === true);
        if (trace._scan) {
          reply.header('x-reproit-events', trace.header());
        } else if (capture !== null) {
          capture.record(trace);
        }
      }
    } catch (ignored) {
      // Oversized or over-long traces drop their header; the response ships.
    }
    next(null, payload);
  });

  done();
}

// Break plugin encapsulation (what fastify-plugin does) so the hooks apply to
// routes registered on the parent instance, without adding a dependency.
reproitFastify[Symbol.for('skip-override')] = true;

module.exports = reproitFastify;
module.exports.reproitFastify = reproitFastify;
