#!/usr/bin/env node

// Gate D5 evidence retention.
//
// `run-production-loop.sh` already walks the whole production-to-local chain,
// but it runs inside a mktemp directory that is deleted on exit, so nothing
// survives. `reproit-proof/production/` therefore records the harness as
// "incomplete: retains no evidence files after a run". This tool closes that
// exact gap: it copies the chain byte-for-byte out of the harness work
// directory, redacts every secret, digests every file, and writes one
// sanitized production record.
//
// It never invents a step. A stage whose artifact is absent is recorded as
// missing, and a record with any missing required stage is not qualified.

import { createHash } from 'node:crypto';
import { mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { sanitizeEvidenceText } from '../sanitize-evidence.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const MAX_FILE_BYTES = 8 * 1024 * 1024;
const QUALIFICATIONS = new Set(['FixtureQualified', 'IndependentQualified']);
const REVISION_PATTERN = /^(?:git:[a-f0-9]{40}|sha256:[a-f0-9]{64})$/;

// The chain, in order. `required` stages must all be present for the record to
// qualify; the rest are retained when the harness produced them.
const STAGES = [
  {
    id: 'reset',
    file: 'reset.json',
    required: true,
    summary: 'clean application workspace and disposable Cloud project reset',
    redactJson: true,
  },
  {
    id: 'production-signal',
    file: 'project.json',
    required: true,
    summary: 'disposable Cloud project created for the production signal',
    redactJson: true,
  },
  {
    id: 'cloud-ingestion',
    file: 'hosted.json',
    required: true,
    summary: 'authenticated hosted ingest, grouping, and bucket creation',
    redactJson: true,
  },
  {
    id: 'local-materialization',
    file: 'pull.log',
    required: true,
    summary: 'replay package pulled into a clean developer workspace',
  },
  {
    id: 'exact-local-reproduction',
    file: 'check.json',
    required: true,
    summary: 'local check reproduced the exact production identity',
    redactJson: true,
  },
  {
    id: 'direct-replay',
    file: 'direct-replay.log',
    required: true,
    summary: 'direct bucket command confirmed the failure',
  },
  {
    id: 'reproduction-stderr',
    file: 'check.err',
    required: false,
    summary: 'local check diagnostics',
  },
  {
    id: 'retention-and-deletion',
    file: 'delete.json',
    required: true,
    summary: 'disposable project deletion response',
    redactJson: true,
  },
];

// Anything that could carry a live credential out of the harness.
const SECRET_PATTERNS = [
  [/sk_live_[A-Za-z0-9_-]+/g, '[REDACTED_ACCOUNT_KEY]'],
  [/pk_live_[A-Za-z0-9_-]+/g, '[REDACTED_PUBLISHABLE_KEY]'],
  [/\bBearer\s+[A-Za-z0-9._-]+/g, 'Bearer [REDACTED]'],
  [/"apiKey"\s*:\s*"[^"]*"/g, '"apiKey": "[REDACTED]"'],
  [/"publishableKey"\s*:\s*"[^"]*"/g, '"publishableKey": "[REDACTED]"'],
];

function redact(text) {
  let output = sanitizeEvidenceText(text, ROOT);
  for (const [pattern, replacement] of SECRET_PATTERNS) {
    output = output.replace(pattern, replacement);
  }
  return output;
}

function isJson(text) {
  try {
    JSON.parse(text);
    return true;
  } catch {
    return false;
  }
}

function sha256(text) {
  return `sha256:${createHash('sha256').update(text).digest('hex')}`;
}

function nonEmpty(value) {
  return typeof value === 'string' && value.trim().length > 0;
}

function qualificationContract(contract, qualification) {
  const blockers = [];
  const requireValue = (condition, field) => {
    if (!condition) blockers.push(field);
  };
  requireValue(contract && typeof contract === 'object', 'contract');
  if (!contract || typeof contract !== 'object') return { blockers };

  requireValue(nonEmpty(contract.targetId), 'targetId');
  requireValue(
    contract.originKind === 'fixture' || contract.originKind === 'independent-application',
    'originKind',
  );
  const expectedOrigin = qualification === 'IndependentQualified'
    ? 'independent-application'
    : 'fixture';
  requireValue(contract.originKind === expectedOrigin, 'originKind-for-qualification');

  const revisions = contract.revisions;
  requireValue(revisions && typeof revisions === 'object', 'revisions');
  requireValue(REVISION_PATTERN.test(revisions?.cli || ''), 'revisions.cli');
  requireValue(REVISION_PATTERN.test(revisions?.application || ''), 'revisions.application');
  requireValue(nonEmpty(revisions?.sdk?.name), 'revisions.sdk.name');
  requireValue(
    REVISION_PATTERN.test(revisions?.sdk?.revision || ''),
    'revisions.sdk.revision',
  );

  requireValue(nonEmpty(contract.local?.provider), 'local.provider');
  requireValue(contract.local?.trusted === true, 'local.trusted');

  const commands = contract.execution?.commands;
  requireValue(Array.isArray(commands) && commands.length > 0, 'execution.commands');
  for (const [index, command] of (commands || []).entries()) {
    requireValue(nonEmpty(command?.stage), `execution.commands[${index}].stage`);
    requireValue(nonEmpty(command?.command), `execution.commands[${index}].command`);
    requireValue(
      Array.isArray(command?.assertions)
        && command.assertions.length > 0
        && command.assertions.every(nonEmpty),
      `execution.commands[${index}].assertions`,
    );
  }
  for (const phase of ['reset', 'cleanup']) {
    requireValue(nonEmpty(contract.execution?.[phase]?.command), `execution.${phase}.command`);
    requireValue(
      Array.isArray(contract.execution?.[phase]?.evidence)
        && contract.execution[phase].evidence.length > 0
        && contract.execution[phase].evidence.every(nonEmpty),
      `execution.${phase}.evidence`,
    );
  }
  return { contract, blockers };
}

async function retainedJson(outputRoot, file) {
  try {
    return JSON.parse(await readFile(join(outputRoot, file), 'utf8'));
  } catch {
    return {};
  }
}

async function readBounded(path) {
  const metadata = await stat(path);
  if (!metadata.isFile()) throw new Error(`${path} is not a regular file`);
  if (metadata.size > MAX_FILE_BYTES) throw new Error(`${path} exceeds the retention bound`);
  return readFile(path, 'utf8');
}

async function retainStage(stage, workRoot, outputRoot) {
  const source = join(workRoot, stage.file);
  let raw;
  try {
    raw = await readBounded(source);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    return { id: stage.id, summary: stage.summary, present: false, required: stage.required };
  }
  if (raw.length === 0) {
    // An empty artifact means the stage produced nothing, which is a missing
    // stage, not a retention error.
    return {
      id: stage.id,
      summary: stage.summary,
      present: false,
      required: stage.required,
      reason: 'the harness produced an empty artifact',
    };
  }
  const sanitized = redact(raw);
  let malformed = false;
  if (stage.redactJson) {
    const wasJson = isJson(raw);
    if (!isJson(sanitized)) {
      // Redaction must never destroy a document. If it was valid JSON before
      // and is not after, that is a bug here and must fail closed. If it was
      // already malformed, that is the harness's output and is recorded.
      if (wasJson) {
        throw new Error(`${stage.file} stopped being JSON after redaction`);
      }
      malformed = true;
    }
  }
  const destination = join(outputRoot, stage.file);
  await writeFile(destination, sanitized, { mode: 0o644 });
  return {
    id: stage.id,
    summary: stage.summary,
    present: true,
    required: stage.required,
    file: stage.file,
    bytes: Buffer.byteLength(sanitized, 'utf8'),
    malformed,
    rawSha256: sha256(raw),
    sanitizedSha256: sha256(sanitized),
  };
}

function assertNoSecret(record) {
  const serialized = JSON.stringify(record);
  for (const [pattern] of SECRET_PATTERNS.slice(0, 2)) {
    if (pattern.test(serialized)) {
      throw new Error('the production record still contains a live credential');
    }
  }
}

export async function retainProductionChain(workRoot, outputRoot, options = {}) {
  const { qualification = 'FixtureQualified', originSummary, contract = null } = options;
  if (!originSummary) throw new Error('originSummary must describe the production signal');
  const requestedQualification = QUALIFICATIONS.has(qualification)
    ? qualification
    : 'Unqualified';
  const contractResult = qualificationContract(contract, requestedQualification);
  if (!QUALIFICATIONS.has(qualification)) {
    contractResult.blockers.push('qualification');
  }
  await mkdir(outputRoot, { recursive: true });
  const stages = [];
  for (const stage of STAGES) stages.push(await retainStage(stage, workRoot, outputRoot));
  const missing = stages.filter(
    (stage) => stage.required && (!stage.present || stage.malformed),
  );
  const stageIds = new Set(stages.map((stage) => stage.id));
  const requiredStageIds = STAGES.filter((stage) => stage.required).map((stage) => stage.id);
  const commandStages = new Set(
    (contract?.execution?.commands || []).map((command) => command.stage),
  );
  for (const stageId of requiredStageIds) {
    if (!commandStages.has(stageId)) {
      contractResult.blockers.push(`execution.commands.${stageId}`);
    }
  }
  for (const stageId of commandStages) {
    if (!stageIds.has(stageId)) {
      contractResult.blockers.push(`execution.commands.unknown-stage.${stageId}`);
    }
  }
  if (!contract?.execution?.reset?.evidence?.includes('reset')) {
    contractResult.blockers.push('execution.reset.evidence.reset');
  }
  if (!contract?.execution?.cleanup?.evidence?.includes('retention-and-deletion')) {
    contractResult.blockers.push('execution.cleanup.evidence.retention-and-deletion');
  }
  const project = await retainedJson(outputRoot, 'project.json');
  const hosted = await retainedJson(outputRoot, 'hosted.json');
  const cloud = {
    baseUrl: hosted.base || null,
    projectId: hosted.projectId || project.appId || null,
    occurrenceId: hosted.occurrenceId || null,
    bucketId: hosted.bucketId || null,
  };
  for (const [field, value] of Object.entries(cloud)) {
    if (!nonEmpty(value)) contractResult.blockers.push(`cloud.${field}`);
  }
  const qualificationBlockers = [
    ...new Set([
      ...contractResult.blockers,
      ...missing.map((stage) => `stage.${stage.id}`),
    ]),
  ];
  const record = {
    schemaVersion: 2,
    gate: 'D5-production-to-local',
    targetId: contract?.targetId || null,
    qualification: qualificationBlockers.length ? 'Unqualified' : requestedQualification,
    origin: {
      kind: contract?.originKind || null,
      summary: originSummary,
    },
    revisions: contract?.revisions || null,
    cloud,
    local: contract?.local || null,
    execution: contract?.execution || null,
    stages,
    missingRequiredStages: missing.map((stage) => stage.id),
    qualificationBlockers,
    chainSha256: sha256(
      stages
        .filter((stage) => stage.present)
        .map((stage) => `${stage.id}:${stage.sanitizedSha256}`)
        .join('\n'),
    ),
  };
  assertNoSecret(record);
  await writeFile(
    join(outputRoot, 'record.json'),
    `${JSON.stringify(record, null, 2)}\n`,
    { mode: 0o644 },
  );
  return record;
}

async function main(argv) {
  const [workRoot, outputRoot, ...rest] = argv;
  if (!workRoot || !outputRoot) {
    throw new Error(
      'usage: retain-production-chain.mjs WORK_DIR OUTPUT_DIR [--qualification NAME] '
      + '--origin "description" --contract CONTRACT.json',
    );
  }
  const originIndex = rest.indexOf('--origin');
  const qualificationIndex = rest.indexOf('--qualification');
  const contractIndex = rest.indexOf('--contract');
  const contract = contractIndex === -1
    ? null
    : JSON.parse(await readBounded(resolve(rest[contractIndex + 1])));
  const record = await retainProductionChain(resolve(workRoot), resolve(outputRoot), {
    originSummary: originIndex === -1 ? null : rest[originIndex + 1],
    qualification: qualificationIndex === -1
      ? 'FixtureQualified'
      : rest[qualificationIndex + 1],
    contract,
  });
  process.stdout.write(`${JSON.stringify({
    schemaVersion: 2,
    output: outputRoot,
    qualification: record.qualification,
    stages: record.stages.filter((stage) => stage.present).length,
    missing: record.missingRequiredStages,
    blockers: record.qualificationBlockers,
  }, null, 2)}\n`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`retain production chain: ${error.message}\n`);
    process.exitCode = 1;
  });
}
