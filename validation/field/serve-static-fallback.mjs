#!/usr/bin/env node

import { createReadStream, statSync } from 'node:fs';
import { createServer } from 'node:http';
import { extname, resolve, sep } from 'node:path';

const MAX_PATH_LENGTH = 2048;
const root = resolve(process.argv[2] || '');
const port = Number(process.argv[3] || 4173);
if (!process.argv[2] || !Number.isInteger(port) || port < 1024 || port > 65535)
  throw new Error('usage: serve-static-fallback.mjs ROOT [PORT]');
if (!statSync(root).isDirectory())
  throw new Error(`not a directory: ${root}`);

const contentTypes = new Map([
  ['.css', 'text/css'],
  ['.html', 'text/html; charset=utf-8'],
  ['.js', 'text/javascript'],
  ['.json', 'application/json'],
  ['.png', 'image/png'],
  ['.svg', 'image/svg+xml'],
  ['.wasm', 'application/wasm'],
  ['.woff2', 'font/woff2'],
]);

createServer((request, response) => {
  const pathname = decodeURIComponent(new URL(request.url, `http://127.0.0.1:${port}`).pathname);
  if (pathname.length > MAX_PATH_LENGTH) {
    response.writeHead(414).end('path too long');
    return;
  }
  let path = resolve(root, pathname.replace(/^[/\\]+/, ''));
  if (path !== root && !path.startsWith(`${root}${sep}`)) {
    response.writeHead(400).end('invalid path');
    return;
  }
  try {
    if (statSync(path).isDirectory())
      path = resolve(path, 'index.html');
    if (!statSync(path).isFile())
      throw new Error('not a file');
  } catch {
    path = resolve(root, 'index.html');
  }
  response.setHeader('cross-origin-embedder-policy', 'require-corp');
  response.setHeader('cross-origin-opener-policy', 'same-origin');
  response.setHeader('cross-origin-resource-policy', 'cross-origin');
  response.setHeader('content-type', contentTypes.get(extname(path)) || 'application/octet-stream');
  createReadStream(path).pipe(response);
}).listen(port, '127.0.0.1');
