type AnyMap = Record<string, any>;
const SECRET = /password|passwd|secret|token|authorization|cookie|email|phone|api[-_. ]?key|publishable[-_. ]?key|private[-_. ]?key|access[-_. ]?key|signing[-_. ]?key/i;

export function redactCausal(value: any): any {
  if (Array.isArray(value)) return value.map(redactCausal);
  if (value && typeof value === "object") {
    const out: AnyMap = {};
    for (const key of Object.keys(value).sort()) {
      const child = value[key];
      out[key] = SECRET.test(key)
        ? `<reproit:${typeof child === "string" ? `string:length=${[...child].length}` : typeof child}>`
        : redactCausal(child);
    }
    return out;
  }
  return value;
}

function canonical(raw: string): string {
  try {
    const u = new URL(raw);
    const pairs = [...u.searchParams.entries()].sort(([ak, av], [bk, bv]) => ak.localeCompare(bk) || av.localeCompare(bv));
    u.search = "";
    for (const [k, v] of pairs) u.searchParams.append(k, v);
    return u.toString();
  } catch { return raw; }
}

/** Automatic global-fetch adapter for Node-based TUIs. It uses runner-owned
 * files rather than stdout/stderr, because both streams are the rendered PTY. */
export function installCausalFetch(excludePrefix?: string | null): () => void {
  const proc: any = (globalThis as any).process;
  const fs = proc?.getBuiltinModule?.("node:fs");
  const networkFile = proc?.env?.REPROIT_NETWORK_FILE;
  const actionFile = proc?.env?.REPROIT_ACTION_FILE;
  const capsulePath = proc?.env?.REPROIT_CAPSULE;
  const capabilitiesFile = proc?.env?.REPROIT_CAPABILITIES_FILE;
  const original: any = (globalThis as any).fetch;
  if (!fs || typeof original !== "function" || (!networkFile && !capsulePath)) return () => {};
  let capsule: AnyMap | null = null;
  try { if (capsulePath) capsule = JSON.parse(fs.readFileSync(capsulePath, "utf8")); } catch {}
  const exchanges: AnyMap[] = capsule?.exchanges || [];
  const used = new Set<number>();
  let ordinal = 0;
  let previous = 0;
  const actionIndex = () => {
    try { return Number(fs.readFileSync(actionFile, "utf8")) || 0; } catch { return 0; }
  };
  const mergeCapabilities = () => {
    try {
      const current = JSON.parse(fs.readFileSync(capabilitiesFile, "utf8"));
      current.http = { status: "captured", detail: "Node global fetch" };
      current.http_replay = capsule
        ? { status: "captured", detail: "Node global fetch fail-closed replay" }
        : { status: "captured", detail: "adapter supports capsule replay" };
      fs.writeFileSync(capabilitiesFile, JSON.stringify(current), { mode: 0o600 });
    } catch {}
  };
  mergeCapabilities();
  (globalThis as any).fetch = async (input: any, init: AnyMap = {}) => {
    const url = typeof input === "string" ? input : input?.url || String(input);
    if (excludePrefix && url.startsWith(excludePrefix)) return original(input, init);
    const action = actionIndex();
    if (action !== previous) { previous = action; ordinal = 0; }
    const thisOrdinal = ordinal++;
    const method = String(init.method || input?.method || "GET").toUpperCase();
    if (capsule) {
      const idx = exchanges.findIndex((e, i) => !used.has(i) && e.required && e.actor === (proc.env.REPROIT_DEVICE || "a") &&
        e.actionIndex === action && String(e.method).toUpperCase() === method && canonical(String(e.url)) === canonical(url));
      if (idx < 0) throw new Error(`CAPSULE:MISS ${method} ${url} action=${action}`);
      used.add(idx);
      const e = exchanges[idx];
      return new Response(typeof e.responseBody === "string" ? e.responseBody : JSON.stringify(e.responseBody ?? ""), {
        status: e.status, headers: e.responseHeaders,
      });
    }
    const response = await original(input, init);
    let responseBody: any;
    try {
      responseBody = /json/i.test(response.headers.get("content-type") || "")
        ? redactCausal(await response.clone().json()) : "<reproit:body>";
    } catch { responseBody = "<reproit:invalid-json>"; }
    let requestBody: any;
    if (typeof init.body === "string") {
      try { requestBody = redactCausal(JSON.parse(init.body)); }
      catch { requestBody = `<reproit:body:length=${init.body.length}>`; }
    }
    const headers: AnyMap = {};
    new Headers(init.headers || {}).forEach((v, k) => { headers[k] = SECRET.test(k) ? "<reproit:secret>" : v; });
    const responseHeaders: AnyMap = {};
    response.headers.forEach((v: string, k: string) => { responseHeaders[k] = SECRET.test(k) ? "<reproit:secret>" : v; });
    const actor = proc.env.REPROIT_DEVICE || "a";
    const exchange = { id: `${actor}-${action}-${thisOrdinal}`, actor, actionIndex: action, ordinal: thisOrdinal,
      protocol: new URL(url).protocol.replace(":", ""), method, url: canonical(url), requestHeaders: headers,
      ...(requestBody === undefined ? {} : { requestBody }), status: response.status,
      responseHeaders, responseBody, required: true };
    try { fs.appendFileSync(networkFile, JSON.stringify(exchange) + "\n", { mode: 0o600 }); } catch {}
    return response;
  };
  return () => { (globalThis as any).fetch = original; };
}
