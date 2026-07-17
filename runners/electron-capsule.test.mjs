import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { installElectronWebSockets } from './electron.mjs';

test('Electron WebSocket replay preserves SEND/RECV order and stays offline', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'reproit-electron-ws-'));
  try {
    const path = join(dir, 'capsule.json');
    writeFileSync(
      path,
      JSON.stringify({
        exchanges: [
          {
            id: 'open',
            actor: 'a',
            actionIndex: 0,
            ordinal: 0,
            protocol: 'wss',
            method: 'RECV',
            url: 'wss://app.test/ws',
            status: 101,
            responseBody: { ready: true },
            required: true,
          },
          {
            id: 'send',
            actor: 'a',
            actionIndex: 0,
            ordinal: 1,
            protocol: 'wss',
            method: 'SEND',
            url: 'wss://app.test/ws',
            status: 101,
            requestBody: { ping: 1 },
            required: true,
          },
          {
            id: 'recv',
            actor: 'a',
            actionIndex: 0,
            ordinal: 2,
            protocol: 'wss',
            method: 'RECV',
            url: 'wss://app.test/ws',
            status: 101,
            responseBody: { pong: 1 },
            required: true,
          },
        ],
      }),
    );
    let route;
    await installElectronWebSockets(
      {
        routeWebSocket: async (_pattern, handler) => {
          route = handler;
        },
      },
      path,
    );
    const sent = [];
    let receive;
    route({
      url: () => 'wss://app.test/ws',
      send: (value) => sent.push(value),
      onMessage: (fn) => {
        receive = fn;
      },
      close: () => assert.fail('closed'),
    });
    await new Promise((resolve) => queueMicrotask(resolve));
    assert.deepEqual(sent, ['{"ready":true}']);
    receive('{"ping":1}');
    assert.deepEqual(sent, ['{"ready":true}', '{"pong":1}']);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
