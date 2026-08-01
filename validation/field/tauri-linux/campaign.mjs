#!/usr/bin/env node

// tauri-linux field campaigns.
//
// One adapter, the same bounded-phase contract the electron-linux campaigns
// use. The worker is x86_64, the application always launches with
// --network none, and every container the campaign owns is removed by cleanup
// on every exit path.
//
//   cc-switch-preset-click-4315  the click-outside handler was bound to a
//                                container ref wrapping only the search row, so
//                                the mousedown on a search result cleared the
//                                search before the click landed and the preset
//                                was never selected.
//
// The phase scripts, the worker Dockerfile, and this driver live together in
// this directory so the campaign that produced the retained evidence is itself
// version controlled. The bounded-phase adapter it drives lives in the lab
// checkout, whose location is REPROIT_LAB_ROOT (default ../../../../reproit-lab).
//
// usage: node validation/field/tauri-linux/campaign.mjs <application-id>

import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const FIELD = dirname(fileURLToPath(import.meta.url));
const CLI_ROOT = resolve(FIELD, '../../..');
const LAB_ROOT = process.env.REPROIT_LAB_ROOT ?? resolve(CLI_ROOT, '../reproit-lab');
const { benchmarkDocument, runFieldCampaign } = await import(
  resolve(LAB_ROOT, 'src/field-campaign.mjs')
);

const WORK_ROOT = resolve(LAB_ROOT, '.work/tauri-campaign');
const PROBE = resolve(CLI_ROOT, 'validation/field');
const EVIDENCE_ROOT = resolve(CLI_ROOT, 'validation/field/evidence');
const BENCHMARK = resolve(CLI_ROOT, 'validation/field/tauri-linux.json');

const IMAGE = 'reproit-field-tauri-linux:amd64';
const DOCKER_CONFIG = process.env.DOCKER_CONFIG
  ?? resolve(process.env.HOME ?? '/root', '.docker');

export const APPLICATIONS = {
  'cc-switch-preset-click-4315': {
    repository: 'https://github.com/farion1231/cc-switch',
    issueUrl: 'https://github.com/farion1231/cc-switch/issues/4302',
    affected: 'caa912e3a39c60330fad641b295ae8b13cdea586',
    fixed: '81d6002ace328cf74c9b63e32b15279a7c445812',
    authority: 'authored-contract',
    identity: 'preset-search:result-not-selected-by-pointer',
    subject: 'cc-switch',
    stage: '/field/stage-cc-switch.sh',
    depsMarker: 'node_modules',
    install: 'cd /work && corepack enable && '
      + 'pnpm install --frozen-lockfile --config.dangerouslyAllowAllBuilds=true',
    appBin: '/work/src-tauri/target/debug/cc-switch',
    scenario: 'preset-pointer-select',
    neighboringLegalBehavior:
      'the same pointer press on a preset reached without the search still selects it',
    minimizedAction:
      'type kimi into the provider preset search and press the Kimi result once',
  },
  'readest-select-mode-last-book-5200': {
    repository: 'https://github.com/readest/readest',
    issueUrl: 'https://github.com/readest/readest/issues/5175',
    affected: '54ad2e9166c54fa3a4956961bb4991fc2bbc22d2',
    fixed: '09548d998f16be10315d988176a2e7acddce3473',
    authority: 'platform',
    identity: 'library-select-mode:last-book-unreachable-under-action-bar',
    subject: 'readest',
    stage: '/field/stage-readest.sh',
    depsMarker: 'node_modules',
    install: 'cd /work && corepack enable && '
      + 'pnpm install --frozen-lockfile --config.dangerouslyAllowAllBuilds=true',
    appBin: '/work/target/debug/Readest',
    scenario: 'library-select-last-book',
    // readest opens every path it is given on argv, which is how the campaign
    // seeds a library without a native file chooser.
    books: 24,
    neighboringLegalBehavior:
      'the row above the last one, pressed the same way with the same bar on screen, '
      + 'still toggles its selection',
    minimizedAction:
      'enter select mode in list view and press the last book row once',
  },
};

function probeArgv(container, verb) {
  return ['docker', 'exec', container, 'node', '/probe/probe-tauri.mjs', 'ask', verb];
}

export function buildSpec(id) {
  const application = APPLICATIONS[id];
  if (!application) throw new Error(`unknown application ${id}`);
  const subject = resolve(WORK_ROOT, application.subject);
  const container = `reproit-field-tauri-linux-${application.subject}`;
  const minContainer = `${container}-min`;
  // Subjects that take their library from argv get a generated book directory
  // mounted read-only, and the paths are passed to the probe.
  const books = application.books ?? 0;
  const booksRoot = resolve(WORK_ROOT, 'books');
  const appArguments = Array.from(
    { length: books },
    (_, index) => `/books/field-book-${String(index + 1).padStart(2, '0')}.txt`,
  ).join(',');
  const booksMount = books ? ` -v "${booksRoot}:/books:ro"` : '';
  const runtime = [
    `-e APP_BIN=${application.appBin}`,
    `-e SCENARIO=${application.scenario}`,
    `-e APP_ARGS=${appArguments}`,
  ].join(' ');

  const shared = {
    DOCKER_CONFIG,
    CAMPAIGN_IMAGE: IMAGE,
    CAMPAIGN_CONTAINER: container,
    CAMPAIGN_MIN_CONTAINER: minContainer,
    CAMPAIGN_SUBJECT: subject,
    CAMPAIGN_REPOSITORY: application.repository,
    CAMPAIGN_FIELD: FIELD,
    CAMPAIGN_PROBE: PROBE,
    CAMPAIGN_AFFECTED: application.affected,
    CAMPAIGN_IDENTITY: application.identity,
    CAMPAIGN_STAGE: application.stage,
    CAMPAIGN_DEPS_MARKER: application.depsMarker,
    CAMPAIGN_INSTALL: application.install,
    CAMPAIGN_RETAIN: resolve(WORK_ROOT, `${id}.retain.txt`),
    CAMPAIGN_BOOKS: books ? booksRoot : '',
    CAMPAIGN_BOOK_COUNT: String(books),
    CAMPAIGN_BOOKS_MOUNT: booksMount,
    APP_BIN: application.appBin,
    APP_ARGS: appArguments,
    SCENARIO: application.scenario,
  };

  const phases = {
    prepare: { argv: ['bash', `${FIELD}/prepare.sh`], env: shared, timeoutMs: 3_600_000 },
    reset: {
      argv: ['bash', '-c', `docker rm -f ${container} ${minContainer} >/dev/null 2>&1; true`],
      env: shared,
      timeoutMs: 120_000,
    },
    build: {
      argv: [
        'bash', '-c',
        'docker run --rm --platform linux/amd64 -e revision="$revision"'
        + ` -v "${subject}:/work" -v "${FIELD}:/field:ro" ${IMAGE}`
        + ` bash ${application.stage}`,
      ],
      env: shared,
      timeoutMs: 3_600_000,
    },
    launch: {
      argv: [
        'bash', '-c',
        `docker run -d --name ${container} --platform linux/amd64 --network none ${runtime}`
        + ` -v "${subject}:/work" -v "${FIELD}:/field:ro" -v "${PROBE}:/probe:ro"`
        + `${booksMount} ${IMAGE} bash /field/launch.sh`,
      ],
      env: shared,
      timeoutMs: 300_000,
    },
    readiness: {
      argv: [
        'bash', '-c',
        'for i in $(seq 1 90); do'
        + ` if docker exec ${container} node /probe/probe-tauri.mjs ask readiness;`
        + ' then exit 0; fi;'
        + ' sleep 2; done; echo "probe never became ready" >&2; exit 1',
      ],
      env: shared,
      timeoutMs: 900_000,
    },
    trigger: { argv: probeArgv(container, 'trigger'), env: shared, timeoutMs: 300_000 },
    observe: { argv: probeArgv(container, 'observe'), env: shared, timeoutMs: 300_000 },
    minimize: { argv: ['bash', `${FIELD}/minimize.sh`], env: shared, timeoutMs: 3_600_000 },
    control: { argv: ['bash', `${FIELD}/control.sh`], env: shared, timeoutMs: 900_000 },
    cleanup: { argv: ['bash', `${FIELD}/cleanup.sh`], env: shared, timeoutMs: 600_000 },
    retain: { argv: ['bash', `${FIELD}/retain.sh`], env: shared, timeoutMs: 300_000 },
  };

  return {
    target: 'tauri-linux',
    workRoot: WORK_ROOT,
    evidenceRoot: EVIDENCE_ROOT,
    application: {
      id,
      repository: application.repository,
      issueUrl: application.issueUrl,
      affectedRevision: application.affected,
      fixedRevision: application.fixed,
      authority: application.authority,
      expectedIdentity: application.identity,
      neighboringLegalBehavior: application.neighboringLegalBehavior,
      minimizedAction: application.minimizedAction,
      // WebKitGTK exposes no JS heap through WebDriver.
      memoryMeasurement: 'unavailable',
    },
    phases,
  };
}

async function writeBenchmark() {
  const entries = await readdir(WORK_ROOT);
  const applications = [];
  for (const id of Object.keys(APPLICATIONS)) {
    if (!entries.includes(`${id}.benchmark.json`)) continue;
    const body = await readFile(resolve(WORK_ROOT, `${id}.benchmark.json`), 'utf8');
    applications.push(JSON.parse(body));
  }
  const document = benchmarkDocument('tauri-linux', applications);
  await writeFile(BENCHMARK, `${JSON.stringify(document, null, 2)}\n`);
  return document;
}

async function main() {
  const id = process.argv[2];
  if (!id) throw new Error(`usage: tauri-campaign.mjs <${Object.keys(APPLICATIONS).join('|')}>`);
  await mkdir(WORK_ROOT, { recursive: true });
  const result = await runFieldCampaign(buildSpec(id));
  await writeFile(
    resolve(WORK_ROOT, `${id}.result.json`),
    `${JSON.stringify(result, null, 2)}\n`,
  );
  if (result.status !== 'passed') {
    process.stderr.write(`${JSON.stringify(result.failure, null, 2)}\n`);
    process.exitCode = 1;
    return;
  }
  await writeFile(
    resolve(WORK_ROOT, `${id}.benchmark.json`),
    `${JSON.stringify(result.benchmark, null, 2)}\n`,
  );
  const document = await writeBenchmark();
  process.stdout.write(
    `tauri-linux campaign passed: ${id} (benchmark ${document.status}, `
    + `${document.applications.length} application(s))\n`,
  );
}

if (import.meta.url === `file://${process.argv[1]}`) await main();
