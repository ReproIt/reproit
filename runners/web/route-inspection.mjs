// Bounded, evidence-safe discovery and verification of broken web routes.

export function isDeadRouteStatus(status) {
  return status === 404 || status === 410;
}

// Navigable HTML pages are deliberately absent from this list.
export const ASSET_EXT_SOURCE =
  '\\.(zip|pdf|dmg|exe|msi|pkg|deb|rpm|apk|tar|gz|tgz|bz2|xz|7z|rar|iso|mp' +
  '4|mp3|wav|mov|avi|mkv|webm|png|jpe?g|gif|svg|webp|avif|ico|bmp|css|js|' +
  'mjs|cjs|map|wasm|woff2?|ttf|otf|eot|xml|csv|txt|rss|atom)$';

export function isAssetPath(pathname) {
  return new RegExp(ASSET_EXT_SOURCE, 'i').test(pathname || '');
}

export function normalizePathname(pathname) {
  if (typeof pathname !== 'string' || pathname.length <= 1) return pathname;
  return pathname.replace(/\/+$/, '') || '/';
}

// Exact, server-visible route identity. Fragments are excluded by the caller.
export function requestRouteKey(pathname, search = '') {
  return normalizePathname(pathname) + (search || '');
}

const SENSITIVE_QUERY_KEY =
  /(auth|code|credential|jwt|key|nonce|password|secret|session|sig|state|ticket|token)/i;
const TRACKING_QUERY_KEY = /^(utm_.+|fbclid|gclid|dclid|msclkid|mc_[ce]id)$/i;

// Evidence-safe route identity. Exact queries stay internal for requests, while
// persisted/logged routes omit tracking parameters and redact secret values.
export function publicRouteKey(route) {
  try {
    const url = new URL(route, 'http://reproit.invalid');
    const safe = new URLSearchParams();
    for (const [key, value] of url.searchParams) {
      if (TRACKING_QUERY_KEY.test(key)) continue;
      safe.append(key, SENSITIVE_QUERY_KEY.test(key) ? '<redacted>' : value);
    }
    const search = safe.toString();
    const hash = url.hash.split('?', 1)[0];
    return normalizePathname(url.pathname) + (search ? '?' + search : '') + hash;
  } catch (_) {
    return normalizePathname(String(route || '').split(/[?#]/, 1)[0]);
  }
}

// In-page collector. It remains self-contained because Playwright serializes
// this function into the document rather than retaining module scope.
export function collectRouteLinks(assetExtSrc) {
  const out = [];
  const ASSET_EXT = new RegExp(assetExtSrc, 'i');
  const norm = (path) =>
    path.length > 1 ? path.replace(/\/+$/, '') || '/' : path;
  const relTokens = (anchor) =>
    (anchor.getAttribute('rel') || '').toLowerCase().split(/\s+/);
  for (const anchor of document.querySelectorAll('a[href]')) {
    try {
      if (anchor.hasAttribute('download')) continue;
      if (anchor.closest('pre,code,[role="code"]')) continue;
      const rel = relTokens(anchor);
      if (rel.includes('nofollow') || rel.includes('external')) continue;
      const raw = (anchor.getAttribute('href') || '').trim();
      if (/^(javascript:|mailto:|tel:|#)/i.test(raw)) continue;
      if (
        anchor.closest('form') &&
        (anchor.getAttribute('type') === 'submit' || anchor.hasAttribute('data-submit'))
      )
        continue;
      const url = new URL(anchor.href);
      if (url.origin !== location.origin || !url.pathname) continue;
      if (ASSET_EXT.test(url.pathname)) continue;
      out.push(norm(url.pathname) + url.search);
    } catch (_) {}
  }
  return out;
}

// In-page signals for distinguishing a hydrated SPA from a genuine dead page.
export function soft404View() {
  const body = document.body;
  if (!body) return { controls: 0, mountFilled: false, notFound: false };
  const mount = document.querySelector(
    '#root,#app,#__next,#__nuxt,[data-reactroot],main,[role=main]',
  );
  const mountFilled = !!(mount && mount.querySelectorAll('*').length > 12);
  const controls = document.querySelectorAll(
    'a[href],button,[role=button],input,select,textarea,[role=tab],' + '[role=menuitem]',
  ).length;
  const headings = Array.from(document.querySelectorAll('h1,h2,[role=heading]')).map((heading) =>
    (heading.textContent || '').trim().toLowerCase(),
  );
  const notFound = headings.some(
    (text) =>
      text.length < 60 &&
      /(^|\b)(404|not found|page not found|doesn'?t exist|no such page)\b/.test(text),
  );
  return { controls, mountFilled, notFound };
}

export function isSoftHandled(view) {
  return !!(view && view.mountFilled && view.controls >= 8 && !view.notFound);
}

// Inspect one bounded hop of successful same-origin pages, then probe their
// links without parsing another successful generation. Findings still require
// both a GET 404/410 and a real navigation 404/410 plus the SPA guard.
export async function inspectLinkedRoutes(
  page,
  {
    origin,
    seenLinks,
    navStatus,
    observe,
    log = () => {},
    fetchCap = 400,
    verifyCap = 20,
    inspectCap = 40,
    renderCap = 4,
  },
) {
  const probed = new Set();
  const renderedParents = new Map();
  const returnUrl = page.url();
  let fetched = 0;
  let verified = 0;
  let inspected = 0;
  let rendered = 0;
  let findings = 0;
  let unverified = 0;
  let coverageGaps = 0;

  const fetchStatuses = async (entries, extractLinks) => {
    const room = Math.max(0, fetchCap - fetched);
    const eligible = entries.filter(
      ([route]) => !probed.has(route) && navStatus[route] === undefined,
    );
    const batch = eligible.slice(0, room);
    coverageGaps += eligible.length - batch.length;
    for (const [route] of batch) probed.add(route);
    fetched += batch.length;
    if (!batch.length) return [];
    try {
      return await page.evaluate(
        async ({ appOrigin, entries: work, assetExtSrc, inspectLimit }) => {
          const out = new Array(work.length);
          const ASSET_EXT = new RegExp(assetExtSrc, 'i');
          const norm = (path) =>
            path.length > 1 ? path.replace(/\/+$/, '') || '/' : path;
          const parseSet = new Set(work.slice(0, inspectLimit).map((entry) => entry[0]));
          const readLimited = async (response, limit) => {
            if (!response.body || !response.body.getReader) {
              const text = await response.text();
              return text.length <= limit ? text : null;
            }
            const reader = response.body.getReader();
            const decoder = new TextDecoder();
            let size = 0;
            let text = '';
            for (;;) {
              const { done, value } = await reader.read();
              if (done) return text + decoder.decode();
              size += value.byteLength;
              if (size > limit) {
                await reader.cancel().catch(() => {});
                return null;
              }
              text += decoder.decode(value, { stream: true });
            }
          };
          let i = 0;
          const worker = async () => {
            while (i < work.length) {
              const workIndex = i++;
              const [route, fromSig, parentRoute] = work[workIndex];
              try {
                const response = await fetch(appOrigin + route, {
                  method: 'GET',
                  redirect: 'manual',
                });
                const result = {
                  route,
                  fromSig,
                  parentRoute,
                  status: response.status,
                  links: [],
                  inspected: false,
                  html: false,
                };
                const type = response.headers.get('content-type') || '';
                result.html = /(?:text\/html|application\/xhtml\+xml)/i.test(type);
                if (
                  parseSet.has(route) &&
                  response.status >= 200 &&
                  response.status < 300 &&
                  result.html
                ) {
                  const html = await readLimited(response, 1024 * 1024);
                  if (html !== null) {
                    result.inspected = true;
                    const doc = new DOMParser().parseFromString(html, 'text/html');
                    const declaredBase = doc.querySelector('base[href]')?.getAttribute('href');
                    const base = declaredBase
                      ? new URL(declaredBase, appOrigin + route).href
                      : appOrigin + route;
                    for (const anchor of doc.querySelectorAll('a[href]')) {
                      if (result.links.length >= 100) break;
                      if (anchor.hasAttribute('download')) continue;
                      if (anchor.closest('pre,code,[role="code"]')) continue;
                      const rel = (anchor.getAttribute('rel') || '').toLowerCase().split(/\s+/);
                      if (rel.includes('nofollow') || rel.includes('external')) continue;
                      const raw = (anchor.getAttribute('href') || '').trim();
                      if (/^(javascript:|mailto:|tel:|#)/i.test(raw)) continue;
                      if (
                        anchor.closest('form') &&
                        (anchor.getAttribute('type') === 'submit' ||
                          anchor.hasAttribute('data-submit'))
                      )
                        continue;
                      const url = new URL(raw, base);
                      if (url.origin !== appOrigin || !url.pathname) continue;
                      if (ASSET_EXT.test(url.pathname)) continue;
                      result.links.push(norm(url.pathname) + url.search);
                    }
                  }
                }
                out[workIndex] = result;
              } catch (_) {
                out[workIndex] = {
                  route,
                  fromSig,
                  parentRoute,
                  status: 0,
                  links: [],
                  inspected: false,
                  html: false,
                };
              }
            }
          };
          await Promise.all(Array.from({ length: 8 }, worker));
          return out;
        },
        {
          appOrigin: origin,
          entries: batch,
          assetExtSrc: ASSET_EXT_SOURCE,
          inspectLimit: extractLinks ? inspectCap : 0,
        },
      );
    } catch (_) {
      return batch.map(([route, fromSig, parentRoute]) => ({
        route,
        fromSig,
        parentRoute,
        status: 0,
        links: [],
        inspected: false,
        html: false,
      }));
    }
  };

  const recoverOrigin = async () => {
    try {
      if (new URL(page.url()).origin === origin) return true;
    } catch (_) {}
    await page.goBack({ timeout: 3000 }).catch(() => {});
    try {
      return new URL(page.url()).origin === origin;
    } catch (_) {
      return false;
    }
  };

  const parentSig = async (route, fallback) => {
    if (!route) return fallback;
    if (renderedParents.has(route)) return renderedParents.get(route);
    if (rendered >= renderCap) return null;
    rendered++;
    let response = null;
    try {
      response = await page.goto(origin + route, { waitUntil: 'load', timeout: 7000 });
    } catch (_) {}
    if (
      !(await recoverOrigin()) ||
      !response ||
      response.status() < 200 ||
      response.status() >= 300
    )
      return null;
    await page.waitForTimeout(200).catch(() => {});
    const snap = await observe().catch(() => null);
    const sig = snap?.sig || null;
    if (sig) renderedParents.set(route, sig);
    return sig;
  };

  const verifyDead = async (entries) => {
    for (const entry of entries) {
      if (!isDeadRouteStatus(entry.status)) continue;
      navStatus[entry.route] = entry.status;
      if (verified >= verifyCap) {
        unverified++;
        continue;
      }
      const fromSig = await parentSig(entry.parentRoute, entry.fromSig);
      if (!fromSig) {
        unverified++;
        continue;
      }
      verified++;
      let response = null;
      try {
        response = await page.goto(origin + entry.route, {
          waitUntil: 'load',
          timeout: 7000,
        });
      } catch (_) {}
      if (!(await recoverOrigin())) continue;
      const status = response ? response.status() : 0;
      navStatus[entry.route] = status;
      if (!isDeadRouteStatus(status)) continue;
      await page.waitForTimeout(500).catch(() => {});
      const view = await page
        .evaluate(soft404View)
        .catch(() => ({ controls: 0, mountFilled: false, notFound: false }));
      if (isSoftHandled(view)) {
        navStatus[entry.route] = 200;
        continue;
      }
      findings++;
      const route = publicRouteKey(entry.route);
      log(
        'EXPLORE:BROKENROUTE ' +
          JSON.stringify({ sig: fromSig, route, status, from: fromSig }),
      );
    }
  };

  const initialEntries = [...seenLinks.entries()].map(([route, fromSig]) => [
    route,
    fromSig,
    null,
  ]);
  const initial = await fetchStatuses(initialEntries, true);
  inspected += initial.filter((entry) => entry.inspected).length;
  const successful = initial.filter((entry) => entry.status >= 200 && entry.status < 300);
  coverageGaps += successful.filter((entry) => entry.html && !entry.inspected).length;
  await verifyDead(initial);

  // Successful children are never parsed, fixing recursion depth at one.
  const childEntries = [];
  const childSeen = new Set();
  for (const parent of initial) {
    for (const route of parent.links) {
      if (childSeen.has(route)) continue;
      childSeen.add(route);
      childEntries.push([route, parent.fromSig, parent.route]);
    }
  }
  const discovered = await fetchStatuses(childEntries, false);
  await verifyDead(discovered);
  if (page.url() !== returnUrl) {
    await page.goto(returnUrl, { waitUntil: 'domcontentloaded', timeout: 5000 }).catch(() => {});
  }
  return {
    fetched,
    verified,
    inspected,
    rendered,
    findings,
    unverified,
    coverageGaps,
  };
}
