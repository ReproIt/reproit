import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { installCausalFetch, redactCausal } from '../causal.ts';

test('explicit key-shaped secrets redact without hiding ordinary keys', () => {
  const safe = redactCausal({
    apiKey: 'raw-api',
    'publishable-key': 'raw-pub',
    private_key: 'raw-private',
    'access.key': 'raw-access',
    'signing key': 'raw-signing',
    keyboardLayout: 'dvorak',
    key: 'ordinary',
  });
  assert.equal(safe.apiKey, '<reproit:string:length=7>');
  assert.equal(safe['publishable-key'], '<reproit:string:length=7>');
  assert.equal(safe.private_key, '<reproit:string:length=11>');
  assert.equal(safe['access.key'], '<reproit:string:length=10>');
  assert.equal(safe['signing key'], '<reproit:string:length=11>');
  assert.equal(safe.keyboardLayout, 'dvorak');
  assert.equal(safe.key, 'ordinary');
  assert.doesNotMatch(JSON.stringify(safe), /raw-(api|pub|private|access|signing)/);
});

test('TUI fetch capture uses side files and never the rendered PTY', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'reproit-tui-cap-'));
  const network = join(dir, 'network.ndjson');
  const action = join(dir, 'action.txt');
  const capabilities = join(dir, 'capabilities.json');
  writeFileSync(network, '');
  writeFileSync(action, '2');
  writeFileSync(capabilities, '{}');
  const prior = globalThis.fetch;
  const env = process.env;
  env.REPROIT_NETWORK_FILE = network;
  env.REPROIT_ACTION_FILE = action;
  env.REPROIT_CAPABILITIES_FILE = capabilities;
  env.REPROIT_DEVICE = 'b';
  globalThis.fetch = (async () =>
    new Response(JSON.stringify({ profile: { email: 'a@example.com' }, ok: true }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    })) as typeof fetch;
  try {
    const uninstall = installCausalFetch();
    await fetch('https://app.test/feed', {
      method: 'POST',
      headers: { authorization: 'raw' },
      body: JSON.stringify({ token: 'raw' }),
    });
    uninstall();
    const exchange = JSON.parse(readFileSync(network, 'utf8').trim());
    assert.equal(exchange.actor, 'b');
    assert.equal(exchange.actionIndex, 2);
    assert.equal(exchange.requestHeaders.authorization, '<reproit:secret>');
    assert.equal(exchange.requestBody.token, '<reproit:string:length=3>');
    assert.equal(exchange.responseBody.profile.email, '<reproit:string:length=13>');
    assert.equal(JSON.parse(readFileSync(capabilities, 'utf8')).http.status, 'captured');
  } finally {
    globalThis.fetch = prior;
    for (const key of [
      'REPROIT_NETWORK_FILE',
      'REPROIT_ACTION_FILE',
      'REPROIT_CAPABILITIES_FILE',
      'REPROIT_DEVICE',
    ])
      delete env[key];
    rmSync(dir, { recursive: true, force: true });
  }
});
