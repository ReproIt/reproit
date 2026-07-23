// ZERO-CONTRAST oracle (web): a run of visible text whose RESOLVED foreground
// color exactly equals the background color painted behind it, so the text is
// invisible where it is required to be read. Exact colorimetric equality only
// (no WCAG-ratio judgment), and only for text that is actually laid out and
// non-empty. This is the browser analogue of the TUI cell oracle and emits the
// same EXPLORE:ZEROCONTRAST shape.
//
// Zero-FP guards:
//   - The foreground is the element's resolved `color`; the background is the
//     FIRST ancestor (self included) with a non-transparent background color,
//     resolved through alpha compositing over white. If nothing paints an
//     opaque background, abstain (we cannot know the effective backdrop).
//   - Fully transparent text (alpha 0) is a deliberate hide (sr-only labels,
//     icon-font spacers), never a contrast bug: abstain.
//   - Off-screen, zero-size, `visibility:hidden`, `display:none`, and
//     clipped sr-only nodes abstain (not "required to be read").
//   - Text behind another element (an overlay, an image) abstains: the
//     painted backdrop is not the CSS ancestor background. Checked via
//     elementFromPoint at the text's own center.

export function zeroContrastScan() {
  const parseColor = (value) => {
    const m = /rgba?\(([^)]+)\)/.exec(value || '');
    if (!m) return null;
    const parts = m[1].split(',').map((p) => parseFloat(p.trim()));
    if (parts.length < 3 || parts.some((n) => Number.isNaN(n))) return null;
    return { r: parts[0], g: parts[1], b: parts[2], a: parts[3] === undefined ? 1 : parts[3] };
  };
  // Composite `c` over an already-opaque `base` (both {r,g,b,a}).
  const over = (c, base) => ({
    r: Math.round(c.r * c.a + base.r * (1 - c.a)),
    g: Math.round(c.g * c.a + base.g * (1 - c.a)),
    b: Math.round(c.b * c.a + base.b * (1 - c.a)),
    a: 1,
  });
  const WHITE = { r: 255, g: 255, b: 255, a: 1 };
  const resolvedBackground = (el) => {
    let base = WHITE;
    const chain = [];
    for (let n = el; n; n = n.parentElement) chain.push(n);
    // Walk root -> element so nearer backgrounds composite on top.
    for (let i = chain.length - 1; i >= 0; i -= 1) {
      const bg = parseColor(getComputedStyle(chain[i]).backgroundColor);
      if (bg && bg.a > 0) base = over(bg, base);
    }
    return base;
  };
  const out = [];
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  let scanned = 0;
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    if (scanned > 4000 || out.length >= 10) break;
    const text = node.textContent.trim();
    if (text.length < 2 || !/[\p{L}\p{N}]/u.test(text)) continue;
    const el = node.parentElement;
    if (!el) continue;
    scanned += 1;
    const style = getComputedStyle(el);
    if (style.visibility === 'hidden' || style.display === 'none' || style.opacity === '0') {
      continue;
    }
    const fg = parseColor(style.color);
    if (!fg || fg.a === 0) continue; // transparent text is a deliberate hide.
    const rect = el.getBoundingClientRect();
    if (rect.width < 1 || rect.height < 1) continue;
    const cx = rect.left + rect.width / 2;
    const cy = rect.top + rect.height / 2;
    if (cx < 0 || cy < 0 || cx > innerWidth || cy > innerHeight) continue;
    // The element must actually own its center point; if something is painted
    // on top, the CSS-ancestor background is not the real backdrop.
    const hit = document.elementFromPoint(cx, cy);
    if (hit && hit !== el && !el.contains(hit) && !hit.contains(el)) continue;
    const fgc = over(fg, resolvedBackground(el));
    const bg = resolvedBackground(el);
    if (fgc.r === bg.r && fgc.g === bg.g && fgc.b === bg.b) {
      // A stable key: tag path + first 24 chars, so replay re-confirms.
      const key = `${el.tagName.toLowerCase()}:${text.slice(0, 24)}`;
      out.push({
        key,
        text: text.slice(0, 40),
        color: `rgb(${bg.r},${bg.g},${bg.b})`,
      });
    }
  }
  return out;
}
