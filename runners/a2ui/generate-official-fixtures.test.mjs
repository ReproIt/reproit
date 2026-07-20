import assert from 'node:assert/strict';
import { mkdtemp, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { generateFixtures } from './generate-official-fixtures.mjs';

const checkout = process.env.A2UI_CHECKOUT;

test(
  'official fixture generation is deterministic and schema-valid',
  { skip: !checkout },
  async () => {
    const one = await mkdtemp(join(tmpdir(), 'a2ui-fixtures-one-'));
    const two = await mkdtemp(join(tmpdir(), 'a2ui-fixtures-two-'));
    const first = await generateFixtures(checkout, one);
    const second = await generateFixtures(checkout, two);
    assert.deepEqual(second, first);
    assert.equal(first.counts.sourceExamples, 43);
    assert.equal(first.counts.streams, 86);
    assert.equal(first.componentTypes.Text > 0, true);
    assert.equal(
      await readFile(join(one, 'manifest.json'), 'utf8'),
      await readFile(join(two, 'manifest.json'), 'utf8'),
    );
  },
);
