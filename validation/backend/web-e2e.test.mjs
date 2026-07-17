import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import test from 'node:test';
import { createRequire } from 'node:module';
import { decodeBackendEventHeader, installBackendCorrelation } from '../../runners/web/runner.mjs';
import { beginBackendTrace } from './sdk-node.mjs';

const require = createRequire(new URL('../../runners/web/package.json', import.meta.url));
const { chromium } = require('playwright');

test('browser request, service evidence, and trace validation work end to ' + 'end', async (t) => {
  const server = createServer((req, res) => {
    if (req.url === '/') {
      res.writeHead(200, { 'content-type': 'text/html' });
      res.end(
        '<button id="send">Send</button><script>send.onclick=()=>fetch("/api/' +
          'messages",{method:"POST",headers:{"content-type":"application/json"},' +
          'body:"{\\"body\\":\\"hello\\"}"})</script>',
      );
      return;
    }
    if (req.url === '/api/messages') {
      const trace = beginBackendTrace(req.headers, {
        operation: 'createMessage',
        tenant: 'team-a',
        input: { body: 'hello' },
      });
      assert.ok(trace, 'the browser must inject backend correlation');
      trace.effect('write', { resource: 'messages', key: 'm1', tenant: 'team-a' });
      trace.effect('emit', { event: 'MessageCreated' });
      trace.finish({ id: 'm1' }, 201, true, true);
      res.writeHead(201, {
        'content-type': 'application/json',
        'x-reproit-events': trace.header(),
      });
      res.end('{"id":"m1"}');
      return;
    }
    res.writeHead(404).end();
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(() => server.close());
  const { port } = server.address();
  const origin = `http://127.0.0.1:${port}`;
  const browser = await chromium.launch({ headless: true });
  t.after(() => browser.close());
  const context = await browser.newContext();
  await installBackendCorrelation(context, true, {
    appOrigin: origin,
    actor: 'alice',
    actionIndex: () => 4,
  });
  const page = await context.newPage();
  const responsePromise = page.waitForResponse((response) =>
    response.url().endsWith('/api/messages'),
  );
  await page.goto(origin);
  await page.click('#send');
  const response = await responsePromise;
  const responseHeaders = await response.allHeaders();
  const requestHeaders = response.request().headers();
  assert.equal(requestHeaders['x-reproit-actor'], 'alice');
  assert.equal(requestHeaders['x-reproit-action'], '4');
  const events = decodeBackendEventHeader(
    responseHeaders['x-reproit-events'],
    requestHeaders['x-reproit-trace'],
    requestHeaders['x-reproit-action'],
    requestHeaders['x-reproit-actor'],
  );
  assert.deepEqual(
    events.map((event) => event.kind),
    ['start', 'effect', 'effect', 'return'],
  );
  assert.ok(events.every((event) => event.actionIndex === 4 && event.actor === 'alice'));
});
