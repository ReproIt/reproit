import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(fileURLToPath(import.meta.url));
let delivered = false;

createServer(async (request, response) => {
  const url = new URL(request.url, 'http://127.0.0.1:4178');
  if (request.method === 'POST' && url.pathname === '/send') {
    delivered = true;
    response.writeHead(204).end();
    return;
  }
  if (request.method === 'POST' && url.pathname === '/reset') {
    delivered = false;
    response.writeHead(204).end();
    return;
  }
  if (url.pathname === '/state') {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ delivered }));
    return;
  }
  const name = url.pathname === '/' ? 'app.html' : url.pathname.slice(1);
  try {
    const body = await readFile(join(root, name));
    response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    response.end(body);
  } catch {
    response.writeHead(404).end('not found');
  }
}).listen(4178, '127.0.0.1');
