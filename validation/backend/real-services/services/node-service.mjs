import { createServer } from 'node:http';

const mode = process.env.REPROIT_FIXTURE_MODE ?? 'clean';
const port = Number(process.env.PORT ?? 19480);
const tag = '"fixture-v1"';
const json = (response, status, value, headers = {}) => {
  const body = Buffer.from(JSON.stringify(value));
  response.writeHead(status, { 'content-type': 'application/json', ...headers });
  response.end(body);
};

createServer((request, response) => {
  const url = new URL(request.url, 'http://fixture.invalid');
  if (url.pathname === '/health') return json(response, 200, { ready: true });
  if (url.pathname === '/codec') {
    const typed = url.searchParams.get('value');
    const decoded = mode === 'broken' ? String(Number(typed)) : typed;
    return json(response, 200, mode === 'incomplete' ? {} : { decoded });
  }
  if (url.pathname === '/representation') {
    const headers = { etag: tag, vary: 'accept-language' };
    if (request.headers['if-none-match'] === tag) {
      if (mode === 'broken') {
        response.writeHead(200, { 'content-type': 'text/plain', ...headers });
        response.end('contradictory-v2');
        return;
      }
      response.writeHead(304, headers);
      response.end();
      return;
    }
    response.writeHead(200, { 'content-type': 'text/plain', ...headers });
    response.end('authoritative-v1');
    return;
  }
  if (url.pathname === '/media') {
    response.writeHead(200, mode === 'incomplete' ? {} : { 'content-type': 'application/json' });
    response.end(mode === 'broken' ? '{invalid-json' : '{"ok":true}');
    return;
  }
  if (url.pathname === '/lifecycle') {
    const events = mode === 'broken'
      ? ['request.start', 'request.close', 'callback']
      : ['request.start', 'callback', 'request.close'];
    return json(response, 200, {
      complete: mode !== 'incomplete',
      scopeKind: 'request', scopeId: 'scope-1', events: events.map((name, index) => ({
        sequence: index + 1, name, scopeId: 'scope-1'
      }))
    });
  }
  response.writeHead(404).end();
}).listen(port, '127.0.0.1', () => console.log(`READY ${port}`));
