/*!
 * Hermetic replay mode for reproit-backend-node.
 *
 * When `REPROIT_REPLAY` names a `reproit-backend-capture` payload, the same
 * client wrappers that record exchanges at capture time SERVE them instead:
 * outbound HTTP is answered from the recorded exchanges by an in-process
 * loopback stub, and wrapped pg clients return recorded results, so the
 * application code re-executes against exactly what production saw, with no
 * live dependencies.
 *
 * Determinism is a contract here, not a similarity score. Matching is
 * strict per-operation ordinals: within one operation (method+path for HTTP,
 * statement text for pg) exchanges are consumed in recorded order, so pooled
 * pg clients and LLM tool-call loops that interleave operations still match
 * exactly. Recorded `$reproit` redaction placeholders match any value at
 * their position; nothing else is tolerated. The first unmatched call is a
 * DIVERGENCE: it is reported as a structured `REPROIT:DIVERGENCE` line on
 * stderr and the call fails with status 599 (HTTP) or a thrown error (pg),
 * never a fuzzy match.
 *
 * The envelope pins the replay's determinism: `TZ` from the capture,
 * `Date.now` offset to the capture moment, `Math.random` seeded from
 * `replaySeed`. Honesty note: the seed makes REPLAY runs deterministic; it
 * does not reproduce the randomness the app drew in production.
 */
'use strict';

const fs = require('fs');

const DIVERGENCE_MARKER = 'REPROIT:DIVERGENCE ';

class ReplaySession {
  static load(path) {
    const payload = JSON.parse(fs.readFileSync(path, 'utf8'));
    if (payload.format !== 'reproit-backend-capture') {
      throw new TypeError('REPROIT_REPLAY file is not a reproit-backend-capture payload');
    }
    if (!(payload.version >= 1 && payload.version <= 2)) {
      throw new TypeError('unsupported capture version ' + payload.version);
    }
    return new ReplaySession(payload);
  }

  constructor(payload) {
    this.payload = payload;
    this.envelope = payload.envelope ?? null;
    this.exchanges = (payload.events ?? [])
      .filter((event) => event.kind === 'effect' && event.exchange)
      .map((event) => ({ exchange: event.exchange, consumed: false }));
    this.diverged = false;
  }

  // Strict per-operation ordinal match. Returns the exchange or null
  // (divergence).
  match(protocol, probe) {
    const matcher = protocol === 'http' ? httpRequestMatcher : pgRequestMatcher;
    const key = operationKey(protocol, probe);
    for (const entry of this.exchanges) {
      if (entry.consumed || entry.exchange.protocol !== protocol) continue;
      if (operationKey(protocol, entry.exchange.request ?? {}) !== key) continue;
      if (matcher(entry.exchange.request ?? {}, probe)) {
        entry.consumed = true;
        return entry.exchange;
      }
      // Strict ordinal within an operation: the next unconsumed exchange of
      // THIS operation is the only candidate; skipping it silently would be
      // a fuzzy match. Other operations' exchanges may interleave (pg
      // pooling, tool-call loops), which is why the key filters above.
      break;
    }
    this.diverge(protocol, probe);
    return null;
  }

  diverge(protocol, probe) {
    this.diverged = true;
    const key = operationKey(protocol, probe);
    const candidates = this.exchanges.filter(
      (entry) => !entry.consumed && entry.exchange.protocol === protocol,
    );
    const expected =
      candidates.find(
        (entry) => operationKey(protocol, entry.exchange.request ?? {}) === key,
      ) ?? candidates[0];
    const report = {
      protocol,
      got: probe,
      expected: expected ? expected.exchange.request : null,
      consumed: this.exchanges.filter((entry) => entry.consumed).length,
      total: this.exchanges.length,
    };
    // Prompt drift: when the recorded and live bodies both exist and differ,
    // name WHERE they differ. Chat-shaped bodies (OpenAI/Anthropic messages
    // arrays) name the first differing message index; unknown shapes fall
    // back to the byte offset of the first differing byte.
    const delta = expected ? bodyDelta((expected.exchange.request ?? {}).body, probe.body) : null;
    if (delta !== null) report.bodyDelta = delta;
    process.stderr.write(DIVERGENCE_MARKER + JSON.stringify(report) + '\n');
  }
}

// One operation's identity for ordinal matching: HTTP is method plus
// path+query, pg is the exact statement text.
function operationKey(protocol, request) {
  return protocol === 'http'
    ? String(request.method ?? '') + ' ' + urlPathAndQuery(request.url)
    : String(request.text ?? '');
}

// The messages array of an OpenAI/Anthropic-shaped chat body, else null.
function chatMessages(body) {
  if (body && typeof body === 'object' && Array.isArray(body.messages)) return body.messages;
  return null;
}

// Locate the first difference between a recorded request body and a live
// one, modulo redaction placeholders. Null when there is nothing to report
// (either body missing, or no difference the matcher would object to).
function bodyDelta(recorded, live) {
  if (recorded === undefined || live === undefined) return null;
  if (matches(recorded, live)) return null;
  const recordedMessages = chatMessages(recorded);
  const liveMessages = chatMessages(live);
  if (recordedMessages !== null && liveMessages !== null) {
    const bound = Math.min(recordedMessages.length, liveMessages.length);
    let index = null;
    for (let i = 0; i < bound; i += 1) {
      if (!matches(recordedMessages[i], liveMessages[i])) {
        index = i;
        break;
      }
    }
    // All shared indexes match: the drift is a longer/shorter conversation,
    // and the first differing message is the first unshared one. If lengths
    // also agree the drift is outside `messages`; fall through to bytes.
    if (index === null && recordedMessages.length !== liveMessages.length) index = bound;
    if (index !== null) {
      return {
        kind: 'message',
        firstDifferingMessage: index,
        recordedMessages: recordedMessages.length,
        liveMessages: liveMessages.length,
      };
    }
  }
  const recordedBytes = Buffer.from(
    typeof recorded === 'string' ? recorded : JSON.stringify(recorded) ?? '',
    'utf8',
  );
  const liveBytes = Buffer.from(
    typeof live === 'string' ? live : JSON.stringify(live) ?? '',
    'utf8',
  );
  const bound = Math.min(recordedBytes.length, liveBytes.length);
  let offset = bound;
  for (let i = 0; i < bound; i += 1) {
    if (recordedBytes[i] !== liveBytes[i]) {
      offset = i;
      break;
    }
  }
  return { kind: 'byte', offset };
}

// A recorded value matches a live one when equal, or when the recorded side
// is a `$reproit` redaction placeholder (any value stood here at capture) or
// a truncation stub (body identity, not bytes). Objects compare per key.
function matches(recorded, live) {
  if (recorded === null || recorded === undefined) return true;
  if (recorded && typeof recorded === 'object') {
    if (recorded.$reproit) return true;
    if (Array.isArray(recorded)) {
      if (!Array.isArray(live) || live.length !== recorded.length) return false;
      return recorded.every((item, index) => matches(item, live[index]));
    }
    if (live === null || typeof live !== 'object') return false;
    return Object.entries(recorded).every(([key, value]) => matches(value, live[key]));
  }
  return recorded === live;
}

function urlPathAndQuery(url) {
  try {
    const parsed = new URL(url);
    return parsed.pathname + parsed.search;
  } catch (ignored) {
    return String(url ?? '');
  }
}

// Resolve a live HTTP probe against the session, entirely in process (no
// sockets, so it also works for clients with synchronous request APIs).
// Returns `{status, headers, bodyText}` to synthesize the response from; a
// divergence and a truncated-at-capture body both serve a hard 599 so the
// application observes an attributable failure instead of a guess.
function serveHttp(session, probe) {
  const recorded = session.match('http', probe);
  if (recorded === null) {
    return diverged599('diverged');
  }
  const response = recorded.response ?? {};
  if (response.truncated === true) {
    // The capture kept identity but not bytes; serving a guessed body would
    // be a silent lie. Fail closed with the named reason.
    session.diverge('http', { ...probe, truncated: true });
    return diverged599('truncated-exchange-body');
  }
  const headers = { ...(response.headers ?? {}) };
  delete headers['content-length'];
  delete headers['transfer-encoding'];
  delete headers['content-encoding'];
  const bodyText =
    response.body === undefined
      ? ''
      : typeof response.body === 'string'
        ? response.body
        : JSON.stringify(response.body);
  const served = { status: response.status ?? 200, headers, bodyText };
  if (response.stream && Array.isArray(response.stream.chunks)) {
    if (response.stream.truncated === true) {
      // The capture kept the body but not every chunk boundary; serving a
      // guessed stream shape would be a silent lie. Fail closed, named.
      session.diverge('http', { ...probe, streamBoundariesTruncated: true });
      return diverged599('truncated-stream-boundaries');
    }
    served.chunks = splitChunks(bodyText, response.stream.chunks);
  }
  return served;
}

// Split a replayed body at the recorded chunk boundaries (byte lengths).
// Redaction can change body byte counts, so lengths are clamped and the last
// chunk absorbs any remainder: the CHUNK COUNT (the stream shape the app
// observed) is preserved exactly, the recorded content is never padded.
function splitChunks(bodyText, lengths) {
  const bytes = Buffer.from(bodyText, 'utf8');
  const chunks = [];
  let offset = 0;
  for (let i = 0; i < lengths.length; i += 1) {
    const last = i === lengths.length - 1;
    const size = Number.isInteger(lengths[i]) && lengths[i] > 0 ? lengths[i] : 0;
    const end = last ? bytes.length : Math.min(offset + size, bytes.length);
    chunks.push(bytes.subarray(offset, end));
    offset = end;
  }
  return chunks;
}

function diverged599(reason) {
  return {
    status: 599,
    headers: { 'content-type': 'application/json' },
    bodyText: JSON.stringify({ reproit: reason }),
  };
}

function tryJson(text, contentType) {
  if (typeof contentType === 'string' && contentType.includes('application/json')) {
    try {
      return JSON.parse(text);
    } catch (ignored) {
      return text;
    }
  }
  return text;
}

// Compare a live probe to a recorded http request: method, path+query of the
// original URL, and body modulo redaction placeholders. Recorded headers are
// deliberately not matched: they carry per-run noise (dates, connection
// management) that would turn every replay into a divergence.
function httpRequestMatcher(recordedRequest, probe) {
  if (recordedRequest.method !== probe.method) return false;
  if (urlPathAndQuery(recordedRequest.url) !== urlPathAndQuery(probe.url)) return false;
  return matches(recordedRequest.body, probe.body);
}

// Compare a live pg probe: exact statement text, values modulo placeholders.
function pgRequestMatcher(recordedRequest, probe) {
  if (recordedRequest.text !== probe.text) return false;
  return matches(recordedRequest.values, probe.values);
}

// Pin process determinism from the capture envelope. Runs once at install.
function pinEnvelope(envelope) {
  if (!envelope || typeof envelope !== 'object') return;
  if (typeof envelope.tz === 'string' && envelope.tz.length > 0) {
    process.env.TZ = envelope.tz;
  }
  if (Number.isFinite(envelope.observedAtMs)) {
    const offset = envelope.observedAtMs - Date.now();
    const realNow = Date.now.bind(Date);
    Date.now = () => realNow() + offset;
  }
  if (typeof envelope.replaySeed === 'string' && envelope.replaySeed.length > 0) {
    let state = BigInt('0x' + envelope.replaySeed.slice(0, 16).padEnd(16, '0')) | 1n;
    Math.random = () => {
      // xorshift64*: deterministic stream from the recorded seed.
      state ^= state << 13n;
      state ^= state >> 7n;
      state ^= state << 17n;
      state &= 0xffffffffffffffffn;
      return Number((state * 0x2545f4914f6cdd1dn & 0xffffffffffffffffn) >> 11n) / 2 ** 53;
    };
  }
}

module.exports = {
  ReplaySession,
  serveHttp,
  pinEnvelope,
  matches,
  tryJson,
  urlPathAndQuery,
  // Exported for the shared behavioral vectors; the matchers are pure and the
  // vectors pin them directly rather than through a live replay.
  httpRequestMatcher,
  pgRequestMatcher,
  operationKey,
  bodyDelta,
  DIVERGENCE_MARKER,
};
