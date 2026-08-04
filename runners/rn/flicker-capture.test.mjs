import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import test from 'node:test';

import { waitForRecordingStarted } from './runner.mjs';

function recordingProcess() {
  const proc = new EventEmitter();
  proc.stderr = new PassThrough();
  return proc;
}

test('video capture waits for simctl first-frame evidence', async () => {
  const proc = recordingProcess();
  const started = waitForRecordingStarted(proc, 100);

  proc.stderr.write('Record');
  proc.stderr.write('ing started\n');

  assert.equal(await started, true);
});

test('video capture abstains when simctl exits before its first frame', async () => {
  const proc = recordingProcess();
  const started = waitForRecordingStarted(proc, 100);

  proc.emit('exit', 1);

  assert.equal(await started, false);
});

test('video capture start has a bounded wait', async () => {
  const proc = recordingProcess();

  assert.equal(await waitForRecordingStarted(proc, 10), false);
});
