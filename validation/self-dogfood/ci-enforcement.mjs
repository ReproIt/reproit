#!/usr/bin/env node

import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';

const DEFECT_EXIT_CODE = 17;
const INFRASTRUCTURE_EXIT_CODE = 70;
const MAX_WORKFLOW_BYTES = 512 * 1024;
const MAX_RUNNER_BYTES = 128 * 1024;

function subjectRoot() {
  const configured = process.env.REPROIT_DOGFOOD_SUBJECT_ROOT;
  return resolve(configured || process.cwd());
}

async function readBounded(path, maximumBytes) {
  const content = await readFile(path);
  if (content.length > maximumBytes) {
    throw new Error(`${path} exceeds the ${maximumBytes}-byte verifier bound`);
  }
  return content.toString('utf8');
}

function workflowJob(workflow, startMarker, endMarker) {
  const start = workflow.indexOf(startMarker);
  if (start === -1) {
    return '';
  }
  const end = workflow.indexOf(endMarker, start + startMarker.length);
  return workflow.slice(start, end === -1 ? workflow.length : end);
}

async function requiredCorpusIsEnforced(root) {
  const workflow = await readBounded(
    resolve(root, '.github/workflows/ci.yml'),
    MAX_WORKFLOW_BYTES,
  );
  const step = workflowJob(
    workflow,
    '      - name: Replay the complete required self-dogfood guard corpus',
    '      - name: Test the self-dogfood validation scripts',
  );
  if (!step.includes('validation/self-dogfood/run-required-guards.py')) {
    return false;
  }
  const runner = await readBounded(
    resolve(root, 'validation/self-dogfood/run-required-guards.py'),
    MAX_RUNNER_BYTES,
  );
  // Replay repetition moved from a per-call flag into the project's gate
  // config; the contract pins BOTH halves so neither can silently vanish.
  const gate = await readBounded(resolve(root, '.reproit/reproit.yaml'), MAX_RUNNER_BYTES);
  return (
    runner.includes('"check"') &&
    runner.includes('"--strict"') &&
    runner.includes('status == "required"') &&
    /gate:\s*\n\s*runs:\s*3/.test(gate)
  );
}

async function directPushPolicyIsEnforced(root) {
  const workflow = await readBounded(
    resolve(root, '.github/workflows/ci.yml'),
    MAX_WORKFLOW_BYTES,
  );
  const job = workflowJob(workflow, '  dogfood-policy:', '\n  windows-build:');
  return (
    job.includes("github.event_name == 'push'") &&
    job.includes('github.event.before') &&
    job.includes('github.event.after') &&
    job.includes('fetch-depth: 0')
  );
}

const checks = new Map([
  [
    'required-guard-corpus-dispatch',
    {
      identity: 'ci:required-guard-corpus-dispatch',
      run: requiredCorpusIsEnforced,
    },
  ],
  [
    'direct-push-dogfood-policy',
    {
      identity: 'ci:direct-push-dogfood-policy',
      run: directPushPolicyIsEnforced,
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
  process.stderr.write(`self-dogfood CI verifier: ${error.message}\n`);
  process.exitCode = INFRASTRUCTURE_EXIT_CODE;
});
