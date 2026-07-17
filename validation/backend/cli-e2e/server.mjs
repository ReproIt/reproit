import { createServer } from 'node:http';
import { beginBackendTrace } from '../sdk-node.mjs';

const port = Number(process.env.PORT ?? 19877);
let statefulUser = null;
createServer((request, response) => {
  if (request.url === '/proof' && request.method === 'GET') {
    response.writeHead(200, { 'content-type': 'text/html' });
    response.end(`<!doctype html><html lang="en">
      <head><title>Proof contract gate</title></head><body>
      <h1>Proof contract gate</h1><script>fetch('/proof-auth')</script></body></html>`);
    return;
  }
  if (request.url === '/proof-auth' && request.method === 'GET') {
    const allowed = beginBackendTrace(request.headers, {
      operation: 'getOrder',
      tenant: 'team-a',
      input: { id: 'o1', revision: 'r1' },
      spanId: 'proof-allow',
    });
    const denied = beginBackendTrace(request.headers, {
      operation: 'getOrder',
      tenant: 'team-b',
      input: { id: 'o1', revision: 'r1' },
      spanId: 'proof-deny',
    });
    if (!allowed || !denied) {
      response.writeHead(500).end();
      return;
    }
    allowed.finish({ secret: 'owner-data' }, 200, true, true);
    if (process.env.VALID_RESPONSE === '1') denied.finish({}, 404, false, true);
    else denied.finish({ secret: 'leaked-data' }, 200, true, true);
    const encoded = Buffer.from(
      JSON.stringify([...allowed.events, ...denied.events]),
      'utf8',
    ).toString('base64url');
    response.writeHead(200, {
      'content-type': 'application/json',
      'x-reproit-events': encoded,
    });
    response.end('{}');
    return;
  }
  if (request.url === '/__reproit/reset' && request.method === 'POST') {
    statefulUser = null;
    response.writeHead(204).end();
    return;
  }
  if (request.url === '/stateful-users' && request.method === 'POST') {
    statefulUser = { id: 'user-42', name: 'Reproit' };
    response.writeHead(201, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ id: statefulUser.id }));
    return;
  }
  if (request.url === '/stateful-users/user-42' && request.method === 'GET') {
    if (!statefulUser) {
      response.writeHead(404).end();
      return;
    }
    const output = process.env.VALID_RESPONSE === '1' ? statefulUser : { id: statefulUser.id };
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify(output));
    return;
  }
  if (request.url === '/headless-message' && request.method === 'GET') {
    if (process.env.SERVER_ERROR === '1') {
      response.writeHead(500, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ error: 'controlled server failure' }));
      return;
    }
    const output = process.env.VALID_RESPONSE === '1' ? { id: 'message-1' } : { accepted: true };
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify(output));
    return;
  }
  if (request.url === '/finance' && request.method === 'GET') {
    const valid = process.env.VALID_RESPONSE === '1';
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(
      JSON.stringify({
        account: { exposure: valid ? 5 : 11, limit: 10 },
        ledger: { debits: 10, credits: valid ? 10 : 9 },
        status: 'accepted',
      }),
    );
    return;
  }
  if (request.url === '/') {
    response.writeHead(200, { 'content-type': 'text/html' });
    response.end(`<!doctype html><html lang="en"><head><title>Backend CLI gate</title></head><body>
      <h1>Backend contract fixture</h1>
      <button data-testid="create" aria-label="Create message">Create</button>
      <script>async function create() {
        await fetch('/messages', {
          method: 'POST',
          headers: {'content-type': 'application/json'},
          body: JSON.stringify({body: 'hello'}),
        });
      }
      document.querySelector('button').onclick = create;
      create();</script></body></html>`);
    return;
  }
  if (request.url === '/messages' && request.method === 'POST') {
    const trace = beginBackendTrace(request.headers, {
      operation: 'createMessage',
      input: { body: 'hello' },
    });
    if (!trace) {
      response.writeHead(500, { 'content-type': 'application/json' });
      response.end('{"error":"missing Reproit correlation"}');
      return;
    }
    const output = process.env.VALID_RESPONSE === '1' ? { id: 'message-1' } : { accepted: true };
    trace.finish(output, 201, true, false);
    response.writeHead(201, {
      'content-type': 'application/json',
      'x-reproit-events': trace.header(),
    });
    response.end(JSON.stringify(output));
    return;
  }
  response.writeHead(404).end();
}).listen(port, '127.0.0.1', () => {
  process.stdout.write(`ready http://127.0.0.1:${port}\n`);
});
