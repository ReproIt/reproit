import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('../src/init.js', import.meta.url), 'utf8');
function runtime(capsule, liveFetch) {
  const recorded = [];
  const window = {
    location: { href: 'tauri://localhost/' },
    fetch: liveFetch,
    __TAURI_INTERNALS__: {
      invoke: async (command, args) => {
        if (command.endsWith('|action_index')) return 1;
        if (command.endsWith('|record_exchange')) {
          recorded.push(JSON.parse(args.line));
          return null;
        }
        throw new Error(command);
      },
    },
  };
  const script = source
    .replace('__REPROIT_CAPSULE_LITERAL__', JSON.stringify(capsule ? JSON.stringify(capsule) : ''))
    .replace('__REPROIT_ACTOR_LITERAL__', JSON.stringify('a'));
  vm.runInContext(script, vm.createContext({ window, Headers, Response, URL, console }));
  return { window, recorded };
}

test('document-start transport captures redacted JSON', async () => {
  const { window, recorded } = runtime(
    null,
    async () =>
      new Response(JSON.stringify({ email: 'a@b.c', ok: true }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
  );
  await window.fetch('https://api.test/send', {
    method: 'POST',
    headers: { authorization: 'raw', 'content-type': 'application/json' },
    body: JSON.stringify({
      token: 'raw',
      apiKey: 'raw-api',
      'publishable-key': 'raw-pub',
      private_key: 'raw-private',
      'access.key': 'raw-access',
      'signing key': 'raw-signing',
      keyboardLayout: 'dvorak',
      key: 'ordinary',
      kind: 'message',
    }),
  });
  assert.equal(recorded.length, 1);
  assert.equal(recorded[0].requestHeaders.authorization, '<reproit:secret>');
  assert.equal(recorded[0].requestBody.token, '<reproit:string:length=3>');
  for (const name of ['apiKey', 'publishable-key', 'private_key', 'access.key', 'signing key'])
    assert.match(recorded[0].requestBody[name], /^<reproit:string:length=/);
  assert.equal(recorded[0].requestBody.keyboardLayout, 'dvorak');
  assert.equal(recorded[0].requestBody.key, 'ordinary');
  assert.doesNotMatch(JSON.stringify(recorded[0]), /raw-(api|pub|private|access|signing)/);
  assert.equal(recorded[0].responseBody.email, '<reproit:string:length=5>');
});

test('document-start transport replays and rejects unmatched requests', async () => {
  const capsule = {
    exchanges: [
      {
        id: 'a-1-0',
        actor: 'a',
        actionIndex: 1,
        ordinal: 0,
        protocol: 'https',
        method: 'GET',
        url: 'https://api.test/config?a=1&b=2',
        status: 200,
        responseHeaders: { 'content-type': 'application/json' },
        responseBody: { enabled: true },
        required: true,
      },
    ],
  };
  const { window } = runtime(capsule, async () => {
    throw new Error('live network used');
  });
  assert.deepEqual(await (await window.fetch('https://api.test/config?b=2&a=1')).json(), {
    enabled: true,
  });
  await assert.rejects(window.fetch('https://api.test/miss'), /CAPSULE:MISS/);
});

test(
  'XMLHttpRequest is captured and replayed through the same fail-closed ' + 'adapter',
  async () => {
    const capsule = {
      exchanges: [
        {
          id: 'a-1-0',
          actor: 'a',
          actionIndex: 1,
          ordinal: 0,
          protocol: 'https',
          method: 'POST',
          url: 'https://api.test/x?a=1&b=2',
          status: 201,
          responseHeaders: { 'content-type': 'application/json' },
          responseBody: { ok: true },
          required: true,
        },
      ],
    };
    const { window } = runtime(capsule, async () => {
      throw new Error('live network used');
    });
    const xhr = new window.XMLHttpRequest();
    xhr.responseType = 'json';
    const done = new Promise((resolve, reject) => {
      xhr.onload = resolve;
      xhr.onerror = () => reject(xhr._error);
    });
    xhr.open('POST', 'https://api.test/x?b=2&a=1');
    xhr.send(JSON.stringify({ value: 1 }));
    await done;
    assert.equal(xhr.status, 201);
    assert.equal(JSON.stringify(xhr.response), '{"ok":true}');
    const miss = new window.XMLHttpRequest();
    const failed = new Promise((resolve) => {
      miss.onerror = resolve;
    });
    miss.open('GET', 'https://api.test/miss');
    miss.send();
    await failed;
    assert.match(String(miss._error), /CAPSULE:MISS/);
  },
);
