import { createHash } from 'node:crypto';

import { installCausalFetch, redactCausal, type CausalExchange } from '../src/causal';
import {
  boundedBody,
  boundedHeaders,
  buildHttpExchange,
  redactExchangeValue,
  sha256Hex,
  utf8Bytes,
  MAX_EXCHANGE_BODY_BYTES,
  type ProductionExchange,
} from '../src/exchange';

describe('production exchange capture', () => {
  const original = globalThis.fetch;
  const originalXhr = globalThis.XMLHttpRequest;
  afterEach(() => {
    globalThis.fetch = original;
    globalThis.XMLHttpRequest = originalXhr;
  });

  test('sha256 and utf8 sizing match node, so truncated identity is provable', () => {
    for (const sample of ['', 'hello', 'grüße', '😀 emoji', 'x'.repeat(9000)]) {
      expect(sha256Hex(sample)).toBe(createHash('sha256').update(sample, 'utf8').digest('hex'));
      expect(utf8Bytes(sample)).toBe(Buffer.byteLength(sample, 'utf8'));
    }
  });

  test('redaction uses the backend placeholder shape, not the marker string', () => {
    const redacted = redactExchangeValue({
      apiKey: 'raw-api',
      'access.key': 'raw-access',
      nested: { authorization: 'raw', ok: 1 },
      keyboardLayout: 'dvorak',
    }) as Record<string, { $reproit?: unknown }>;
    expect(redacted.apiKey).toEqual({ $reproit: { redacted: true, type: 'string', length: 7 } });
    expect(redacted['access.key']).toEqual({
      $reproit: { redacted: true, type: 'string', length: 10 },
    });
    expect((redacted.nested as Record<string, unknown>).authorization).toEqual({
      $reproit: { redacted: true, type: 'string', length: 3 },
    });
    expect((redacted.nested as Record<string, unknown>).ok).toBe(1);
    expect(redacted.keyboardLayout).toBe('dvorak');
    expect(JSON.stringify(redacted)).not.toMatch(/raw-(api|access)/);
  });

  test('bodies are bounded: over budget keeps identity and drops content', () => {
    const big = 'x'.repeat(MAX_EXCHANGE_BODY_BYTES + 1);
    const bounded = boundedBody(big, 'text/plain');
    expect(bounded.truncated).toBe(true);
    expect(bounded.bodyBytes).toBe(MAX_EXCHANGE_BODY_BYTES + 1);
    expect(bounded.bodySha256).toBe(sha256Hex(big));
    expect(bounded.body).toBeUndefined();
    expect(boundedBody('', 'text/plain')).toEqual({});
    expect(boundedBody('{"a":1}', 'application/json')).toEqual({ body: { a: 1 } });
    expect(boundedBody('not json', 'application/json')).toEqual({ body: 'not json' });
  });

  test('headers are capped at 32 and lowercased, secrets replaced', () => {
    const many: Record<string, string> = {};
    for (let index = 0; index < 40; index += 1) many[`H${index}`] = String(index);
    const bounded = boundedHeaders(many).headers as Record<string, string>;
    expect(Object.keys(bounded)).toHaveLength(32);
    expect(bounded.h0).toBe('0');
    const secret = boundedHeaders({ Authorization: 'Bearer raw' }).headers as Record<string, string>;
    expect(secret.authorization).toBe('<reproit:secret>');
  });

  test('an assembled exchange carries request and response verbatim modulo redaction', () => {
    const exchange = buildHttpExchange(
      {
        method: 'POST',
        url: 'https://api.test/orders',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ sku: 'widget', token: 'raw' }),
      },
      {
        status: 502,
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ error: 'upstream down' }),
      },
    );
    expect(exchange.protocol).toBe('http');
    expect(exchange.request.method).toBe('POST');
    expect((exchange.request.body as Record<string, unknown>).sku).toBe('widget');
    expect((exchange.request.body as Record<string, unknown>).token).toEqual({
      $reproit: { redacted: true, type: 'string', length: 3 },
    });
    expect(exchange.response.status).toBe(502);
    expect((exchange.response.body as Record<string, unknown>).error).toBe('upstream down');
  });

  test('the production sink records only when supplied, and stays silent on the console', async () => {
    globalThis.fetch = jest.fn(
      async () =>
        new Response(JSON.stringify({ prices: null }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
    ) as typeof fetch;
    const recorded: ProductionExchange[] = [];
    const lines: string[] = [];
    const uninstall = installCausalFetch({
      actionIndex: () => 1,
      emit: (line) => lines.push(line),
      emitMarker: false,
      record: (exchange) => recorded.push(exchange),
    });
    await globalThis.fetch!('https://api.test/prices?tier=gold');
    uninstall();
    expect(recorded).toHaveLength(1);
    expect(recorded[0].request.url).toBe('https://api.test/prices?tier=gold');
    expect(recorded[0].response.status).toBe(200);
    expect(recorded[0].response.body).toEqual({ prices: null });
    expect(typeof recorded[0].at).toBe('number');
    // The runner's console protocol stays out of a shipping app.
    expect(lines).toHaveLength(0);
  });

  test('without a record sink nothing is captured and the marker path is unchanged', async () => {
    globalThis.fetch = jest.fn(
      async () => new Response('{}', { status: 200, headers: { 'content-type': 'application/json' } }),
    ) as typeof fetch;
    const lines: string[] = [];
    const uninstall = installCausalFetch({ actionIndex: () => 1, emit: (line) => lines.push(line) });
    await globalThis.fetch!('https://api.test/x');
    uninstall();
    expect(lines.some((line) => line.startsWith('REPROIT:EXCHANGE '))).toBe(true);
  });

  test('replay stays fail closed and never records when a capsule is present', async () => {
    const exchange: CausalExchange = {
      id: 'a-0-0',
      actor: 'a',
      actionIndex: 0,
      ordinal: 0,
      protocol: 'https',
      method: 'GET',
      url: 'https://app.test/config',
      requestHeaders: {},
      status: 200,
      responseHeaders: { 'content-type': 'application/json' },
      responseBody: { enabled: true },
      required: true,
    };
    globalThis.fetch = jest.fn(async () => {
      throw new Error('live network must not run');
    }) as typeof fetch;
    const recorded: ProductionExchange[] = [];
    installCausalFetch({
      actionIndex: () => 0,
      capsule: { exchanges: [exchange] },
      emit: () => {},
      record: (item) => recorded.push(item),
    });
    expect(await (await globalThis.fetch!('https://app.test/config')).json()).toEqual({
      enabled: true,
    });
    await expect(globalThis.fetch!('https://app.test/other')).rejects.toThrow('CAPSULE:MISS');
    expect(recorded).toHaveLength(0);
  });

  test('the marker redaction contract is untouched by the new path', () => {
    expect(redactCausal({ token: 'raw' })).toEqual({ token: '<reproit:string:length=3>' });
  });
});
