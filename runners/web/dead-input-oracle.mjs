// DEAD-INPUT oracle: a runner-injected input provably vanished where an effect
// is structurally required. The runner controls the input (trusted CDP
// dispatch) and observes the whole event pipeline, so "known input, zero
// effect, nobody consumed it" is an equality check, not a judgment.
//
// Subsets (each with its own zero-FP guards):
//
// WHEEL over a scrollable region with room in the wheel direction:
//   - a handler that calls preventDefault claims the wheel (custom/virtual
//     scrollers) -> abstain;
//   - a dialog/modal interceptor owning the probe point is intentional UX ->
//     abstain;
//   - an INVISIBLE non-dialog interceptor (zero-alpha background, no painted
//     content) blocking wheel over visible scrollable content is never
//     intentional -> finding (the input-blocking-overlay bug family);
//   - a direct hit that fires no scroll event anywhere, moves nothing, and is
//     not prevented is a broken scroll pipeline -> finding.
//
// KEYSTROKE into a focused, empty, enabled, non-readonly text input:
//   - any preventDefault abstains (filters, masks, custom editors own the
//     key; their internal bugs need an authored contract to judge);
//   - fires only when the trusted key arrived, nobody prevented it, and the
//     browser still produced no beforeinput/input and no value/selection
//     delta -- a broken input pipeline. Browsers guarantee insertion
//     otherwise, so this is a strict safety net.
//
// Non-destructive: scroll offsets are restored, an inserted probe char is
// backspaced, and the pointer is parked back at the origin.

export const DEAD_INPUT_MAX_SCROLLABLES = 3;
export const DEAD_INPUT_MAX_EDITABLES = 1;
const SETTLE_MS = 150;
const WHEEL_DELTA = 120;

// In-page: find wheel-probe candidates. Self-contained (browser globals only).
export function deadInputScrollCandidates() {
  const out = [];
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  let scanned = 0;
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT);
  for (let el = walker.currentNode; el; el = walker.nextNode()) {
    if (++scanned > 4000 || out.length >= 12) break;
    if (el.tagName === 'IFRAME') continue;
    const cs = getComputedStyle(el);
    if (!/^(auto|scroll)$/.test(cs.overflowY)) continue;
    const room = el.scrollHeight - el.clientHeight - el.scrollTop;
    if (el.scrollHeight - el.clientHeight < 48 || room < 24) continue;
    const r = el.getBoundingClientRect();
    if (r.width < 80 || r.height < 60) continue;
    const cx = Math.max(0, r.left) + (Math.min(vw, r.right) - Math.max(0, r.left)) / 2;
    const cy = Math.max(0, r.top) + (Math.min(vh, r.bottom) - Math.max(0, r.top)) / 2;
    if (cx <= 1 || cy <= 1 || cx >= vw - 1 || cy >= vh - 1) continue;
    if (el.querySelector('iframe')) continue;
    const key = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
    out.push({
      idx: out.length,
      x: cx,
      y: cy,
      key: key ? 'testid:' + key : 'scrollable#' + out.length,
      context: el.tagName.toLowerCase() + (el.id ? '#' + el.id : ''),
    });
    el.setAttribute('data-reproit-deadinput', String(out.length - 1));
  }
  return out;
}

// In-page: classify what owns the probe point BEFORE dispatching the wheel.
// Returns 'target' (the scrollable or its subtree), 'dialog' (intentional
// modal UX -> abstain), 'blocker' (invisible non-dialog interceptor), or
// 'visible-interceptor' (someone visible owns the point -> abstain; occlusion
// is a different oracle's business).
export function deadInputPointOwner(arg) {
  const el = document.querySelector('[data-reproit-deadinput="' + arg.idx + '"]');
  if (!el) return { owner: 'gone' };
  const t = document.elementFromPoint(arg.x, arg.y);
  if (!t) return { owner: 'gone' };
  if (t === el || el.contains(t) || t.contains(el)) return { owner: 'target' };
  for (let n = t; n; n = n.parentElement) {
    const role = n.getAttribute && (n.getAttribute('role') || '');
    if (/^(dialog|alertdialog)$/.test(role)) return { owner: 'dialog' };
    if (n.getAttribute && n.getAttribute('aria-modal') === 'true') return { owner: 'dialog' };
    if (n.tagName === 'DIALOG' && n.open) return { owner: 'dialog' };
  }
  const cs = getComputedStyle(t);
  const alpha = (() => {
    const m = /rgba?\(\s*\d+\s*,\s*\d+\s*,\s*\d+\s*(?:,\s*([\d.]+)\s*)?\)/.exec(
      cs.backgroundColor,
    );
    return m ? (m[1] === undefined ? 1 : parseFloat(m[1])) : 1;
  })();
  const paints = alpha > 0 || cs.backgroundImage !== 'none'
    || (t.textContent || '').trim() !== ''
    || t.querySelector('img,svg,canvas,video') !== null;
  const desc = t.tagName.toLowerCase() + (t.id ? '#' + t.id : '')
    + (t.className && typeof t.className === 'string'
      ? '.' + t.className.trim().split(/\s+/).slice(0, 2).join('.')
      : '');
  return { owner: paints ? 'visible-interceptor' : 'blocker', desc };
}

// In-page: arm the pipeline recorder. Captures every scroll anywhere (capture
// phase on the document sees non-bubbling scroll events) and keeps a ref to
// the wheel event so its FINAL defaultPrevented is readable after dispatch.
export function deadInputArm(arg) {
  const el = document.querySelector('[data-reproit-deadinput="' + arg.idx + '"]');
  const rec = {
    scrolls: 0,
    wheel: null,
    key: null,
    inputs: 0,
    startTop: el ? el.scrollTop : 0,
    startWinY: window.scrollY,
  };
  rec.onScroll = () => { rec.scrolls += 1; };
  rec.onWheel = (e) => { rec.wheel = e; };
  rec.onKey = (e) => { rec.key = e; };
  rec.onInput = () => { rec.inputs += 1; };
  document.addEventListener('scroll', rec.onScroll, { capture: true, passive: true });
  window.addEventListener('wheel', rec.onWheel, { capture: true, passive: true });
  window.addEventListener('keydown', rec.onKey, { capture: true, passive: true });
  window.addEventListener('beforeinput', rec.onInput, { capture: true, passive: true });
  window.addEventListener('input', rec.onInput, { capture: true, passive: true });
  window.__reproitDeadInput = rec;
  return true;
}

// In-page: read the recorder AFTER dispatch settled and disarm. The event
// refs' defaultPrevented properties are final once dispatch completed.
export function deadInputRead(arg) {
  const rec = window.__reproitDeadInput;
  if (!rec) return null;
  const el = document.querySelector('[data-reproit-deadinput="' + arg.idx + '"]');
  document.removeEventListener('scroll', rec.onScroll, { capture: true });
  window.removeEventListener('wheel', rec.onWheel, { capture: true });
  window.removeEventListener('keydown', rec.onKey, { capture: true });
  window.removeEventListener('beforeinput', rec.onInput, { capture: true });
  window.removeEventListener('input', rec.onInput, { capture: true });
  delete window.__reproitDeadInput;
  return {
    scrolls: rec.scrolls,
    wheelSeen: rec.wheel !== null,
    wheelPrevented: rec.wheel ? rec.wheel.defaultPrevented === true : false,
    keySeen: rec.key !== null,
    keyPrevented: rec.key ? rec.key.defaultPrevented === true : false,
    inputs: rec.inputs,
    topDelta: el ? el.scrollTop - rec.startTop : 0,
    winDelta: window.scrollY - rec.startWinY,
  };
}

// Pure verdict for one wheel probe; unit-testable without a browser. A
// finding requires: the trusted wheel ARRIVED, nobody claimed it, and nothing
// anywhere scrolled. Anything else abstains.
export function classifyWheelProbe(owner, read) {
  if (!read || !read.wheelSeen) return null;
  if (read.wheelPrevented) return null;
  if (read.scrolls > 0 || read.topDelta !== 0 || read.winDelta !== 0) return null;
  if (owner.owner === 'target') return 'dead-scroll';
  if (owner.owner === 'blocker') return 'blocked-by-invisible-overlay';
  return null; // dialog / visible interceptor / gone -> abstain
}

// Pure verdict for one keystroke probe. A finding requires: the trusted key
// ARRIVED at the pipeline, nobody prevented it, and the browser still
// produced no input event and no value delta.
export function classifyKeyProbe(read, valueBefore, valueAfter) {
  if (!read || !read.keySeen) return null;
  if (read.keyPrevented) return null;
  if (read.inputs > 0 || valueAfter !== valueBefore) return null;
  return 'dead-keystroke';
}

// Host-side probe driver. Discovers candidates, dispatches trusted input via
// the page's keyboard/mouse, classifies with the pure verdicts, restores all
// state. Bounded: at most DEAD_INPUT_MAX_SCROLLABLES wheel probes and
// DEAD_INPUT_MAX_EDITABLES keystroke probes per state.
export async function deadInputProbe(page) {
  const items = [];
  const candidates = await page.evaluate(deadInputScrollCandidates);
  for (const cand of candidates.slice(0, DEAD_INPUT_MAX_SCROLLABLES)) {
    const owner = await page.evaluate(deadInputPointOwner, cand);
    if (owner.owner === 'gone' || owner.owner === 'visible-interceptor'
      || owner.owner === 'dialog') continue;
    await page.evaluate(deadInputArm, cand);
    await page.mouse.move(cand.x, cand.y);
    await page.mouse.wheel(0, WHEEL_DELTA);
    await page.waitForTimeout(SETTLE_MS);
    const read = await page.evaluate(deadInputRead, cand);
    const verdict = classifyWheelProbe(owner, read);
    if (verdict) {
      // Confirm on a second settled sample: the same probe must fail twice.
      await page.evaluate(deadInputArm, cand);
      await page.mouse.wheel(0, WHEEL_DELTA);
      await page.waitForTimeout(SETTLE_MS);
      const again = await page.evaluate(deadInputRead, cand);
      if (classifyWheelProbe(owner, again) === verdict) {
        items.push({
          key: cand.key,
          input: 'wheel:down',
          context: verdict === 'blocked-by-invisible-overlay'
            ? cand.context + ' blocked by ' + (owner.desc || 'overlay')
            : cand.context,
        });
      }
    } else if (read && (read.topDelta !== 0 || read.winDelta !== 0)) {
      // The wheel scrolled something: restore the offsets we disturbed.
      await page.evaluate((arg) => {
        const el = document.querySelector('[data-reproit-deadinput="' + arg.idx + '"]');
        if (el) el.scrollTop -= arg.topDelta;
        if (arg.winDelta) window.scrollBy(0, -arg.winDelta);
      }, { idx: cand.idx, topDelta: read.topDelta, winDelta: read.winDelta });
    }
  }
  await page.evaluate(() => {
    for (const el of document.querySelectorAll('[data-reproit-deadinput]')) {
      el.removeAttribute('data-reproit-deadinput');
    }
  });

  const editable = await page.evaluate(() => {
    const ok = (el) => {
      const t = (el.getAttribute('type') || 'text').toLowerCase();
      return el.tagName === 'TEXTAREA'
        || (el.tagName === 'INPUT' && /^(text|search|email|url|tel)$/.test(t));
    };
    for (const el of document.querySelectorAll('input, textarea')) {
      if (!ok(el) || el.disabled || el.readOnly || el.value !== '') continue;
      const r = el.getBoundingClientRect();
      if (r.width < 40 || r.height < 12 || r.bottom < 0 || r.top > window.innerHeight) continue;
      el.setAttribute('data-reproit-deadinput', 'key');
      const key = el.getAttribute('data-testid') || el.getAttribute('data-test-id');
      return {
        key: key ? 'testid:' + key : (el.name ? 'name:' + el.name : 'editable#0'),
        context: el.tagName.toLowerCase() + (el.id ? '#' + el.id : ''),
      };
    }
    return null;
  });
  if (editable) {
    const target = page.locator('[data-reproit-deadinput="key"]');
    await target.focus();
    await page.evaluate(deadInputArm, { idx: 'key' });
    await page.keyboard.press('a');
    await page.waitForTimeout(SETTLE_MS);
    const valueAfter = await page.evaluate(() => {
      const el = document.querySelector('[data-reproit-deadinput="key"]');
      return el ? el.value : null;
    });
    const read = await page.evaluate(deadInputRead, { idx: 'key' });
    const verdict = classifyKeyProbe(read, '', valueAfter === null ? '' : valueAfter);
    if (verdict) {
      items.push({ key: editable.key, input: 'key:a', context: editable.context });
    } else if (valueAfter) {
      await page.keyboard.press('Backspace');
    }
    await page.evaluate(() => {
      const el = document.querySelector('[data-reproit-deadinput="key"]');
      if (el) el.removeAttribute('data-reproit-deadinput');
      if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
    });
  }
  await page.mouse.move(0, 0);
  return items.slice(0, 6);
}
