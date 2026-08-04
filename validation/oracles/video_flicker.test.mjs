import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import {
  classifyVideoFile,
  classifyVideoFlicker,
} from '../../runners/shared/video-flicker.mjs';

function makeVideo(path, segments) {
  const args = ['-hide_banner', '-loglevel', 'error', '-y'];
  for (const [color, duration] of segments) {
    args.push('-f', 'lavfi', '-i', `color=c=${color}:s=160x120:d=${duration}:r=20`);
  }
  const inputs = segments.map((_, index) => `[${index}:v]`).join('');
  args.push(
    '-filter_complex',
    `${inputs}concat=n=${segments.length}:v=1:a=0,format=yuv420p[v]`,
    '-map',
    '[v]',
    path,
  );
  execFileSync('ffmpeg', args);
}

function withVideo(segments, callback) {
  const dir = mkdtempSync(join(tmpdir(), 'reproit-video-flicker-test-'));
  try {
    const path = join(dir, 'fixture.mp4');
    makeVideo(path, segments);
    callback(path);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('encoded persistent flash is detected', () => {
  withVideo(
    [
      ['black', 0.3],
      ['white', 0.2],
      ['black', 0.5],
    ],
    (path) => assert.ok(classifyVideoFile(path)),
  );
});

test('encoder startup frames do not become the behavioral baseline', () => {
  const solid = (value) => Buffer.alloc(12, value);
  const frames = [
    solid(0),
    solid(0),
    ...Array.from({ length: 6 }, () => solid(96)),
    ...Array.from({ length: 4 }, () => solid(255)),
    ...Array.from({ length: 6 }, () => solid(96)),
  ];

  assert.ok(classifyVideoFlicker(frames));
});

test('measured action boundary excludes a long encoder startup', () => {
  const solid = (value) => Buffer.alloc(12, value);
  const frames = [
    ...Array.from({ length: 8 }, () => solid(0)),
    ...Array.from({ length: 8 }, () => solid(96)),
    ...Array.from({ length: 4 }, () => solid(255)),
    ...Array.from({ length: 8 }, () => solid(96)),
  ];

  assert.ok(classifyVideoFlicker(frames, { actionFrameIndex: 16 }));
});

test('measured action boundary keeps a one-way transition silent', () => {
  const solid = (value) => Buffer.alloc(12, value);
  const frames = [
    ...Array.from({ length: 8 }, () => solid(0)),
    ...Array.from({ length: 8 }, () => solid(96)),
    ...Array.from({ length: 12 }, () => solid(255)),
  ];

  assert.equal(classifyVideoFlicker(frames, { actionFrameIndex: 16 }), null);
});

test('fixed presentation is silent', () => {
  withVideo([['black', 1]], (path) => assert.equal(classifyVideoFile(path), null));
});

test('ordinary one-way transition is silent', () => {
  withVideo(
    [
      ['black', 0.3],
      ['gray', 0.3],
      ['white', 0.4],
    ],
    (path) => assert.equal(classifyVideoFile(path), null),
  );
});

test('unreadable or incomplete video abstains', () => {
  assert.equal(classifyVideoFile('/definitely/missing/reproit-video.mp4'), null);
});
