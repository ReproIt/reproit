import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import test from 'node:test';

import {
  acquireRecordingProcess,
  waitForRecordingStarted,
} from './runner.mjs';

function recordingProcess() {
  const proc = new EventEmitter();
  proc.stderr = new PassThrough();
  proc.exitCode = null;
  proc.kill = () => {
    proc.exitCode = 0;
    proc.emit('exit', 0, null);
  };
  return proc;
}

test('video capture waits for simctl first-frame evidence', async () => {
  const proc = recordingProcess();
  const started = waitForRecordingStarted(proc, 100);

  proc.stderr.write('Record');
  proc.stderr.write('ing started\n');

  assert.deepEqual(await started, { status: 'started' });
});

test('video capture abstains when simctl exits before its first frame', async () => {
  const proc = recordingProcess();
  const started = waitForRecordingStarted(proc, 100);

  proc.emit('exit', 1, null);

  assert.deepEqual(await started, {
    status: 'unavailable',
    reason: 'recorder-exited',
    exitCode: 1,
  });
});

test('video capture start has a bounded wait', async () => {
  const proc = recordingProcess();

  assert.deepEqual(await waitForRecordingStarted(proc, 10), {
    status: 'unavailable',
    reason: 'recorder-start-timeout',
    timeoutMs: 10,
  });
});

test('video capture preserves bounded recorder failure evidence', async () => {
  const proc = recordingProcess();
  const started = waitForRecordingStarted(proc, 100);

  proc.stderr.write('x'.repeat(5000));
  proc.emit('exit', null, 'SIGABRT');

  const result = await started;
  assert.equal(result.reason, 'recorder-exited');
  assert.equal(result.signal, 'SIGABRT');
  assert.equal(result.stderr.length, 4096);
});

test('video capture retries with a fresh process before abstaining', async () => {
  const processes = [];
  const acquisition = acquireRecordingProcess(
    () => {
      const proc = recordingProcess();
      processes.push(proc);
      if (processes.length === 1) {
        queueMicrotask(() => proc.emit('exit', 1, null));
      } else {
        queueMicrotask(() => proc.stderr.write('Recording started\n'));
      }
      return proc;
    },
    { maxAttempts: 2, timeoutMs: 100, stopTimeoutMs: 10 },
  );

  const result = await acquisition;
  assert.equal(result.status, 'started');
  assert.equal(result.attempt, 2);
  assert.equal(result.attempts[0].reason, 'recorder-exited');
  assert.equal(processes.length, 2);
});

test('video capture reports every bounded attempt when unavailable', async () => {
  const result = await acquireRecordingProcess(
    () => {
      const proc = recordingProcess();
      queueMicrotask(() => proc.emit('exit', 2, null));
      return proc;
    },
    { maxAttempts: 2, timeoutMs: 100, stopTimeoutMs: 10 },
  );

  assert.equal(result.status, 'unavailable');
  assert.equal(result.reason, 'capture-unavailable');
  assert.deepEqual(
    result.attempts.map(({ attempt, reason, exitCode }) => ({
      attempt,
      reason,
      exitCode,
    })),
    [
      { attempt: 1, reason: 'recorder-exited', exitCode: 2 },
      { attempt: 2, reason: 'recorder-exited', exitCode: 2 },
    ],
  );
});
