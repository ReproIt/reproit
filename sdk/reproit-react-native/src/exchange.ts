/**
 * Outbound dependency exchanges, in the shape the backend SDKs record.
 *
 * A production failure is only re-executable if the capsule carries what the
 * app's dependencies actually returned. This module builds that record with
 * the SAME bounds, redaction, and field names as
 * `sdk/reproit-backend-node/instrument.js`, so one replay engine consumes
 * captures from a React Native app and a Node service without branching.
 *
 * React Native has neither `node:crypto` nor `Buffer`, so the UTF-8 sizing
 * and the SHA-256 used for truncated-body identity are implemented here in
 * plain TypeScript rather than imported.
 */

/** Inline body budget per exchange side, matching the backend SDKs. */
export const MAX_EXCHANGE_BODY_BYTES = 8 * 1024;
/** Recorded headers per exchange side, matching the backend SDKs. */
export const MAX_EXCHANGE_HEADERS = 32;

/** One recorded request or response side. */
export type ExchangeSide = {
  method?: string;
  url?: string;
  status?: number;
  headers?: Record<string, string>;
  body?: unknown;
  bodyBytes?: number;
  bodySha256?: string;
  truncated?: boolean;
};

/** The replay input: what the app sent and what the dependency returned. */
export type ProductionExchange = {
  protocol: 'http';
  request: ExchangeSide;
  response: ExchangeSide;
  /** Wall clock at capture, milliseconds since the epoch. */
  at?: number;
  /** Monotonic offset from the first captured event, nanoseconds. */
  monoNs?: number;
};

/**
 * Secret-shaped field names, byte-identical to the backend SDKs' list. A name
 * matches when its alphanumeric fold contains one of these, so `api-key`,
 * `API_KEY`, and `apiKey` all resolve the same way.
 */
const SECRET_PARTS = [
  'password',
  'passwd',
  'secret',
  'token',
  'authorization',
  'cookie',
  'email',
  'phone',
  'apikey',
  'publishablekey',
  'privatekey',
  'accesskey',
  'signingkey',
  'idempotencykey',
];

function secretField(name: string): boolean {
  const folded = name.replace(/[^A-Za-z0-9]/g, '').toLowerCase();
  return SECRET_PARTS.some((part) => folded.includes(part));
}

/**
 * The structure-preserving placeholder the backend SDKs emit. Type and length
 * survive so a replay can still match the shape; the value never leaves the
 * device.
 */
function metadata(value: unknown): { $reproit: { redacted: true; type: string; length: number | null } } {
  let kind = 'null';
  let length: number | null = null;
  if (typeof value === 'boolean') kind = 'boolean';
  else if (typeof value === 'number') kind = Number.isInteger(value) ? 'integer' : 'number';
  else if (typeof value === 'string') {
    kind = 'string';
    length = [...value].length;
  } else if (Array.isArray(value)) {
    kind = 'array';
    length = value.length;
  } else if (value !== null && typeof value === 'object') kind = 'object';
  return { $reproit: { redacted: true, type: kind, length } };
}

/**
 * Recursive structural redaction: secret-named fields become `$reproit`
 * placeholders, everything else recurses. This is the backend SDKs' `redact`,
 * not the causal marker's `<reproit:...>` string form, because the replay
 * matcher wildcards on the placeholder object.
 */
export function redactExchangeValue(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(redactExchangeValue);
  if (value !== null && typeof value === 'object') {
    const source = value as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const key of Object.keys(source)) {
      out[key] = secretField(key) ? metadata(source[key]) : redactExchangeValue(source[key]);
    }
    return out;
  }
  return value === undefined ? null : value;
}

/** UTF-8 byte length without Buffer or TextEncoder. */
export function utf8Bytes(text: string): number {
  let bytes = 0;
  for (let index = 0; index < text.length; index += 1) {
    const point = text.codePointAt(index) as number;
    if (point > 0xffff) index += 1;
    if (point < 0x80) bytes += 1;
    else if (point < 0x800) bytes += 2;
    else if (point < 0x10000) bytes += 3;
    else bytes += 4;
  }
  return bytes;
}

const K = [
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

function utf8Encode(text: string): number[] {
  const out: number[] = [];
  for (let index = 0; index < text.length; index += 1) {
    let point = text.codePointAt(index) as number;
    if (point > 0xffff) index += 1;
    if (point < 0x80) out.push(point);
    else if (point < 0x800) out.push(0xc0 | (point >> 6), 0x80 | (point & 0x3f));
    else if (point < 0x10000) {
      out.push(0xe0 | (point >> 12), 0x80 | ((point >> 6) & 0x3f), 0x80 | (point & 0x3f));
    } else {
      out.push(
        0xf0 | (point >> 18),
        0x80 | ((point >> 12) & 0x3f),
        0x80 | ((point >> 6) & 0x3f),
        0x80 | (point & 0x3f),
      );
      point = 0;
    }
  }
  return out;
}

/**
 * SHA-256 of a UTF-8 string, lowercase hex. Used only for the identity of a
 * body too large to carry inline, so a replay can fail closed with proof
 * rather than guess at content it never received.
 */
export function sha256Hex(text: string): string {
  const bytes = utf8Encode(text);
  const bitLength = bytes.length * 8;
  bytes.push(0x80);
  while (bytes.length % 64 !== 56) bytes.push(0);
  const high = Math.floor(bitLength / 0x100000000);
  const low = bitLength >>> 0;
  bytes.push(
    (high >>> 24) & 0xff,
    (high >>> 16) & 0xff,
    (high >>> 8) & 0xff,
    high & 0xff,
    (low >>> 24) & 0xff,
    (low >>> 16) & 0xff,
    (low >>> 8) & 0xff,
    low & 0xff,
  );
  const hash = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ];
  const w = new Array<number>(64);
  for (let chunk = 0; chunk < bytes.length; chunk += 64) {
    for (let i = 0; i < 16; i += 1) {
      w[i] =
        ((bytes[chunk + i * 4] << 24) |
          (bytes[chunk + i * 4 + 1] << 16) |
          (bytes[chunk + i * 4 + 2] << 8) |
          bytes[chunk + i * 4 + 3]) >>>
        0;
    }
    for (let i = 16; i < 64; i += 1) {
      const s0 =
        (((w[i - 15] >>> 7) | (w[i - 15] << 25)) ^
          ((w[i - 15] >>> 18) | (w[i - 15] << 14)) ^
          (w[i - 15] >>> 3)) >>>
        0;
      const s1 =
        (((w[i - 2] >>> 17) | (w[i - 2] << 15)) ^
          ((w[i - 2] >>> 19) | (w[i - 2] << 13)) ^
          (w[i - 2] >>> 10)) >>>
        0;
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, h] = hash;
    for (let i = 0; i < 64; i += 1) {
      const S1 = (((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7))) >>> 0;
      const ch = ((e & f) ^ (~e & g)) >>> 0;
      const temp1 = (h + S1 + ch + K[i] + w[i]) >>> 0;
      const S0 = (((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10))) >>> 0;
      const maj = ((a & b) ^ (a & c) ^ (b & c)) >>> 0;
      const temp2 = (S0 + maj) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d + temp1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (temp1 + temp2) >>> 0;
    }
    hash[0] = (hash[0] + a) >>> 0;
    hash[1] = (hash[1] + b) >>> 0;
    hash[2] = (hash[2] + c) >>> 0;
    hash[3] = (hash[3] + d) >>> 0;
    hash[4] = (hash[4] + e) >>> 0;
    hash[5] = (hash[5] + f) >>> 0;
    hash[6] = (hash[6] + g) >>> 0;
    hash[7] = (hash[7] + h) >>> 0;
  }
  return hash.map((word) => word.toString(16).padStart(8, '0')).join('');
}

/**
 * Bound one exchange body. Over-budget bodies keep provable identity (byte
 * count and digest) and drop their content, so replay refuses them by name
 * instead of serving something it never saw.
 */
export function boundedBody(body: string | null | undefined, contentType: string): ExchangeSide {
  if (body === null || body === undefined || body.length === 0) return {};
  const bytes = utf8Bytes(body);
  if (bytes > MAX_EXCHANGE_BODY_BYTES) {
    return { bodyBytes: bytes, bodySha256: sha256Hex(body), truncated: true };
  }
  if (contentType.includes('application/json')) {
    try {
      return { body: redactExchangeValue(JSON.parse(body)) };
    } catch {
      // Declared JSON that does not parse is recorded as text below.
    }
  }
  return { body };
}

/** Bound and lowercase one header set, redacting secret-named values. */
export function boundedHeaders(headers: Record<string, string>): ExchangeSide {
  const entries = Object.entries(headers).slice(0, MAX_EXCHANGE_HEADERS);
  if (entries.length === 0) return {};
  const out: Record<string, string> = {};
  for (const [name, value] of entries) {
    const key = String(name).toLowerCase();
    out[key] = secretField(key) ? '<reproit:secret>' : String(value);
  }
  return { headers: out };
}

/** Read a header set into a plain object, tolerating Headers and records. */
export function headerRecord(value: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (!value) return out;
  const iterable = value as { forEach?: (fn: (v: string, k: string) => void) => void };
  if (typeof iterable.forEach === 'function') {
    iterable.forEach((headerValue, key) => {
      out[key] = String(headerValue);
    });
    return out;
  }
  for (const [key, headerValue] of Object.entries(value as Record<string, unknown>)) {
    out[key] = String(headerValue);
  }
  return out;
}

/** Assemble one HTTP exchange in the backend SDKs' field order and shape. */
export function buildHttpExchange(
  request: { method: string; url: string; headers: Record<string, string>; body?: string | null },
  response: { status: number; headers: Record<string, string>; body?: string | null },
): ProductionExchange {
  const requestType = request.headers['content-type'] ?? request.headers['Content-Type'] ?? '';
  const responseType = response.headers['content-type'] ?? response.headers['Content-Type'] ?? '';
  return {
    protocol: 'http',
    request: {
      method: request.method,
      url: request.url,
      ...boundedHeaders(request.headers),
      ...boundedBody(request.body, String(requestType)),
    },
    response: {
      status: response.status,
      ...boundedHeaders(response.headers),
      ...boundedBody(response.body, String(responseType)),
    },
  };
}
