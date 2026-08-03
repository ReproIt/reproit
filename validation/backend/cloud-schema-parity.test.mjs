import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import test from 'node:test';

const cloud = resolve(import.meta.dirname, '../../../reproit-cloud');
const routerPath = resolve(cloud, 'src/router.rs');

test(
  'local Cloud dogfood schema stays tied to real routes and response keys',
  { skip: !existsSync(routerPath) },
  () => {
    const schema = JSON.parse(
      readFileSync(resolve(cloud, 'contracts/backend-openapi.json'), 'utf8'),
    );
    const router = readFileSync(routerPath, 'utf8');
    const registry = readFileSync(resolve(cloud, 'src/backend_contract.rs'), 'utf8');
    const routes = [
      ['post', '/auth/link', 'SIGN_IN'],
      ['post', '/account/projects', 'CREATE_PROJECT'],
      ['post', '/v1/events', 'INGEST_EVENTS'],
      ['post', '/v1/capture-batches', 'INGEST_CAPTURE_BATCHES'],
      ['get', '/v1/me', 'GET_ME'],
      ['get', '/v1/occurrences/{occurrence}', 'GET_OCCURRENCE'],
      ['post', '/v1/apps/{app}/buckets/{bucket}/replay-results', 'RECORD_REPLAY'],
    ];
    for (const [method, route, constant] of routes) {
      assert.ok(
        schema.paths[route]?.[method],
        `${method} ${route} missing from Cloud artifact`,
      );
      const escaped = route.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      assert.match(registry, new RegExp(`router_path:\\s*"${escaped}"`));
      assert.match(
        router,
        new RegExp(`\\.route\\(\\s*backend_contract::${constant}\\s*,\\s*${method}\\(`, 's'),
      );
    }

    const sourceChecks = [
      ['src/auth/mod.rs', ['"ok"', '"expiresInMinutes"', '"mailed"']],
      ['src/auth/projects.rs', ['"appId"', '"apiKeyPrefix"', '"publishableKeyPrefix"']],
      ['src/ingest/mod.rs', ['"ingested"', '"deduped"', '"orgId"', '"projects"']],
      ['src/ingest/capture_batch.rs', ['"occurrenceId"', '"bucketId"', '"capture"']],
      ['src/ingest/replay.rs', ['"localReproId"']],
    ];
    for (const [relative, tokens] of sourceChecks) {
      const source = readFileSync(resolve(cloud, relative), 'utf8');
      for (const token of tokens) assert.ok(source.includes(token), `${relative} lost ${token}`);
    }
  },
);
