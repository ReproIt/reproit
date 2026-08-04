import { execFileSync } from 'node:child_process';

const WIDTH = 160;
const HEIGHT = 120;
const CHANNELS = 3;
const FRAME_BYTES = WIDTH * HEIGHT * CHANNELS;
const MAX_FRAMES = 50;
const STARTUP_FRAMES_TO_SKIP = 5;

function changedFraction(left, right) {
  if (!left || !right || left.length !== right.length || left.length === 0) return 1;
  let changed = 0;
  const pixels = left.length / CHANNELS;
  for (let offset = 0; offset + 2 < left.length; offset += CHANNELS) {
    const delta =
      Math.abs(left[offset] - right[offset]) +
      Math.abs(left[offset + 1] - right[offset + 1]) +
      Math.abs(left[offset + 2] - right[offset + 2]);
    if (delta > 48) changed++;
  }
  return changed / pixels;
}

function decodeFrames(inputArgs) {
  let raw;
  try {
    raw = execFileSync(
      'ffmpeg',
      [
        '-hide_banner',
        '-loglevel',
        'error',
        ...inputArgs,
        '-vf',
        `fps=20,scale=${WIDTH}:${HEIGHT}:force_original_aspect_ratio=decrease,` +
          `pad=${WIDTH}:${HEIGHT}:(ow-iw)/2:(oh-ih)/2`,
        '-frames:v',
        String(MAX_FRAMES),
        '-f',
        'rawvideo',
        '-pix_fmt',
        'rgb24',
        '-',
      ],
      {
        encoding: 'buffer',
        maxBuffer: FRAME_BYTES * MAX_FRAMES,
        stdio: ['ignore', 'pipe', 'pipe'],
      },
    );
  } catch (_) {
    return null;
  }
  const count = Math.min(MAX_FRAMES, Math.floor(raw.length / FRAME_BYTES));
  if (count < 4) return null;
  const frames = [];
  for (let index = 0; index < count; index++) {
    const start = index * FRAME_BYTES;
    frames.push(raw.subarray(start, start + FRAME_BYTES));
  }
  return classifyVideoFlicker(frames);
}

// A video finding needs a visual overshoot that persists for two consecutive
// presented samples. Requiring persistence rejects an isolated encoder glitch,
// while comparison with both endpoints rejects ordinary one-way transitions.
export function classifyVideoFlicker(frames) {
  if (!Array.isArray(frames) || frames.length < 4) return null;
  const end = frames[frames.length - 1];
  // Capture begins before the action with a 500ms pre-roll. simctl can still
  // prepend black encoder-startup frames on loaded hosts, so use a bounded
  // settled frame inside that pre-roll as behavioral authority.
  const startIndex = Math.min(STARTUP_FRAMES_TO_SKIP, frames.length - 4);
  const start = changedFraction(frames[startIndex], end);
  const floor = Math.max(0.04, start > 0.04 ? start * 1.35 : 0.04);
  let peak = 0;
  let persistent = false;
  let previous = 0;
  for (const frame of frames.slice(1, -1)) {
    const difference = changedFraction(frame, end);
    peak = Math.max(peak, difference);
    if (difference > floor && previous > floor) persistent = true;
    previous = difference;
  }
  if (!persistent) return null;
  return { peak: Math.round(peak * 1000) / 1000, frames: frames.length };
}

// Decode a bounded action-scoped recording through ffmpeg into fixed-size RGB
// samples. Missing ffmpeg, corrupt video, and short captures all abstain.
export function classifyVideoFile(path) {
  return decodeFrames(['-t', '2.5', '-i', path]);
}
