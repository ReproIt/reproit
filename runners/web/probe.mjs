// Universal framebuffer-probe FLOOR for the operability graph (PIECE 2,
// docs/operability-graph.md "universal floor"). DETERMINISTIC, NO ML / no vision
// understanding: it is pixel-DIFF only, the exact machinery as reproit's flicker
// oracle. The idea: synthesize a pointer click at a bounded GRID of points; diff
// the framebuffer before vs after each click. A point whose click CHANGED pixels
// but is covered by NO accessibility node is an operable region with no
// accessible control: a WCAG 4.1.2 gap that the a11y-tree walk structurally
// cannot see (there is no node there to inspect).
//
// This is the floor that works on ANY rendered surface (canvas/WebGL games,
// custom-drawn widgets, an <img> map) where there is no DOM/a11y node to probe.
// It is OPT-IN (REPROIT_PROBE=1) and coarse, because it is SIDE-EFFECTING: it
// actually clicks the page. It is kept bounded (a small grid, capped points) and
// only reported as operable-but-a11y-absent regions.
//
// This module is PURE (no Playwright import): the grid math, the framebuffer
// diff, and the region classification are host-pure so they unit-test in Node
// without a browser. runner.mjs supplies the screenshots + the click + the a11y
// hit-test and feeds them here.

// Default probe geometry. Small + bounded on purpose (it is side-effecting and
// coarse): a COLS x ROWS grid inset from the edges so we don't click chrome.
export const DEFAULT_GRID = { cols: 6, rows: 6, inset: 0.04 };
// A click must change at least this fraction of the diffed pixels to count as
// "operable" (so anti-aliasing / a 1px caret blink is not a false positive). The
// flicker oracle uses the same per-pixel-then-fraction shape.
export const DEFAULT_CHANGE_FRACTION = 0.002; // 0.2% of probed pixels
// Per-channel absolute difference above which a pixel counts as changed. Matches
// a conservative pixelmatch-style threshold so JPEG-ish noise is ignored.
export const PIXEL_DELTA = 24;

// The deterministic grid of click points (viewport coords), left-to-right then
// top-to-bottom so the probe order is stable across runs. Inset keeps clicks off
// the very edges (scrollbars, browser chrome). Pure.
export function gridPoints(width, height, grid = DEFAULT_GRID) {
  const { cols, rows, inset } = { ...DEFAULT_GRID, ...grid };
  const pts = [];
  if (width <= 0 || height <= 0 || cols < 1 || rows < 1) return pts;
  const x0 = Math.round(width * inset);
  const y0 = Math.round(height * inset);
  const x1 = Math.round(width * (1 - inset));
  const y1 = Math.round(height * (1 - inset));
  const dx = cols > 1 ? (x1 - x0) / (cols - 1) : 0;
  const dy = rows > 1 ? (y1 - y0) / (rows - 1) : 0;
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      pts.push({ x: Math.round(x0 + c * dx), y: Math.round(y0 + r * dy) });
    }
  }
  return pts;
}

// Count the fraction of pixels that changed between two equal-size RGBA buffers
// (Uint8ClampedArray/Buffer, length = w*h*4). A pixel is "changed" if any RGB
// channel differs by more than PIXEL_DELTA (alpha ignored: a fade can flip alpha
// without a visible change). Returns a number in [0,1]. Pure + deterministic;
// this is the same per-pixel threshold then fraction the flicker oracle uses.
export function changedFraction(before, after, pixelDelta = PIXEL_DELTA) {
  const n = Math.min(before.length, after.length);
  if (n === 0) return 0;
  let changed = 0;
  let pixels = 0;
  for (let i = 0; i + 3 < n; i += 4) {
    pixels++;
    const dr = Math.abs(before[i] - after[i]);
    const dg = Math.abs(before[i + 1] - after[i + 1]);
    const db = Math.abs(before[i + 2] - after[i + 2]);
    if (dr > pixelDelta || dg > pixelDelta || db > pixelDelta) changed++;
  }
  return pixels === 0 ? 0 : changed / pixels;
}

// Classify ONE probed point given (a) the change fraction its click produced and
// (b) whether an accessibility node covered that point. Returns one of:
//   'gap'        operable (pixels changed) AND no a11y node there -> the finding
//   'covered'    operable AND an a11y node there -> healthy (already in graph 2)
//   'inert'      no pixel change -> not operable, nothing to report
// Pure; the engine only acts on 'gap'. `changeFraction` is the threshold.
export function classifyPoint(changed, a11yCovered, changeFraction = DEFAULT_CHANGE_FRACTION) {
  if (changed < changeFraction) return 'inert';
  return a11yCovered ? 'covered' : 'gap';
}

// Reduce a list of probed points into the EXPLORE:GROUNDTRUTH `elements` the
// engine consumes. Each input point: { x, y, changed (fraction), a11yCovered }.
// Only 'gap' points become elements. They are emitted as operable:true with
// rolePresent:false (the floor's signal: pixels react but AT sees no control),
// addressed by a deterministic spatial selector so the same surface yields the
// same ids. Pure + deterministic (sorted by selector).
export function probeRegionsToGroundtruth(points) {
  const els = [];
  for (const p of points || []) {
    if (classifyPoint(p.changed, p.a11yCovered) !== 'gap') continue;
    els.push({
      // Spatial selector: the only stable address for a region with no DOM node.
      // The map layer surfaces it as "operable region at (x,y), no control".
      id: 'probe:@' + p.x + ',' + p.y,
      operable: true,
      gestureKind: 'probe',
      a11y: {
        // The whole point of the floor: pixels react but no a11y node is there.
        rolePresent: false,
        namePresent: false,
      },
    });
  }
  els.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return els;
}
