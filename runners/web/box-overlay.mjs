// Draw finding boxes onto an ALREADY-CAPTURED video (post-capture annotation).
//
// This is the boxing path for every runner that CANNOT inject a live DOM overlay
// before capture -- native iOS (.mov from simctl), Android (adb screenrecord),
// macOS/Windows desktop (screen record), TUI (frames assembled to video). Those
// runners all know the finding's element RECTANGLE (in the captured video's
// pixel space -- that is how they drove the tap) and the time window it is on
// screen, so the box is drawn here with ffmpeg's `drawbox`, plus a caption
// rendered as a PNG (this ffmpeg has no drawtext/freetype) and composited.
//
// DOM runners (web/electron/tauri) keep their live in-page overlay -- it is
// crisper and needs no rect bookkeeping. This tool is the uniform fallback so
// the rule "every finding is boxed on its clip" holds on every backend.
//
// Usage:
//   node box-overlay.mjs <in-video> <out-video> <boxes.json>
// boxes.json: { "videoW": <px>, "videoH": <px>, "boxes": [
//   { "x":, "y":, "w":, "h":, "tStart":, "tEnd":, "label":, "color"? } ... ] }
// Coordinates and tStart/tEnd are in the captured video's own pixel/second space.
// color is "red" (default, a real finding) or "blue" (context). Prints the out
// path on success. Exit 2 = bad input, 1 = ffmpeg failure.
import { chromium } from 'playwright';
import { mkdirSync, existsSync, readFileSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';

const IN = process.argv[2];
const OUT = process.argv[3];
const SPEC = process.argv[4];

if (!IN || !existsSync(IN)) {
  console.error(`box-overlay: input video not found: ${IN}`);
  process.exit(2);
}
if (!OUT) {
  console.error('box-overlay: no output path given');
  process.exit(2);
}
if (!SPEC || !existsSync(SPEC)) {
  console.error(`box-overlay: boxes spec not found: ${SPEC}`);
  process.exit(2);
}

let spec;
try {
  spec = JSON.parse(readFileSync(SPEC, 'utf8'));
} catch (e) {
  console.error(`box-overlay: bad boxes spec: ${e.message}`);
  process.exit(2);
}
const boxes = Array.isArray(spec.boxes) ? spec.boxes : [];
if (!boxes.length) {
  console.error('box-overlay: no boxes to draw');
  process.exit(2);
}

function ffprobe(path, entries, stream) {
  const args = ['-v', 'error'];
  if (stream) args.push('-select_streams', stream);
  args.push('-show_entries', entries, '-of', 'csv=p=0', path);
  const r = spawnSync('ffprobe', args, { encoding: 'utf8' });
  return (r.stdout || '').trim();
}

// The captured video's real pixel size. A runner may report rects in a logical
// coordinate space (CSS px, points) that differs from the recorded pixel size
// (Retina/DPR, simulator scale); we scale every rect by videoPx/videoLogical so
// the box lands correctly regardless. Fall back to the video's own size when the
// spec omits its logical size.
const dims = ffprobe(IN, 'stream=width,height', 'v:0').split(',');
const vW = parseInt(dims[0], 10) || 1280;
const vH = parseInt(dims[1], 10) || 720;
const logW = spec.videoW && spec.videoW > 0 ? spec.videoW : vW;
const logH = spec.videoH && spec.videoH > 0 ? spec.videoH : vH;
const sx = vW / logW;
const sy = vH / logH;

const WORK = join(tmpdir(), `reproit-box-${process.pid}`);
mkdirSync(WORK, { recursive: true });
mkdirSync(dirname(OUT), { recursive: true });

// Render one caption chip PNG (label text on a colored pill), matching the live
// DOM overlay's look, so a post-capture box reads the same as an in-DOM one.
async function captionPng(label, color, out) {
  const bg = color === 'blue' ? 'rgba(47,107,255,.95)' : 'rgba(226,31,31,.95)';
  const fs = 15;
  const html = `<!doctype html><meta charset=utf8><style>
    html,body{margin:0;background:transparent}
    .chip{display:inline-block;padding:4px 9px;background:${bg};color:#fff;
      font:700 ${fs}px/1.1 ui-monospace,Menlo,monospace;border-radius:6px;
      white-space:nowrap}
  </style><div class=chip id=c>${String(label).replace(/[<>&]/g, '')}</div>`;
  const b = await chromium.launch({ headless: true });
  const p = await (await b.newContext({ deviceScaleFactor: 1 })).newPage();
  await p.setContent(html);
  const el = await p.$('#c');
  const box = await el.boundingBox();
  await el.screenshot({ path: out, omitBackground: true });
  await b.close();
  return { w: Math.ceil(box.width), h: Math.ceil(box.height) };
}

// Build the filter graph: for each box, a drawbox (thick stroke, timed) plus a
// caption chip overlaid just above the box (or just below when it would clip the
// top edge). Chips are separate inputs composited in order.
const inputs = ['-i', IN];
const filters = [];
let vlabel = '0:v';
let inIdx = 1;

for (let i = 0; i < boxes.length; i++) {
  const b = boxes[i];
  const x = Math.round((b.x || 0) * sx);
  const y = Math.round((b.y || 0) * sy);
  const w = Math.max(2, Math.round((b.w || 0) * sx));
  const h = Math.max(2, Math.round((b.h || 0) * sy));
  const color = b.color === 'blue' ? 'blue' : 'red';
  const stroke = color === 'blue' ? '#2f6bff' : '#e21f1f';
  const t0 = Number.isFinite(b.tStart) ? b.tStart : 0;
  const t1 = Number.isFinite(b.tEnd) ? b.tEnd : 1e9;
  const en = `between(t,${t0.toFixed(3)},${t1.toFixed(3)})`;
  const out = `b${i}`;
  filters.push(
    `[${vlabel}]drawbox=x=${x}:y=${y}:w=${w}:h=${h}:color=${stroke}@1:t=3:enable='${en}'[${out}]`,
  );
  vlabel = out;

  if (b.label) {
    const chipPng = join(WORK, `chip${i}.png`);
    // eslint-disable-next-line no-await-in-loop
    const chip = await captionPng(b.label, color, chipPng);
    inputs.push('-i', chipPng);
    const cx = Math.max(0, Math.min(vW - chip.w, x));
    // Prefer above the box; if that clips the top, put it just below.
    const cy = y - chip.h - 4 >= 0 ? y - chip.h - 4 : y + h + 4;
    const out2 = `c${i}`;
    filters.push(`[${vlabel}][${inIdx}:v]overlay=x=${cx}:y=${cy}:enable='${en}'[${out2}]`);
    vlabel = out2;
    inIdx++;
  }
}

const args = [
  '-hide_banner',
  '-loglevel',
  'error',
  '-y',
  ...inputs,
  '-filter_complex',
  filters.join(';'),
  '-map',
  `[${vlabel}]`,
  '-c:v',
  'libx264',
  '-pix_fmt',
  'yuv420p',
  '-movflags',
  '+faststart',
  OUT,
];
const r = spawnSync('ffmpeg', args, { stdio: ['ignore', 'inherit', 'inherit'] });
try {
  rmSync(WORK, { recursive: true, force: true });
} catch (_) {}
if (r.status !== 0) {
  console.error(`box-overlay: ffmpeg failed (${r.status})`);
  process.exit(r.status || 1);
}
console.log(OUT);
