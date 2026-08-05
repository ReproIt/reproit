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
    '      - name: ',
  );
  // The dispatch is the product path a customer copies: plain `reproit
  // check`, no wrapper script and no flag vocabulary.
  if (
    !step.includes('target/debug/reproit check') ||
    step.includes('run-required-guards.py')
  ) {
    return false;
  }
  // Plain `check` only enforces the corpus if its enumeration is fail-closed
  // in the product: strict store validation, wired into the suite paths.
  const corpus = await readBounded(
    resolve(root, 'crates/reproit/src/domain/repro/corpus.rs'),
    MAX_RUNNER_BYTES,
  );
  const suite = await readBounded(
    resolve(root, 'crates/reproit/src/workflows/check.rs'),
    MAX_RUNNER_BYTES,
  );
  return (
    corpus.includes('pub fn load_corpus') &&
    corpus.includes('is not a content-addressed guard directory') &&
    corpus.includes('does not identify its directory') &&
    suite.includes('repro::load_corpus(&loaded.root)')
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
