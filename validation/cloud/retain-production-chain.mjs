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

// The chain, in order. `required` stages must all be present for the record to
// qualify; the rest are retained when the harness produced them.
const STAGES = [
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
    required: false,
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
  const { qualification = 'FixtureQualified', originSummary } = options;
  if (!originSummary) throw new Error('originSummary must describe the production signal');
  await mkdir(outputRoot, { recursive: true });
  const stages = [];
  for (const stage of STAGES) stages.push(await retainStage(stage, workRoot, outputRoot));
  const missing = stages.filter(
    (stage) => stage.required && (!stage.present || stage.malformed),
  );
  const record = {
    schemaVersion: 1,
    gate: 'D5-production-to-local',
    qualification: missing.length ? 'Unqualified' : qualification,
    originSummary,
    stages,
    missingRequiredStages: missing.map((stage) => stage.id),
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
      + '--origin "description"',
    );
  }
  const originIndex = rest.indexOf('--origin');
  const qualificationIndex = rest.indexOf('--qualification');
  const record = await retainProductionChain(resolve(workRoot), resolve(outputRoot), {
    originSummary: originIndex === -1 ? null : rest[originIndex + 1],
    qualification: qualificationIndex === -1
      ? 'FixtureQualified'
      : rest[qualificationIndex + 1],
  });
  process.stdout.write(`${JSON.stringify({
    schemaVersion: 1,
    output: outputRoot,
    qualification: record.qualification,
    stages: record.stages.filter((stage) => stage.present).length,
    missing: record.missingRequiredStages,
  }, null, 2)}\n`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`retain production chain: ${error.message}\n`);
    process.exitCode = 1;
  });
}
