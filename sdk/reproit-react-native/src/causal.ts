export type CausalExchange = {
  id: string; actor: string; actionIndex: number; ordinal: number; protocol: string;
  method: string; url: string; requestHeaders: Record<string, string>;
  requestBody?: unknown; status: number; responseHeaders: Record<string, string>;
  responseBody?: unknown; required: boolean;
};

/** Read the guarded capsule injected by Appium through the autolinked native
 * runtime module. The global remains a test/embedded-host override. */
export function nativeCausalCapsule(): { exchanges?: CausalExchange[] } | undefined {
  const globalValue = (globalThis as { __reproit_capsule?: { exchanges?: CausalExchange[] } }).__reproit_capsule;
  if (globalValue) return globalValue;
  try {
    // eslint-disable-next-line @typescript-eslint/no-var-requires
    const native = require('react-native').NativeModules?.ReproItRuntime;
    const raw = native?.capsuleJson;
    return typeof raw === 'string' && raw.length ? JSON.parse(raw) : undefined;
  } catch { return undefined; }
}

const SECRET = /password|passwd|secret|token|authorization|cookie|email|phone/i;

function canonicalUrl(raw: string): string {
  try {
    const url = new URL(raw);
    const pairs = Array.from(url.searchParams.entries()).sort(([ak, av], [bk, bv]) => ak.localeCompare(bk) || av.localeCompare(bv));
    url.search = '';
    for (const [key, value] of pairs) url.searchParams.append(key, value);
    return url.toString();
  } catch { return raw; }
}

export function redactCausal(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(redactCausal);
  if (value && typeof value === 'object') {
    const src = value as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const key of Object.keys(src).sort()) {
      const child = src[key];
      out[key] = SECRET.test(key)
        ? `<reproit:${typeof child === 'string' ? `string:length=${[...child].length}` : typeof child}>`
        : redactCausal(child);
    }
    return out;
  }
  return value;
}

function headersOf(value: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (!value) return out;
  const h = value as { forEach?: (fn: (v: string, k: string) => void) => void };
  if (typeof h.forEach === 'function') h.forEach((v, k) => { out[k] = SECRET.test(k) ? '<reproit:secret>' : String(v); });
  else for (const [k, v] of Object.entries(value as Record<string, unknown>)) out[k] = SECRET.test(k) ? '<reproit:secret>' : String(v);
  return out;
}

/** Install once under the fuzzer. Capture is automatic for global fetch; replay
 * is fail-closed when a capsule is supplied. The SDK emits the universal marker
 * consumed by the Appium runner and Rust host. */
export function installCausalFetch(options: {
  actor?: string;
  actionIndex: () => number;
  emit?: (line: string) => void;
  capsule?: { exchanges?: CausalExchange[] };
  excludePrefix?: string | null;
}): () => void {
  const g = globalThis as { fetch?: typeof fetch; XMLHttpRequest?: typeof XMLHttpRequest };
  const original = g.fetch;
  const originalXhr = g.XMLHttpRequest;
  if (typeof original !== 'function') return () => {};
  const actor = options.actor ?? 'a';
  const emit = options.emit ?? ((line) => console.log(line));
  const used = new Set<number>();
  let ordinal = 0;
  let priorAction = options.actionIndex();
  g.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
    if (options.excludePrefix && url.startsWith(options.excludePrefix)) return original(input, init);
    const actionIndex = options.actionIndex();
    if (actionIndex !== priorAction) { ordinal = 0; priorAction = actionIndex; }
    const method = String(init?.method || (typeof input !== 'string' && !(input instanceof URL) ? input.method : 'GET')).toUpperCase();
    const thisOrdinal = ordinal++;
    const exchanges = options.capsule?.exchanges || [];
    if (options.capsule) {
      const idx = exchanges.findIndex((e, i) => !used.has(i) && e.required && e.actor === actor && e.actionIndex === actionIndex && e.method.toUpperCase() === method && canonicalUrl(e.url) === canonicalUrl(url));
      if (idx < 0) throw new Error(`CAPSULE:MISS ${method} ${url} action=${actionIndex}`);
      used.add(idx);
      const e = exchanges[idx];
      emit(`CAPSULE:HIT ${e.id}`);
      return new Response(typeof e.responseBody === 'string' ? e.responseBody : JSON.stringify(e.responseBody ?? ''), { status: e.status, headers: e.responseHeaders });
    }
    const response = await original(input, init);
    let responseBody: unknown;
    try {
      const contentType = response.headers.get('content-type') || '';
      responseBody = /json/i.test(contentType) ? redactCausal(await response.clone().json()) : `<reproit:body>`;
    } catch { responseBody = '<reproit:invalid-json>'; }
    let requestBody: unknown;
    if (typeof init?.body === 'string') {
      try { requestBody = redactCausal(JSON.parse(init.body)); } catch { requestBody = `<reproit:body:length=${init.body.length}>`; }
    }
    const exchange: CausalExchange = {
      id: `${actor}-${actionIndex}-${thisOrdinal}`, actor, actionIndex, ordinal: thisOrdinal,
      protocol: url.split(':', 1)[0], method, url, requestHeaders: headersOf(init?.headers),
      ...(requestBody === undefined ? {} : { requestBody }), status: response.status,
      responseHeaders: headersOf(response.headers), responseBody, required: true,
    };
    emit(`REPROIT:EXCHANGE ${JSON.stringify(exchange)}`);
    return response;
  }) as typeof fetch;
  // Direct XMLHttpRequest is common in React Native dependencies. During a
  // Reproit run, translate it through the guarded fetch path so those requests
  // cannot bypass capture or escape to live network during replay.
  if (originalXhr) {
    class ReproItXMLHttpRequest {
      readyState = 0; status = 0; responseText = ''; response: unknown = null; responseType: XMLHttpRequestResponseType = '';
      onreadystatechange: ((event: Event) => void) | null = null; onload: ((event: Event) => void) | null = null;
      onerror: ((event: Event) => void) | null = null; onloadend: ((event: Event) => void) | null = null;
      private method = 'GET'; private url = ''; private headers: Record<string, string> = {};
      private responseHeaders = new Headers(); private aborted = false; private listeners = new Map<string, EventListener[]>();
      open(method: string, url: string, async = true): void { if (!async) throw new Error('Reproit causal replay does not permit synchronous XMLHttpRequest'); this.method = method.toUpperCase(); this.url = url; this.readyState = 1; this.fire('readystatechange'); }
      setRequestHeader(key: string, value: string): void { this.headers[key] = value; }
      addEventListener(kind: string, fn: EventListener): void { this.listeners.set(kind, [...(this.listeners.get(kind) || []), fn]); }
      removeEventListener(kind: string, fn: EventListener): void { this.listeners.set(kind, (this.listeners.get(kind) || []).filter((item) => item !== fn)); }
      getResponseHeader(key: string): string | null { return this.responseHeaders.get(key); }
      getAllResponseHeaders(): string { let out = ''; this.responseHeaders.forEach((v, k) => { out += `${k}: ${v}\r\n`; }); return out; }
      abort(): void { this.aborted = true; }
      private fire(kind: string): void { const event = { target: this } as unknown as Event; const handler = this[`on${kind}` as 'onload'] as ((event: Event) => void) | null; handler?.(event); for (const fn of this.listeners.get(kind) || []) fn.call(this as unknown as EventTarget, event); }
      async send(body?: Document | XMLHttpRequestBodyInit | null): Promise<void> {
        try {
          const response = await g.fetch!(this.url, { method: this.method, headers: this.headers, body: body as BodyInit | null | undefined });
          if (this.aborted) return;
          this.status = response.status; this.responseHeaders = response.headers; this.responseText = await response.text();
          this.response = this.responseType === 'json' ? JSON.parse(this.responseText) : this.responseText;
          this.readyState = 4; this.fire('readystatechange'); this.fire('load'); this.fire('loadend');
        } catch { this.readyState = 4; this.fire('readystatechange'); this.fire('error'); this.fire('loadend'); }
      }
    }
    g.XMLHttpRequest = ReproItXMLHttpRequest as unknown as typeof XMLHttpRequest;
  }
  emit('REPROIT:CAPABILITIES {"http":{"status":"captured","detail":"global fetch + XMLHttpRequest"},"http_replay":{"status":"captured","detail":"global fetch + XMLHttpRequest fail-closed adapter"}}');
  return () => { g.fetch = original; g.XMLHttpRequest = originalXhr; };
}
