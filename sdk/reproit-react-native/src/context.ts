/**
 * PII-safe context dimensions sent with each batch (the "which users" answer).
 *
 * Mirrors the Flutter SDK (`sdk/reproit_flutter/lib/reproit_flutter.dart`):
 * a `ctx` map of low-cardinality, zero-PII signals that the reproit cloud
 * (`crates/cloud/src/ingest.rs`) folds into every event's context and uses to
 * compute a cohort DISCRIMINATOR ("this error hits users where locale=tr"),
 * turning "works for me but not for them" into a queryable cohort.
 *
 * Tier-1 auto dimensions (collected at init, dependency-free):
 *   - platform: `Platform.OS` ('ios' | 'android' | 'web' | ...)
 *   - osVersion: `Platform.Version` (OS build/release, stringified)
 *   - locale:   best-effort from `Intl.DateTimeFormat().resolvedOptions()`
 *   - tz:       IANA timezone from the same Intl resolved options
 *   - release:  `!__DEV__` (true in a release build)
 *
 * Locale source note (honest limitation): we read locale/timezone from the
 * JS `Intl` API rather than a native module, to stay dependency-free. On
 * Hermes, `Intl` is available when built with `intl` enabled (the RN default
 * since 0.73), when it is not, locale/tz are simply omitted (never throws).
 * The user's *device* locale via a native module (e.g. `I18nManager` /
 * `NativeModules.SettingsManager`) would be more precise but needs a native
 * dependency, which this SDK deliberately avoids.
 */

/** A single context value: low-cardinality, JSON-serializable, zero-PII. */
export type ContextValue = string | number | boolean | null;

/** The context map sent as the batch-level `ctx`. */
export type Context = Record<string, ContextValue>;

/**
 * Pure-JS SHA-256 (no crypto dependency). Returns lowercase hex.
 *
 * RN has no built-in `crypto.subtle`/`createHash` a library can rely on across
 * engines, so we hash in-process. This is byte-identical to the Flutter SDK's
 * `sha256.convert(utf8.encode(userId))`, so a given userId yields the SAME
 * `uid` on RN and Flutter and the cloud can group cross-platform.
 */
export function sha256Hex(input: string): string {
  // UTF-8 encode.
  const bytes: number[] = [];
  for (let i = 0; i < input.length; i++) {
    let c = input.charCodeAt(i);
    if (c < 0x80) {
      bytes.push(c);
    } else if (c < 0x800) {
      bytes.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
    } else if (c >= 0xd800 && c <= 0xdbff && i + 1 < input.length) {
      // surrogate pair
      const c2 = input.charCodeAt(++i);
      c = 0x10000 + ((c & 0x3ff) << 10) + (c2 & 0x3ff);
      bytes.push(
        0xf0 | (c >> 18),
        0x80 | ((c >> 12) & 0x3f),
        0x80 | ((c >> 6) & 0x3f),
        0x80 | (c & 0x3f)
      );
    } else {
      bytes.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
    }
  }

  const K = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
    0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
  ];

  const h = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
    0x1f83d9ab, 0x5be0cd19,
  ];

  // Padding (RFC 6234): append 0x80, zero-fill to 56 mod 64, then 64-bit length.
  const bitLen = bytes.length * 8;
  bytes.push(0x80);
  while (bytes.length % 64 !== 56) bytes.push(0);
  // 64-bit big-endian length. JS ints are safe up to 2^53, so the high 32 bits
  // come from a float divide; sufficient for any realistic userId length.
  const hi = Math.floor(bitLen / 0x100000000);
  const lo = bitLen >>> 0;
  bytes.push(
    (hi >>> 24) & 0xff,
    (hi >>> 16) & 0xff,
    (hi >>> 8) & 0xff,
    hi & 0xff,
    (lo >>> 24) & 0xff,
    (lo >>> 16) & 0xff,
    (lo >>> 8) & 0xff,
    lo & 0xff
  );

  const w = new Array<number>(64);
  const rotr = (x: number, n: number): number => (x >>> n) | (x << (32 - n));

  for (let off = 0; off < bytes.length; off += 64) {
    for (let i = 0; i < 16; i++) {
      const j = off + i * 4;
      w[i] =
        ((bytes[j] << 24) |
          (bytes[j + 1] << 16) |
          (bytes[j + 2] << 8) |
          bytes[j + 3]) >>>
        0;
    }
    for (let i = 16; i < 64; i++) {
      const s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >>> 3);
      const s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) >>> 0;
    }

    let [a, b, c, d, e, f, g, hh] = h;
    for (let i = 0; i < 64; i++) {
      const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (hh + S1 + ch + K[i] + w[i]) >>> 0;
      const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) >>> 0;
      hh = g;
      g = f;
      f = e;
      e = (d + t1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) >>> 0;
    }
    h[0] = (h[0] + a) >>> 0;
    h[1] = (h[1] + b) >>> 0;
    h[2] = (h[2] + c) >>> 0;
    h[3] = (h[3] + d) >>> 0;
    h[4] = (h[4] + e) >>> 0;
    h[5] = (h[5] + f) >>> 0;
    h[6] = (h[6] + g) >>> 0;
    h[7] = (h[7] + hh) >>> 0;
  }

  let out = '';
  for (const v of h) out += ('00000000' + (v >>> 0).toString(16)).slice(-8);
  return out;
}

/**
 * A 16-char hashed user id (first 16 hex chars of SHA-256), so the cloud can
 * group "these N users hit it" without storing identity. Matches Flutter's
 * `sha256.convert(utf8.encode(userId)).toString().substring(0, 16)`.
 */
export function hashUid(userId: string): string {
  return sha256Hex(userId).slice(0, 16);
}

/**
 * Collect the tier-1 auto dimensions. Each is best-effort and omitted (never
 * thrown) when its source is unavailable in the host engine.
 */
export function autoContext(): Context {
  const ctx: Context = {};

  // platform + OS version from RN's Platform module (dependency-free; RN core).
  try {
    // Required lazily so the pure context module can be unit-tested without RN.
    // eslint-disable-next-line @typescript-eslint/no-var-requires
    const { Platform } = require('react-native') as {
      Platform?: { OS?: string; Version?: string | number };
    };
    if (Platform && typeof Platform.OS === 'string') ctx.platform = Platform.OS;
    if (Platform && Platform.Version != null) {
      ctx.osVersion = String(Platform.Version);
    }
  } catch {
    /* react-native not present (e.g. pure-JS test env): skip */
  }

  // locale + timezone from the JS Intl API (no native dep). See module note.
  try {
    const intl = (
      globalThis as {
        Intl?: { DateTimeFormat?: new () => { resolvedOptions(): { locale?: string; timeZone?: string } } };
      }
    ).Intl;
    if (intl && typeof intl.DateTimeFormat === 'function') {
      const opts = new intl.DateTimeFormat().resolvedOptions();
      if (opts.locale) ctx.locale = opts.locale;
      if (opts.timeZone) ctx.tz = opts.timeZone;
    }
  } catch {
    /* Intl unavailable / not built with locale data: skip */
  }

  // release vs dev build. `__DEV__` is a RN global (true in dev, false in
  // release); default to a release assumption when it is undefined.
  const dev = (globalThis as { __DEV__?: boolean }).__DEV__;
  ctx.release = dev === undefined ? true : !dev;

  return ctx;
}
