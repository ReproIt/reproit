#!/usr/bin/env node

/**
 * Trusted verifier for CLI defect classes that a request-shaped guard cannot
 * express.
 *
 * The existing self-dogfood guards assert CI wiring. These assert behaviour of
 * the reproit binary and of the repository's own callers, which is where this
 * project's real defects have actually lived.
 *
 * Contract, identical to ci-enforcement.mjs so the guard machinery is
 * unchanged: exit 17 when the defect REPRODUCES, exit 0 when the subject is
 * clean, exit 70 when the verifier itself could not decide. A verifier that
 * cannot decide must never report clean.
 *
 * Both directions are provable without mutating the repository:
 *   REPROIT_DOGFOOD_SUBJECT_ROOT  the tree to inspect (default: cwd)
 *   REPROIT_DOGFOOD_BINARY        the reproit binary to exercise
 */

import { mkdtemp, mkdir, writeFile, rm, readFile, readdir } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);

const DEFECT_EXIT_CODE = 17;
const INFRASTRUCTURE_EXIT_CODE = 70;
const MAX_FILE_BYTES = 512 * 1024;
const MAX_SCANNED_FILES = 256;
const COMMAND_TIMEOUT_MS = 60_000;

function subjectRoot() {
  return resolve(process.env.REPROIT_DOGFOOD_SUBJECT_ROOT || process.cwd());
}

function subjectBinary(root) {
  return process.env.REPROIT_DOGFOOD_BINARY || join(root, 'target', 'debug', 'reproit');
}

async function readBounded(path) {
  const content = await readFile(path);
  if (content.length > MAX_FILE_BYTES) {
    throw new Error(`${path} exceeds the ${MAX_FILE_BYTES}-byte verifier bound`);
  }
  return content.toString('utf8');
}

/**
 * Defect: a kept guard's contract serializes optional fields as explicit
 * nulls, and the deserializer read a present-but-null key as a malformed
 * struct, so any guard carrying a query-semantics invariant could not load and
 * silently stopped guarding. That is a fail-open in the one artifact the CI
 * gate depends on.
 *
 * The check writes a finding artifact whose contract contains explicit nulls
 * and asks the binary to verify it. The target is deliberately unreachable:
 * we are asserting that the artifact LOADS, not that a request succeeds, so a
 * connection error is a pass and a deserialization error is the defect.
 */
async function contractNullOptionalIsAccepted(root) {
  const binary = subjectBinary(root);
  const directory = await mkdtemp(join(tmpdir(), 'reproit-null-optional-'));
  try {
    const schema = [
      'openapi: 3.1.0',
      'info: { title: guard, version: "1" }',
      'paths:',
      '  /notes:',
      '    get:',
      '      operationId: listNotes',
      '      responses:',
      '        "200": { content: { application/json: { schema: { type: object } } } }',
      '',
    ].join('\n');
    await writeFile(join(directory, 'openapi.yaml'), schema);
    await writeFile(
      join(directory, 'reproit.yaml'),
      'backend:\n  enabled: true\n  schemas: [openapi.yaml]\n',
    );
    const findings = join(directory, '.reproit', 'findings', 'fnd_deadbeef0001');
    await mkdir(findings, { recursive: true });
    const artifact = {
      format: 'reproit-backend-finding',
      version: 3,
      schema: 'openapi.yaml',
      schemaSha256: `sha256:${createHash('sha256').update(schema).digest('hex')}`,
      // Port 9 is the discard service: reliably refused, never answered.
      origin: 'http://127.0.0.1:9',
      reset: { steps: [] },
      setup: [],
      failing: {
        contract: { id: 'listNotes', authority: 'declared' },
        request: { operation: 'listNotes', method: 'GET', url: '/notes', input: {} },
        policy: {
          invariants: [
            {
              kind: 'query-semantics',
              operation: 'listNotes',
              itemsPath: '$.items',
              identityPath: 'id',
              consistency: 'strong',
              // The whole point: `keep` writes these, so they must load.
              sort: null,
              pagination: null,
            },
          ],
        },
      },
      finding: {
        id: 'fnd_deadbeef0001',
        kind: 'backend-server-error',
        fingerprint: 'abc123',
        message: 'guard fixture',
        operation: 'listNotes',
      },
    };
    await writeFile(
      join(findings, 'backend.json'),
      `${JSON.stringify(artifact, null, 2)}\n`,
    );

    let output = '';
    try {
      const result = await execFileAsync(binary, ['internal', 'verify'], {
        cwd: directory,
        timeout: COMMAND_TIMEOUT_MS,
        env: { ...process.env, REPROIT_NO_UPDATE_CHECK: '1' },
      });
      output = `${result.stdout}${result.stderr}`;
    } catch (error) {
      output = `${error.stdout ?? ''}${error.stderr ?? ''}${error.message ?? ''}`;
    }
    // A deserialization refusal names the type it could not build.
    if (/invalid type: null/.test(output)) return false;
    // Absence of that string is NOT evidence of health: a binary that never
    // ran (a missing file, or the SIGKILL macOS delivers to a code-signed
    // binary overwritten in place) also fails to print it. Demand positive
    // evidence that the artifact was loaded and the replay was attempted,
    // and refuse to decide otherwise. A verifier that cannot decide must
    // never report clean, which is the failure mode this project keeps
    // meeting: a harness that stopped early looks exactly like one that
    // passed.
    const reachedReplay =
      /127\.0\.0\.1:9/.test(output) ||
      /error sending request/.test(output) ||
      /fnd_deadbeef0001/.test(output) ||
      /\breproduc/i.test(output) ||
      /\bheld\b/.test(output);
    if (!reachedReplay) {
      throw new Error(
        `the subject binary did not reach the replay: ${output.trim().slice(0, 300) || '(no output)'}`,
      );
    }
    return true;
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

/**
 * Defect: the vocabulary purge moved check's per-project knobs into the
 * reproit.yaml gate section, but repository callers kept passing the deleted
 * flags, so the guard replay step exited 2 with a usage error. In CI that read
 * as a flaky guard rather than a broken caller.
 *
 * --runs survives as a hidden contract flag for config-less suite gates, so it
 * is deliberately absent from this list.
 */
const DELETED_CHECK_FLAGS = ['--devices', '--locale', '--device', '--kind'];
const CALLER_DIRECTORIES = [
  ['.github', 'workflows'],
  ['.github', 'actions'],
  ['validation', 'self-dogfood'],
];

async function callerFiles(root) {
  const found = [];
  for (const parts of CALLER_DIRECTORIES) {
    const directory = join(root, ...parts);
    let entries;
    try {
      entries = await readdir(directory, { withFileTypes: true, recursive: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (!entry.isFile()) continue;
      // Test files legitimately embed the affected fixture text; the callers
      // under contract are the workflows and the runner scripts.
      if (entry.name.startsWith('test_') || entry.name.startsWith('test-')) continue;
      if (!/\.(ya?ml|py|mjs|js|sh)$/.test(entry.name)) continue;
      found.push(join(entry.parentPath ?? directory, entry.name));
      if (found.length > MAX_SCANNED_FILES) {
        throw new Error(`more than ${MAX_SCANNED_FILES} caller files to scan`);
      }
    }
  }
  return found;
}

async function noCallerPassesADeletedCheckFlag(root) {
  const files = await callerFiles(root);
  if (files.length === 0) {
    throw new Error('no caller files found; the subject root is probably wrong');
  }
  for (const file of files) {
    const text = await readBounded(file);
    if (!text.includes('check')) continue;
    for (const flag of DELETED_CHECK_FLAGS) {
      // Only a flag reaching `check` matters. Look at the window after each
      // occurrence of the verb rather than the whole file, so an unrelated
      // command's flag is not miscounted.
      let index = text.indexOf('check');
      while (index !== -1) {
        if (text.slice(index, index + 400).includes(flag)) return false;
        index = text.indexOf('check', index + 5);
      }
    }
  }
  return true;
}

const checks = new Map([
  [
    'contract-null-optional',
    {
      identity: 'cli:contract-null-optional',
      run: contractNullOptionalIsAccepted,
    },
  ],
  [
    'check-flag-callers',
    {
      identity: 'cli:check-flag-callers',
      run: noCallerPassesADeletedCheckFlag,
    },
  ],
]);

async function main() {
  const name = process.argv[2];
  const check = checks.get(name);
  if (!check || process.argv.length !== 3) {
    throw new Error(`expected one check name: ${[...checks.keys()].join(', ')}`);
  }
  const reproduced = !(await check.run(subjectRoot()));
  process.stdout.write(
    `${JSON.stringify({
      identity: check.identity,
      reproduced,
      subjectRoot: subjectRoot(),
    })}\n`,
  );
  process.exitCode = reproduced ? DEFECT_EXIT_CODE : 0;
}

main().catch((error) => {
  process.stderr.write(`self-dogfood CLI verifier: ${error.message}\n`);
  process.exitCode = INFRASTRUCTURE_EXIT_CODE;
});
