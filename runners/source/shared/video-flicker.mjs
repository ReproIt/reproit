import { execFileSync } from 'node:child_process';

const WIDTH = 160;
const HEIGHT = 120;
const CHANNELS = 3;
const FRAME_BYTES = WIDTH * HEIGHT * CHANNELS;
const FRAME_RATE = 20;
const MAX_CAPTURE_SECONDS = 5;
const MAX_FRAMES = FRAME_RATE * MAX_CAPTURE_SECONDS;
const SETTLED_WINDOW_FRAMES = 8;

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

function decodeFrames(inputArgs, options) {
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
        `fps=${FRAME_RATE},scale=${WIDTH}:${HEIGHT}:force_original_aspect_ratio=decrease,` +
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
  const actionFrameIndex = Number.isFinite(options?.actionAtSeconds)
    ? Math.floor(options.actionAtSeconds * FRAME_RATE)
    : undefined;
  return classifyVideoFlicker(frames, { actionFrameIndex });
}

function medoidFrame(frames) {
  if (frames.length === 0) return null;
  let best = frames[0];
  let bestDistance = Number.POSITIVE_INFINITY;
  for (const candidate of frames) {
    let distance = 0;
    for (const other of frames) distance += changedFraction(candidate, other);
    if (distance < bestDistance) {
      best = candidate;
      bestDistance = distance;
    }
  }
  return best;
}

function boundedActionFrameIndex(frames, requestedIndex) {
  const fallback = Math.min(SETTLED_WINDOW_FRAMES, frames.length - 3);
  if (!Number.isFinite(requestedIndex)) return fallback;
  return Math.max(1, Math.min(Math.floor(requestedIndex), frames.length - 3));
}

// A video finding needs a visual overshoot after the measured action boundary
// that persists for two consecutive presented samples and differs from both
// settled endpoints. The bounded medoids reject encoder startup and finalization
// frames without granting any single frame behavioral authority.
export function classifyVideoFlicker(frames, options = {}) {
  if (!Array.isArray(frames) || frames.length < 4) return null;
  const actionFrameIndex = boundedActionFrameIndex(frames, options.actionFrameIndex);
  const preActionFrames = frames.slice(
    Math.max(0, actionFrameIndex - SETTLED_WINDOW_FRAMES),
    actionFrameIndex,
  );
  const tailFrames = frames.slice(-SETTLED_WINDOW_FRAMES);
  const start = medoidFrame(preActionFrames);
  const end = medoidFrame(tailFrames);
  if (!start || !end) return null;

  const endpointDifference = changedFraction(start, end);
  const floor = Math.max(0.04, endpointDifference * 1.35);
  let peak = 0;
  let persistent = false;
  let previous = 0;
  for (const frame of frames.slice(actionFrameIndex, -1)) {
    const difference = Math.min(
      changedFraction(frame, start),
      changedFraction(frame, end),
    );
    peak = Math.max(peak, difference);
    if (difference > floor && previous > floor) persistent = true;
    previous = difference;
  }
  if (!persistent) return null;
  return { peak: Math.round(peak * 1000) / 1000, frames: frames.length };
}

// Decode a bounded action-scoped recording through ffmpeg into fixed-size RGB
// samples. Missing ffmpeg, corrupt video, and short captures all abstain.
export function classifyVideoFile(path, options = {}) {
  return decodeFrames(['-t', String(MAX_CAPTURE_SECONDS), '-i', path], options);
}
