#!/usr/bin/env node
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { capture, replay, sha256 } from './adapter.mjs';

export const PIN = '96abfdc60de0657c6322028d10c1cc7bc25c237c';

export function messagesFrom(document) {
  return Array.isArray(document) ? document : document.messages;
}

export function compactDocument(document) {
  const messages = messagesFrom(document);
  const create = messages.find((message) => message.createSurface);
  assert(create, 'stream has no createSurface');
  const byId = new Map();
  for (const message of messages) {
    for (const component of message.updateComponents?.components || [])
      byId.set(component.id, structuredClone(component));
  }
  const surfaceId = create.createSurface.surfaceId;
  const compacted = [structuredClone(create)];
  compacted.push(
    ...messages
      .filter((message) => message.updateDataModel)
      .map((message) => structuredClone(message)),
  );
  if (byId.size)
    compacted.push({
      version: 'v0.9',
      updateComponents: { surfaceId, components: [...byId.values()] },
    });
  return { ...(Array.isArray(document) ? {} : structuredClone(document)), messages: compacted };
}

export async function validateMessageDirectory(checkout, messagesDirectory, catalogDocument) {
  const alias = structuredClone(catalogDocument);
  alias.$id = 'https://a2ui.org/specification/v0_9/catalog.json';
  const aliasPath = join(dirname(messagesDirectory), 'catalog.json');
  await writeFile(aliasPath, JSON.stringify(alias));
  const schemaDir = join(checkout, 'specification/v0_9_1/json');
  execFileSync(
    'corepack',
    [
      'yarn',
      'run',
      'ajv',
      'validate',
      '-s',
      join(schemaDir, 'server_to_client.json'),
      '--spec=draft2020',
      '--strict=false',
      '-c',
      'ajv-formats',
      '-d',
      join(messagesDirectory, '*.json'),
      '-r',
      join(schemaDir, 'common_types.json'),
      '-r',
      aliasPath,
      '-r',
      join(schemaDir, 'client_to_server.json'),
    ],
    { cwd: join(checkout, 'specification/v0_9_1/test'), stdio: 'pipe' },
  );
}

export async function generateFixtures(checkout, output) {
  checkout = resolve(checkout);
  output = resolve(output);
  const actual = execFileSync('git', ['-C', checkout, 'rev-parse', 'HEAD'], {
    encoding: 'utf8',
  }).trim();
  const expected = process.env.A2UI_EXPECTED_COMMIT || PIN;
  assert.equal(actual, expected, `A2UI checkout must be pinned to ${expected}`);
  const examplesDir = join(checkout, 'specification/v0_9_1/catalogs/basic/examples');
  const names = (await readdir(examplesDir)).filter((name) => name.endsWith('.json')).sort();
  assert(names.length > 0, 'official example fixture set is empty');
  const protocolDocument = JSON.parse(
    await readFile(join(checkout, 'specification/v0_9_1/json/server_to_client.json'), 'utf8'),
  );
  const catalogDocument = JSON.parse(
    await readFile(join(checkout, 'specification/v0_9_1/catalogs/basic/catalog.json'), 'utf8'),
  );
  await mkdir(join(output, 'messages'), { recursive: true });
  await mkdir(join(output, 'streams'), { recursive: true });

  const entries = [];
  const componentTypes = new Map();
  let messages = 0;
  let components = 0;
  let messageIndex = 0;
  for (const name of names) {
    const official = JSON.parse(await readFile(join(examplesDir, name), 'utf8'));
    for (const variant of [
      { kind: 'official', document: official },
      { kind: 'compacted', document: compactDocument(official) },
    ]) {
      const stream = messagesFrom(variant.document);
      const capsule = capture({
        protocolVersion: 'v0.9',
        protocolDocument,
        catalog: { id: catalogDocument.catalogId, document: catalogDocument },
        stream,
        renderer: { name: 'fixture-generator', version: '1', platform: 'schema' },
        oracle: { kind: 'protocol-valid' },
      });
      const result = replay(capsule);
      assert.equal(
        result.status,
        'pass',
        `${variant.kind}/${name}: ${result.protocolErrors?.join('; ')}`,
      );
      const streamName = `${variant.kind}-${name}`;
      await writeFile(
        join(output, 'streams', streamName),
        JSON.stringify(variant.document, null, 2) + '\n',
      );
      for (const message of stream) {
        await writeFile(
          join(output, 'messages', `${String(messageIndex++).padStart(4, '0')}.json`),
          JSON.stringify(message) + '\n',
        );
        for (const component of message.updateComponents?.components || []) {
          componentTypes.set(
            component.component,
            (componentTypes.get(component.component) || 0) + 1,
          );
          components++;
        }
      }
      messages += stream.length;
      entries.push({
        kind: variant.kind,
        source: name,
        messages: stream.length,
        sha256: sha256(stream),
      });
    }
  }

  await validateMessageDirectory(checkout, join(output, 'messages'), catalogDocument);

  const manifest = {
    format: 'a2ui-official-conformance-fixtures',
    version: 1,
    upstream: { repository: 'https://github.com/a2ui-project/a2ui', commit: actual },
    counts: { sourceExamples: names.length, streams: entries.length, messages, components },
    componentTypes: Object.fromEntries([...componentTypes].sort()),
    entries,
  };
  manifest.sha256 = sha256(manifest);
  await writeFile(join(output, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
  return manifest;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const checkout = process.argv[2];
  const output = process.argv[3] || (await mkdtemp(join(tmpdir(), 'a2ui-fixtures-')));
  if (!checkout)
    throw new Error('usage: generate-official-fixtures.mjs <pinned-a2ui-checkout> [output]');
  console.log(JSON.stringify(await generateFixtures(checkout, output), null, 2));
}
