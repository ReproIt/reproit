// Annotate a minimized-repro clip for the PR comment.
//
// This ffmpeg has NO drawtext (no freetype), so every text overlay is rendered
// as a PNG via headless Chrome and composited with ffmpeg's `overlay`. We
// produce two artifacts from one source
// .mov/.mp4:
//   1. an annotated MP4: a caption bar (bug name | action being performed) laid
//      across the top, with the final ~0.8s tinted red + a "FAILURE" badge so
//      the moment the oracle fired is unmistakable.
//   2. a short GIF of the failure tail (palettegen/paletteuse for clean color),
//      sized for inline display in a GitHub comment.
//
// Usage:
//   node annotate.mjs <in.mov> <outDir> "<bug label>" "<action label>"
// Writes <outDir>/repro.mp4 and <outDir>/repro.gif. Prints the two paths.
import { chromium } from 'playwright';
import { mkdirSync, existsSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';

const IN = process.argv[2];
const OUTDIR = process.argv[3] || '/tmp/reproit-annotate';
const BUG = process.argv[4] || 'finding';
const ACTION = process.argv[5] || 'minimized repro';

if (!IN || !existsSync(IN)) {
  console.error(`annotate: input video not found: ${IN}`);
  process.exit(2);
}
mkdirSync(OUTDIR, { recursive: true });

function ffmpeg(args) {
  const r = spawnSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', ...args], {
    stdio: ['ignore', 'inherit', 'inherit'],
  });
  if (r.status !== 0) {
    console.error(`ffmpeg failed (${r.status}): ffmpeg ${args.join(' ')}`);
    process.exit(r.status || 1);
  }
}

function ffprobeWidth(path) {
  const r = spawnSync(
    'ffprobe',
    ['-v', 'error', '-select_streams', 'v:0', '-show_entries', 'stream=width', '-of', 'csv=p=0', path],
    { encoding: 'utf8' }
  );
  const w = parseInt((r.stdout || '').trim(), 10);
  return Number.isFinite(w) && w > 0 ? w : 1280;
}

function ffprobeDuration(path) {
  const r = spawnSync(
    'ffprobe',
    ['-v', 'error', '-show_entries', 'format=duration', '-of', 'csv=p=0', path],
    { encoding: 'utf8' }
  );
  const d = parseFloat((r.stdout || '').trim());
  return Number.isFinite(d) && d > 0 ? d : 3.0;
}

// Render the verdict badge PNG (transparent background, centered text): red
// "FAILURE REPRODUCED" when the repro reproduced, reproit-green "FIX VERIFIED"
// when it replayed clean on the fixed code. Font sized to width so the label
// always fits one line.
async function verdictBadge(width, out, verdict) {
  const pass = verdict === 'pass';
  const label = pass ? 'FIX VERIFIED' : 'FAILURE REPRODUCED';
  const bg = pass ? 'rgba(74,222,128,.96)' : 'rgba(229,83,60,.95)';
  const fg = pass ? '#06140c' : '#fff';
  const W = Math.min(width - 24, 360);
  const H = 64;
  const fs = Math.max(13, Math.floor((W - 28) / (label.length * 0.62)));
  const html = `<!doctype html><meta charset=utf8><style>
    html,body{margin:0;width:${W}px;height:${H}px;background:transparent}
    .badge{width:${W}px;height:${H}px;box-sizing:border-box;display:flex;
      align-items:center;justify-content:center;
      background:${bg};border:3px solid #fff;border-radius:12px;
      color:${fg};font:800 ${fs}px/1 ui-monospace,Menlo,monospace;
      letter-spacing:.04em;white-space:nowrap}
  </style><div class=badge>${label}</div>`;
  // deviceScaleFactor=1: the badge is overlaid at its native pixel size (it is
  // NOT ffmpeg-scaled like the caption bar), so it must match the video's pixel
  // coordinate space exactly or it overflows the frame.
  const b = await chromium.launch({ headless: true });
  const p = await (await b.newContext({ viewport: { width: W, height: H }, deviceScaleFactor: 1 })).newPage();
  await p.setContent(html);
  await p.screenshot({ path: out, omitBackground: true });
  await b.close();
  return { w: W, h: H };
}

const VERDICT = (process.argv[6] || 'fail').toLowerCase();
const W = ffprobeWidth(IN);
const DUR = ffprobeDuration(IN);
const badgePng = join(OUTDIR, 'badge.png');
const badge = await verdictBadge(W, badgePng, VERDICT);

const MP4 = join(OUTDIR, 'repro.mp4');
const GIF = join(OUTDIR, 'repro.gif');

// Annotated MP4: NO top banner (the surrounding demo carries the caption, and
// the bar read as an off-brand error strip). In the last 0.8s, tint the frame
// and show the verdict badge centered, exactly when the oracle fired: red for a
// reproduced failure, reproit-green for a verified fix. `duration` is not
// available in the enable expression, so bake the numeric tail start.
const tailStart = Math.max(0, DUR - 0.8).toFixed(3);
const tailExpr = `gte(t,${tailStart})`;
const tint = VERDICT === 'pass' ? 'green@0.14' : 'red@0.18';
ffmpeg([
  '-i', IN,
  '-i', badgePng,
  '-filter_complex',
  [
    `[0:v]drawbox=x=0:y=0:w=iw:h=ih:color=${tint}:t=fill:enable='${tailExpr}'[v1]`,
    `[v1][1:v]overlay=x=(W-w)/2:y=(H-h)/2:enable='${tailExpr}'[v]`,
  ].join(';'),
  '-map', '[v]',
  '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-movflags', '+faststart',
  MP4,
]);

// GIF of the failure tail (last 2.5s), 480px wide, 12fps, palette for clean color.
const palette = join(OUTDIR, 'palette.png');
ffmpeg(['-sseof', '-2.5', '-i', MP4, '-vf', 'fps=12,scale=480:-1:flags=lanczos,palettegen', palette]);
ffmpeg([
  '-sseof', '-2.5', '-i', MP4, '-i', palette,
  '-lavfi', 'fps=12,scale=480:-1:flags=lanczos[x];[x][1:v]paletteuse',
  GIF,
]);

console.log(`MP4 ${MP4}`);
console.log(`GIF ${GIF}`);
