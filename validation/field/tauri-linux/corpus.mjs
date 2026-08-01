#!/usr/bin/env node

// tauri-linux clean and adversarial corpus.
//
// The field benchmark proves the oracle finds the defect. This proves it stays
// silent on known-good subjects, including subjects that look like the defect.
// Every case is a real run in the same worker the campaign uses, offline, and
// its observation is retained verbatim.
//
// The adversarial cases exist because of a specific weakness this oracle had:
// an empty provider name is not by itself the defect. The Custom Configuration
// preset legitimately leaves the name empty, so an oracle keyed on the name
// alone reports a false positive on a perfectly healthy build. The identity
// therefore requires the pressed preset to be unselected as well.
//
// usage: node validation/field/tauri-linux/corpus.mjs

import { execFile } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const run = promisify(execFile);
const FIELD = dirname(fileURLToPath(import.meta.url));
const CLI_ROOT = resolve(FIELD, '../../..');
const LAB_ROOT = process.env.REPROIT_LAB_ROOT ?? resolve(CLI_ROOT, '../reproit-lab');
const SUBJECT = resolve(LAB_ROOT, '.work/tauri-campaign/cc-switch');
const PROBE = resolve(CLI_ROOT, 'validation/field');
const OUTPUT = resolve(CLI_ROOT, 'validation/field/corpus/tauri-linux.json');
const IMAGE = 'reproit-field-tauri-linux:amd64';
const CONTAINER = 'reproit-field-tauri-linux-corpus';
const REPOSITORY = 'https://github.com/farion1231/cc-switch';
const FIXED = '81d6002ace328cf74c9b63e32b15279a7c445812';
const STAGE_TIMEOUT_MS = 3_600_000;
const RUN_TIMEOUT_MS = 900_000;

const CASES = [
  {
    id: 'cc-switch-clean-fixed-search-select',
    kind: 'clean',
    variant: 'default',
    why: 'the ordinary trigger on the fixed build: the preset reached through the '
      + 'search is selected and fills the provider name',
  },
  {
    id: 'cc-switch-adversarial-custom-preset',
    kind: 'adversarial',
    variant: 'custom-preset-legal',
    why: 'the Custom Configuration preset legitimately leaves the provider name '
      + 'empty, which is the defect\'s own observable arriving from legal behavior',
  },
  {
    id: 'cc-switch-adversarial-no-search',
    kind: 'adversarial',
    variant: 'no-search-legal',
    why: 'a preset pressed without ever opening the search exercises the same '
      + 'pointer path with the click-outside handler inert',
  },
];

async function docker(args, timeout) {
  const { stdout } = await run('docker', args, { timeout, maxBuffer: 8 * 1024 * 1024 });
  return stdout.trim();
}

async function removeContainer() {
  await docker(['rm', '-f', CONTAINER], 120_000).catch(() => '');
}

async function stageFixed() {
  await docker([
    'run', '--rm', '--platform', 'linux/amd64', '-e', `revision=${FIXED}`,
    '-v', `${SUBJECT}:/work`, '-v', `${FIELD}:/field:ro`,
    IMAGE, 'bash', '/field/stage-cc-switch.sh',
  ], STAGE_TIMEOUT_MS);
}

async function observeCase(entry) {
  await removeContainer();
  await docker([
    'run', '-d', '--name', CONTAINER, '--platform', 'linux/amd64', '--network', 'none',
    '-e', 'APP_BIN=/work/src-tauri/target/debug/cc-switch',
    '-e', 'SCENARIO=preset-pointer-select',
    '-e', `VARIANT=${entry.variant}`,
    '-v', `${SUBJECT}:/work`, '-v', `${FIELD}:/field:ro`, '-v', `${PROBE}:/probe:ro`,
    IMAGE, 'bash', '/field/launch.sh',
  ], 300_000);

  const ask = (verb) => docker(
    ['exec', CONTAINER, 'node', '/probe/probe-tauri.mjs', 'ask', verb],
    RUN_TIMEOUT_MS,
  );
  let ready = false;
  for (let attempt = 0; attempt < 90 && !ready; attempt += 1) {
    ready = await ask('readiness').then(() => true).catch(() => false);
    if (!ready) await new Promise((r) => setTimeout(r, 2_000));
  }
  if (!ready) throw new Error(`${entry.id}: the probe never became ready`);
  await ask('trigger');
  const observation = JSON.parse(await ask('observe'));
  await removeContainer();
  return observation;
}

async function main() {
  await stageFixed();
  const cases = [];
  for (const entry of CASES) {
    const observation = await observeCase(entry);
    if (observation.identity !== null) {
      throw new Error(
        `${entry.id} is a known-good subject but reported ${observation.identity}`,
      );
    }
    if (observation.observationReached !== true) {
      throw new Error(`${entry.id} never reached its observation point`);
    }
    cases.push({
      id: entry.id,
      kind: entry.kind,
      application: 'cc-switch-preset-click-4315',
      repository: REPOSITORY,
      revision: FIXED,
      fixture: 'cc-switch-add-provider',
      variant: entry.variant,
      why: entry.why,
      observationReached: true,
      identity: null,
      falsePositive: false,
      observation,
    });
    process.stdout.write(`${entry.id}: no identity\n`);
  }
  const remaining = await docker(['ps', '-a', '--format', '{{.Names}}'], 120_000);
  const containersRemaining = remaining
    .split('\n')
    .filter((name) => name.trim() === CONTAINER).length;
  const document = {
    schemaVersion: 1,
    target: 'tauri-linux',
    worker: { image: IMAGE, platform: 'linux/amd64', network: 'none' },
    cleanCases: cases.filter((entry) => entry.kind === 'clean').length,
    adversarialCases: cases.filter((entry) => entry.kind === 'adversarial').length,
    confirmedFalsePositives: 0,
    unreachedObservations: 0,
    containersRemaining,
    cases,
  };
  await mkdir(dirname(OUTPUT), { recursive: true });
  await writeFile(OUTPUT, `${JSON.stringify(document, null, 2)}\n`);
  process.stdout.write(
    `tauri-linux corpus: ${document.cleanCases} clean, `
    + `${document.adversarialCases} adversarial, 0 false positives\n`,
  );
}

await main();
