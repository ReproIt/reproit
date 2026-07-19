import { spawn, spawnSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { request } from 'node:http';
import process from 'node:process';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '../../..');
const artifacts = process.env.REPROIT_BACKEND_ARTIFACTS ?? join(here, 'artifacts');
const host = process.env.REPROIT_HOST_LABEL ?? `${process.platform}-${process.arch}`;
const requested = (process.env.REPROIT_SERVICES ?? 'node,python,go').split(',').filter(Boolean);
const modes = ['clean', 'broken', 'incomplete'];
const targetDir = process.env.CARGO_TARGET_DIR ?? join(here, 'target');
const validator = join(
  targetDir,
  'debug',
  process.platform === 'win32'
    ? 'reproit-real-backend-validator.exe'
    : 'reproit-real-backend-validator',
);
const services = {
  node: { command: 'node', args: [join(here, 'services/node-service.mjs')] },
  python: { command: 'python3', args: [join(here, 'services/python_service.py')] },
  go: { command: 'go', args: ['run', join(here, 'services/go-service.go')] },
};

function available(command) {
  const probe = process.platform === 'win32' ? ['where', [command]] : ['sh', ['-c', `command -v ${command}`]];
  return spawnSync(probe[0], probe[1], { stdio: 'ignore' }).status === 0;
}

function exchange(port, path, headers = {}) {
  return new Promise((resolve, reject) => {
    const req = request({ hostname: '127.0.0.1', port, path, method: 'GET', headers }, (res) => {
      const chunks = [];
      res.on('data', (chunk) => chunks.push(chunk));
      res.on('end', () => resolve({
        requestMethod: 'GET', requestTarget: path.split('?')[0], requestHeaders: headers,
        requestBody: [], responseStatus: res.statusCode, responseHeaders: res.headers,
        responseBody: [...Buffer.concat(chunks)],
      }));
    });
    req.on('error', reject);
    req.end();
  });
}

async function ready(port, child) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (child.exitCode !== null) throw new Error(`service exited ${child.exitCode}`);
    try { await exchange(port, '/health'); return; } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error('service readiness timed out');
}

function textBody(exchangeValue) {
  return Buffer.from(exchangeValue.responseBody).toString('utf8');
}

async function capture(service, mode, port) {
  const initialHeaders = { 'accept-language': 'en-US' };
  const initialCache = await exchange(port, '/representation', initialHeaders);
  const etag = initialCache.responseHeaders.etag;
  const conditionalHeaders = {
    'accept-language': mode === 'incomplete' ? 'fr-FR' : 'en-US',
    'if-none-match': etag,
  };
  const conditionalCache = await exchange(port, '/representation', conditionalHeaders);
  const codecResponse = await exchange(port, '/codec?value=9007199254740993');
  const media = await exchange(port, '/media');
  const lifecycleResponse = await exchange(port, '/lifecycle');
  const codecBody = JSON.parse(textBody(codecResponse));
  const lifecycleEvidence = JSON.parse(textBody(lifecycleResponse));
  return {
    schemaVersion: 1, service, mode, host,
    codec: { input: '9007199254740993', output: codecBody.decoded ?? null },
    initialCache, conditionalCache, media,
    lifecycleContract: {
      scopeKind: 'request',
      rules: [
        { kind: 'precedence', before: 'request.start', after: 'request.close' },
        { kind: 'forbid-after', event: 'callback', boundary: 'request.close' },
        { kind: 'cardinality', event: 'request.close', atLeast: 1, atMost: 1 },
      ],
    },
    lifecycleEvidence,
  };
}

async function runOne(name, mode, port) {
  const definition = services[name];
  const logs = [];
  const child = spawn(definition.command, definition.args, {
    cwd: root,
    env: { ...process.env, PORT: String(port), REPROIT_FIXTURE_MODE: mode },
    stdio: ['ignore', 'pipe', 'pipe'],
    // `go run` launches a compiled child process. Isolate the process group so
    // cleanup terminates both the tool and the actual HTTP service.
    detached: process.platform !== 'win32',
  });
  child.stdout.on('data', (chunk) => logs.push(chunk));
  child.stderr.on('data', (chunk) => logs.push(chunk));
  try {
    await ready(port, child);
    const captured = await capture(name, mode, port);
    const directory = join(artifacts, host);
    await mkdir(directory, { recursive: true });
    const path = join(directory, `${name}-${mode}.json`);
    const serialized = `${JSON.stringify(captured, null, 2)}\n`;
    await writeFile(path, serialized);
    const validation = spawnSync(validator, [path], {
      cwd: root,
      encoding: 'utf8',
      env: process.env,
    });
    if (validation.status !== 0) {
      throw new Error(`validator failed (${validation.status}): ${validation.stdout}${validation.stderr}`);
    }
    return {
      service: name, mode, status: 'passed',
      evidenceSha256: createHash('sha256').update(serialized).digest('hex'),
      validator: JSON.parse(validation.stdout.trim()),
    };
  } finally {
    if (child.exitCode === null) {
      if (process.platform === 'win32') child.kill();
      else {
        try { process.kill(-child.pid, 'SIGTERM'); } catch {}
      }
      await new Promise((resolve) => child.once('exit', resolve));
    }
    if (logs.length) await writeFile(join(artifacts, host, `${name}-${mode}.log`), Buffer.concat(logs));
  }
}

await mkdir(join(artifacts, host), { recursive: true });
// Build once before any service is resident. Besides being much faster than
// nine `cargo run` calls, this avoids memory-constrained hosts linking the
// validator while a language runtime is already serving requests.
const validatorBuild = spawnSync(
  'cargo',
  ['build', '--locked', '--quiet', '--manifest-path', join(here, 'Cargo.toml')],
  { cwd: root, encoding: 'utf8', env: { ...process.env, CARGO_TARGET_DIR: targetDir } },
);
if (validatorBuild.status !== 0) {
  throw new Error(
    `validator build failed (${validatorBuild.status}): ` +
      `${validatorBuild.stdout}${validatorBuild.stderr}`,
  );
}
const results = [];
for (const name of requested) {
  const definition = services[name];
  if (!definition) throw new Error(`unknown service ${name}`);
  if (!available(definition.command)) {
    results.push({ service: name, status: 'unavailable', reason: `${definition.command} not installed` });
    continue;
  }
  for (let index = 0; index < modes.length; index += 1) {
    try {
      results.push(await runOne(name, modes[index], 19480 + index));
    } catch (error) {
      results.push({ service: name, mode: modes[index], status: 'failed', reason: error.message });
    }
  }
}
const evidence = results.filter((result) => result.status === 'passed').map((result) => ({
  service: result.service, mode: result.mode, sha256: result.evidenceSha256,
}));
const fingerprint = createHash('sha256').update(JSON.stringify(evidence)).digest('hex');
const report = { schemaVersion: 1, host, fingerprint, results };
await writeFile(join(artifacts, host, 'results.json'), `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify(report, null, 2));
// A requested runtime is part of the gate, not an optional best-effort probe.
// Callers that intentionally want a smaller matrix must say so with
// REPROIT_SERVICES; every service in that explicit set then has to run.
if (results.some((result) => result.status !== 'passed')) process.exitCode = 1;
