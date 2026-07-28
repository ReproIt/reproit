#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const REPOSITORY_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const SUBJECT = resolve(
  REPOSITORY_ROOT,
  'target/debug',
  process.platform === 'win32' ? 'reproit.exe' : 'reproit',
);
const TIMEOUT_MS = 60_000;
const MAX_OUTPUT_BYTES = 1024 * 1024;

function appendBounded(chunks, chunk, state) {
  const bytes = Buffer.from(chunk);
  const remaining = Math.max(0, MAX_OUTPUT_BYTES - state.bytes);
  if (remaining > 0) chunks.push(bytes.subarray(0, remaining));
  state.bytes += Math.min(bytes.length, remaining);
  state.truncated ||= bytes.length > remaining;
}

function killProcessGroup(child, signal) {
  if (!child.pid) return;
  try {
    if (process.platform === 'win32') child.kill(signal);
    else process.kill(-child.pid, signal);
  } catch {
    // The process may have exited between the timeout and the signal.
  }
}

function executeSubject(fixtureRoot) {
  const environment = {
    HOME: fixtureRoot,
    LANG: 'C.UTF-8',
    NO_COLOR: '1',
    PATH: process.env.PATH ?? '/usr/bin:/bin',
    REPROIT_NO_UPDATE_CHECK: '1',
  };
  if (process.env.SystemRoot) environment.SystemRoot = process.env.SystemRoot;
  if (process.env.TMPDIR) environment.TMPDIR = process.env.TMPDIR;
  return new Promise((resolveExecution) => {
    const stdout = [];
    const stderr = [];
    const stdoutState = { bytes: 0, truncated: false };
    const stderrState = { bytes: 0, truncated: false };
    let child;
    let timedOut = false;
    let settled = false;
    let timeout;
    let forceKill;
    let abandon;

    const finish = (exitCode, signal, error = null) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      clearTimeout(forceKill);
      clearTimeout(abandon);
      resolveExecution({
        exitCode,
        signal,
        timedOut,
        error: error?.message ?? null,
        stdout: Buffer.concat(stdout).toString('utf8'),
        stderr: Buffer.concat(stderr).toString('utf8'),
        stdoutTruncated: stdoutState.truncated,
        stderrTruncated: stderrState.truncated,
      });
    };

    try {
      child = spawn(SUBJECT, ['--config', 'reproit.yaml', '--json', 'doctor'], {
        cwd: fixtureRoot,
        detached: process.platform !== 'win32',
        env: environment,
        shell: false,
        stdio: ['ignore', 'pipe', 'pipe'],
      });
    } catch (error) {
      finish(null, null, error);
      return;
    }
    child.stdout.on('data', (chunk) => appendBounded(stdout, chunk, stdoutState));
    child.stderr.on('data', (chunk) => appendBounded(stderr, chunk, stderrState));
    child.once('error', (error) => finish(null, null, error));
    child.once('close', (exitCode, signal) => finish(exitCode, signal));
    timeout = setTimeout(() => {
      timedOut = true;
      killProcessGroup(child, 'SIGTERM');
      forceKill = setTimeout(() => killProcessGroup(child, 'SIGKILL'), 2_000);
      abandon = setTimeout(
        () => finish(null, 'SIGKILL', new Error('subject did not exit after SIGKILL')),
        4_000,
      );
    }, TIMEOUT_MS);
  });
}

function check(document, name) {
  return Array.isArray(document?.checks)
    ? document.checks.find((candidate) => candidate?.name === name)
    : null;
}

function classify(execution, fixtureRoot) {
  if (
    execution.timedOut
    || execution.error
    || execution.stdoutTruncated
    || execution.stderrTruncated
  ) {
    return { verdict: 'infrastructure-failed', identity: null, exitCode: 19 };
  }
  let document;
  try {
    document = JSON.parse(execution.stdout);
  } catch {
    return { verdict: 'different-failure', identity: null, exitCode: 18 };
  }
  const config = check(document, 'config');
  const schema = check(document, 'schema');
  const prefix = 'backend project root ';
  const detail = typeof config?.detail === 'string' ? config.detail : '';
  const root = detail.startsWith(prefix) ? detail.slice(prefix.length) : null;
  const controlsPassed = config?.ok === true
    && schema?.ok === true
    && /1 operation\(s\)/.test(schema.detail ?? '');
  if (root !== null && root.trim() === '' && controlsPassed) {
    return {
      verdict: 'reproduced',
      identity: 'doctor:blank-backend-project-root',
      exitCode: 17,
    };
  }
  if (root !== null && resolve(root) === resolve(fixtureRoot) && controlsPassed) {
    return { verdict: 'not-reproduced', identity: null, exitCode: 0 };
  }
  return { verdict: 'different-failure', identity: null, exitCode: 18 };
}

async function main() {
  const createdRoot = await mkdtemp(resolve(tmpdir(), 'reproit-cli-backend-root-'));
  const fixtureRoot = await realpath(createdRoot);
  try {
    await writeFile(
      resolve(fixtureRoot, 'reproit.yaml'),
      [
        'backend:',
        '  enabled: true',
        '  target: http://127.0.0.1:9',
        '  schemas:',
        '    - openapi.yaml',
        '',
      ].join('\n'),
    );
    await writeFile(
      resolve(fixtureRoot, 'openapi.yaml'),
      [
        'openapi: 3.1.0',
        'info:',
        '  title: Reproit self-dogfood guard',
        '  version: 1.0.0',
        'paths:',
        '  /health:',
        '    get:',
        '      operationId: health',
        '      responses:',
        '        "200":',
        '          description: Healthy',
        '',
      ].join('\n'),
    );
    const result = classify(await executeSubject(fixtureRoot), fixtureRoot);
    process.stdout.write(`${JSON.stringify({
      provider: 'cli-backend-root',
      verdict: result.verdict,
      identity: result.identity,
    })}\n`);
    return result.exitCode;
  } finally {
    await rm(createdRoot, { recursive: true, force: true });
  }
}

main().then(
  (exitCode) => {
    process.exitCode = exitCode;
  },
  (error) => {
    process.stderr.write(`cli-backend-root guard: ${error.message}\n`);
    process.exitCode = 19;
  },
);
