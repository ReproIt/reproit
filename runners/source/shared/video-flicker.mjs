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

function emitDiagnostics(options, details) {
  if (typeof options?.onDiagnostics !== 'function') return;
  try {
    options.onDiagnostics(details);
  } catch (_) {
    // Diagnostics are validation evidence, never behavioral authority.
  }
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
    emitDiagnostics(options, { outcome: 'abstained', reason: 'decode-failed' });
    return null;
  }
  const count = Math.min(MAX_FRAMES, Math.floor(raw.length / FRAME_BYTES));
  if (count < 4) {
    emitDiagnostics(options, {
      outcome: 'abstained',
      reason: 'short-capture',
      frames: count,
    });
    return null;
  }
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

function medoidFrameIndex(frames, startIndex, endIndex) {
  if (startIndex >= endIndex) return null;
  let bestIndex = startIndex;
  let bestDistance = Number.POSITIVE_INFINITY;
  for (let candidateIndex = startIndex; candidateIndex < endIndex; candidateIndex++) {
    let distance = 0;
    for (let otherIndex = startIndex; otherIndex < endIndex; otherIndex++) {
      distance += changedFraction(frames[candidateIndex], frames[otherIndex]);
    }
    if (distance < bestDistance) {
      bestIndex = candidateIndex;
      bestDistance = distance;
    }
  }
  return bestIndex;
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
  if (!Array.isArray(frames) || frames.length < 4) {
    emitDiagnostics(options, {
      outcome: 'abstained',
      reason: 'short-capture',
      frames: Array.isArray(frames) ? frames.length : 0,
    });
    return null;
  }
  const actionFrameIndex = boundedActionFrameIndex(frames, options.actionFrameIndex);
  const startIndex = medoidFrameIndex(
    frames,
    Math.max(0, actionFrameIndex - SETTLED_WINDOW_FRAMES),
    actionFrameIndex,
  );
  const endIndex = medoidFrameIndex(
    frames,
    Math.max(0, frames.length - SETTLED_WINDOW_FRAMES),
    frames.length,
  );
  if (startIndex === null || endIndex === null) return null;
  const start = frames[startIndex];
  const end = frames[endIndex];

  const endpointDifference = changedFraction(start, end);
  const floor = Math.max(0.04, endpointDifference * 1.35);
  let peak = 0;
  let persistent = false;
  let previous = 0;
  const differences = [];
  for (const frame of frames.slice(actionFrameIndex, -1)) {
    const difference = Math.min(
      changedFraction(frame, start),
      changedFraction(frame, end),
    );
    differences.push(Math.round(difference * 1_000) / 1_000);
    peak = Math.max(peak, difference);
    if (difference > floor && previous > floor) persistent = true;
    previous = difference;
  }
  const finding = persistent
    ? { peak: Math.round(peak * 1_000) / 1_000, frames: frames.length }
    : null;
  emitDiagnostics(options, {
    outcome: finding ? 'finding' : 'clean',
    frames: frames.length,
    requestedActionFrameIndex: options.actionFrameIndex ?? null,
    actionFrameIndex,
    startIndex,
    endIndex,
    endpointDifference: Math.round(endpointDifference * 1_000) / 1_000,
    floor: Math.round(floor * 1_000) / 1_000,
    differences,
  });
  return finding;
}

// Decode a bounded action-scoped recording through ffmpeg into fixed-size RGB
// samples. Missing ffmpeg, corrupt video, and short captures all abstain.
export function classifyVideoFile(path, options = {}) {
  return decodeFrames(['-t', String(MAX_CAPTURE_SECONDS), '-i', path], options);
}
