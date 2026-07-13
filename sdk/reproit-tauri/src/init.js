const __reproitCapsuleRaw = __REPROIT_CAPSULE_LITERAL__;
const __reproitActor = __REPROIT_ACTOR_LITERAL__;
const __reproitCapsule = __reproitCapsuleRaw ? JSON.parse(__reproitCapsuleRaw) : null;
const __reproitUsed = new Set();
let __reproitPriorAction = -1;
let __reproitOrdinal = 0;
const __reproitOriginalFetch = window.fetch.bind(window);
const __reproitSecret = /password|passwd|secret|token|authorization|cookie|email|phone|api[-_. ]?key|publishable[-_. ]?key|private[-_. ]?key|access[-_. ]?key|signing[-_. ]?key/i;
const __reproitInvoke = (command, args) => window.__TAURI_INTERNALS__.invoke(`plugin:reproit|${command}`, args || {});
const __reproitRedact = (value) => {
  if (Array.isArray(value)) return value.map(__reproitRedact);
  if (value && typeof value === 'object') {
    const out = {};
    for (const key of Object.keys(value).sort()) out[key] = __reproitSecret.test(key)
      ? `<reproit:${typeof value[key] === 'string' ? `string:length=${[...value[key]].length}` : typeof value[key]}>`
      : __reproitRedact(value[key]);
    return out;
  }
  return value;
};
const __reproitHeaders = (headers) => {
  const out = {}; new Headers(headers || {}).forEach((value, key) => { out[key] = __reproitSecret.test(key) ? '<reproit:secret>' : value; }); return out;
};
const __reproitUrl = (raw) => { try { const u = new URL(raw, window.location.href); u.searchParams.sort(); return u.toString(); } catch (_) { return String(raw); } };
window.fetch = async (input, init) => {
  const url = typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
  const method = String((init && init.method) || (input && input.method) || 'GET').toUpperCase();
  const actionIndex = Number(await __reproitInvoke('action_index')) || 0;
  if (__reproitPriorAction !== actionIndex) { __reproitPriorAction = actionIndex; __reproitOrdinal = 0; }
  const ordinal = __reproitOrdinal++;
  if (__reproitCapsule) {
    const exchanges = __reproitCapsule.exchanges || [];
    const index = exchanges.findIndex((exchange, i) => !__reproitUsed.has(i) && exchange.required &&
      exchange.actor === __reproitActor && (exchange.actionIndex ?? exchange.action_index) === actionIndex &&
      String(exchange.method).toUpperCase() === method && __reproitUrl(exchange.url) === __reproitUrl(url));
    if (index < 0) throw new Error(`CAPSULE:MISS ${method} ${url} action=${actionIndex}`);
    __reproitUsed.add(index); const exchange = exchanges[index];
    console.log(`CAPSULE:HIT ${exchange.id}`);
    const responseBody = exchange.responseBody ?? exchange.response_body ?? '';
    return new Response(typeof responseBody === 'string' ? responseBody : JSON.stringify(responseBody), {
      status: exchange.status, headers: exchange.responseHeaders || exchange.response_headers || {},
    });
  }
  const response = await __reproitOriginalFetch(input, init);
  const requestHeaders = __reproitHeaders((init && init.headers) || (input && input.headers));
  let requestBody;
  if (init && typeof init.body === 'string') { try { requestBody = __reproitRedact(JSON.parse(init.body)); } catch (_) { requestBody = `<reproit:body:length=${init.body.length}>`; } }
  const responseHeaders = __reproitHeaders(response.headers);
  let responseBody;
  try { responseBody = /json/i.test(response.headers.get('content-type') || '') ? __reproitRedact(await response.clone().json()) : `<reproit:body>`; }
  catch (_) { responseBody = '<reproit:invalid-json>'; }
  const exchange = { id: `${__reproitActor}-${actionIndex}-${ordinal}`, actor: __reproitActor, actionIndex, ordinal,
    protocol: new URL(url, window.location.href).protocol.replace(':', ''), method, url: __reproitUrl(url),
    requestHeaders, ...(requestBody === undefined ? {} : { requestBody }), status: response.status,
    responseHeaders, responseBody, required: true };
  await __reproitInvoke('record_exchange', { line: JSON.stringify(exchange) });
  return response;
};

// Tauri webviews still expose XMLHttpRequest and several popular clients use it
// directly. Route it through the same document-start fetch adapter so capture,
// redaction, canonical matching, and fail-closed replay cannot be bypassed.
class __ReproItXMLHttpRequest {
  constructor() { this.readyState = 0; this.status = 0; this.responseText = ''; this.response = null; this.responseType = ''; this._headers = {}; this._listeners = {}; }
  open(method, url, async = true) {
    if (async === false) throw new Error('Reproit causal replay does not permit synchronous XMLHttpRequest');
    this._method = String(method || 'GET').toUpperCase(); this._url = String(url); this.readyState = 1; this._fire('readystatechange');
  }
  setRequestHeader(key, value) { this._headers[key] = value; }
  addEventListener(kind, fn) { (this._listeners[kind] ||= []).push(fn); }
  removeEventListener(kind, fn) { this._listeners[kind] = (this._listeners[kind] || []).filter((item) => item !== fn); }
  getResponseHeader(key) { return this._responseHeaders?.get(key) || null; }
  getAllResponseHeaders() { let out = ''; this._responseHeaders?.forEach((v, k) => { out += `${k}: ${v}\r\n`; }); return out; }
  abort() { this._aborted = true; }
  _fire(kind) { if (typeof this[`on${kind}`] === 'function') this[`on${kind}`]({ target: this }); for (const fn of this._listeners[kind] || []) fn.call(this, { target: this }); }
  async send(body = undefined) {
    try {
      const response = await window.fetch(this._url, { method: this._method, headers: this._headers, body });
      if (this._aborted) return;
      this.status = response.status; this._responseHeaders = response.headers;
      this.responseText = await response.text();
      this.response = this.responseType === 'json' ? JSON.parse(this.responseText) : this.responseText;
      this.readyState = 4; this._fire('readystatechange'); this._fire('load'); this._fire('loadend');
    } catch (error) { this.readyState = 4; this._error = error; this._fire('readystatechange'); this._fire('error'); this._fire('loadend'); }
  }
}
window.XMLHttpRequest = __ReproItXMLHttpRequest;
