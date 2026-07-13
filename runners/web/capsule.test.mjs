import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { installCapsuleReplay, installWebSocketCausal } from './runner.mjs';

function request(url) {
  return {
    resourceType: () => 'fetch',
    url: () => url,
    method: () => 'GET',
  };
}

test('capsule replay fulfills exact bootstrap exchange and blocks an unmatched request', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'reproit-web-cap-'));
  try {
    const path = join(dir, 'capsule.json');
    writeFileSync(path, JSON.stringify({
      id: 'cap_test',
      exchanges: [{
        id: 'a-0-0', actor: 'a', actionIndex: 0, ordinal: 0, protocol: 'https',
        method: 'GET', url: 'https://app.test/api?a=1&b=2', status: 200,
        responseHeaders: { 'content-type': 'application/json' },
        responseBody: { items: [{ author: null }] }, required: true,
      }],
    }));
    let handler;
    const context = { route: async (_glob, fn) => { handler = fn; } };
    await installCapsuleReplay(context, path);
    let fulfilled;
    await handler({
      request: () => request('https://app.test/api?b=2&a=1'),
      fulfill: async (value) => { fulfilled = value; },
      abort: async () => { throw new Error('should not abort'); },
      continue: async () => { throw new Error('should not continue'); },
    });
    assert.equal(fulfilled.status, 200);
    assert.equal(fulfilled.body, '{"items":[{"author":null}]}');

    let aborted = false;
    await handler({
      request: () => request('https://app.test/api/unknown'),
      fulfill: async () => { throw new Error('should not fulfill'); },
      abort: async () => { aborted = true; },
      continue: async () => { throw new Error('should not continue'); },
    });
    assert.equal(aborted, true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('websocket capsule replays ordered JSON frames without a live connection', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'reproit-web-ws-'));
  try {
    const path = join(dir, 'capsule.json');
    writeFileSync(path, JSON.stringify({ exchanges: [
      { id: 'open', actor: 'a', actionIndex: 0, ordinal: 0, protocol: 'wss', method: 'RECV', url: 'wss://app.test/ws', status: 101, responseBody: { ready: true }, required: true },
      { id: 'send', actor: 'a', actionIndex: 0, ordinal: 1, protocol: 'wss', method: 'SEND', url: 'wss://app.test/ws', status: 101, requestBody: { ping: 1 }, required: true },
      { id: 'recv', actor: 'a', actionIndex: 0, ordinal: 2, protocol: 'wss', method: 'RECV', url: 'wss://app.test/ws', status: 101, responseBody: { pong: 1 }, required: true },
    ] }));
    let routeHandler;
    const context = { routeWebSocket: async (_pattern, handler) => { routeHandler = handler; } };
    await installWebSocketCausal(context, path);
    const sent = [];
    let receive;
    const socket = {
      url: () => 'wss://app.test/ws',
      send: (value) => sent.push(value),
      onMessage: (handler) => { receive = handler; },
      close: () => { throw new Error('unexpected close'); },
    };
    routeHandler(socket);
    await new Promise((resolve) => queueMicrotask(resolve));
    assert.deepEqual(sent, ['{"ready":true}']);
    receive('{"ping":1}');
    assert.deepEqual(sent, ['{"ready":true}', '{"pong":1}']);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
