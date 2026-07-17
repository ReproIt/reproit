import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import test from 'node:test';

const cloud = resolve(import.meta.dirname, '../../../reproit-cloud');
const mainPath = resolve(cloud, 'src/main.rs');

test(
  'local Cloud dogfood schema stays tied to real routes and response keys',
  { skip: !existsSync(mainPath) },
  () => {
    const schema = JSON.parse(
      readFileSync(resolve(cloud, 'contracts/backend-openapi.json'), 'utf8'),
    );
    const main = readFileSync(mainPath, 'utf8');
    const registry = readFileSync(resolve(cloud, 'src/backend_contract.rs'), 'utf8');
    const routes = [
      ['post', '/auth/signup', 'SIGNUP'],
      ['post', '/account/projects', 'CREATE_PROJECT'],
      ['post', '/v1/events', 'INGEST_EVENTS'],
      ['get', '/v1/me', 'GET_ME'],
      ['post', '/v1/apps/:app/buckets/:bucket/replay-results', 'RECORD_REPLAY'],
    ];
    for (const [method, route, constant] of routes) {
      const openapiRoute = route.replaceAll(':app', '{app}').replaceAll(':bucket', '{bucket}');
      assert.ok(
        schema.paths[openapiRoute]?.[method],
        `${method} ${openapiRoute} missing from Cloud artifact`,
      );
      const escaped = route.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      assert.match(registry, new RegExp(`router_path:\\s*"${escaped}"`));
      assert.match(
        main,
        new RegExp(`\\.route\\(\\s*backend_contract::${constant}\\s*,\\s*${method}\\(`, 's'),
      );
    }

    const sourceChecks = [
      ['src/auth/mod.rs', ['"email"', '"appId"', '"apiKeyPrefix"', '"publishableKeyPrefix"']],
      ['src/ingest/mod.rs', ['"ingested"', '"deduped"', '"orgId"', '"projects"', '"localReproId"']],
    ];
    for (const [relative, tokens] of sourceChecks) {
      const source = readFileSync(resolve(cloud, relative), 'utf8');
      for (const token of tokens) assert.ok(source.includes(token), `${relative} lost ${token}`);
    }
  },
);
