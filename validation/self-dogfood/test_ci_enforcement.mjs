import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import test from 'node:test';

const execFileAsync = promisify(execFile);
const verifier = new URL('./ci-enforcement.mjs', import.meta.url);

async function fixture(workflow, runner = null, gate = null) {
  const root = await mkdtemp(join(tmpdir(), 'reproit-ci-enforcement-'));
  await mkdir(join(root, '.github/workflows'), { recursive: true });
  await writeFile(join(root, '.github/workflows/ci.yml'), workflow);
  if (runner !== null) {
    await mkdir(join(root, 'validation/self-dogfood'), { recursive: true });
    await writeFile(
      join(root, 'validation/self-dogfood/run-required-guards.py'),
      runner,
    );
  }
  if (gate !== null) {
    await mkdir(join(root, '.reproit'), { recursive: true });
    await writeFile(join(root, '.reproit/reproit.yaml'), gate);
  }
  return root;
}

async function run(check, root) {
  try {
    const result = await execFileAsync('node', [verifier.pathname, check], {
      env: { ...process.env, REPROIT_DOGFOOD_SUBJECT_ROOT: root },
      timeout: 10_000,
    });
    return { code: 0, stdout: result.stdout };
  } catch (error) {
    return { code: error.code, stdout: error.stdout };
  }
}

test('required corpus dispatch distinguishes affected and fixed workflows', async () => {
  const affected = await fixture(`
      - name: Replay the complete required self-dogfood guard corpus
        run: target/debug/reproit --json --yes check --strict --runs 3
      - name: Test the self-dogfood validation scripts
  `);
  const fixed = await fixture(
    `
      - name: Replay the complete required self-dogfood guard corpus
        run: python3 validation/self-dogfood/run-required-guards.py target/debug/reproit
      - name: Test the self-dogfood validation scripts
  `,
    `
if status == "required":
command = ["check", guard, "--strict"]
  `,
    'gate:\n  runs: 3\n',
  );

  assert.equal((await run('required-guard-corpus-dispatch', affected)).code, 17);
  assert.equal((await run('required-guard-corpus-dispatch', fixed)).code, 0);
});

test('direct push policy distinguishes affected and fixed workflows', async () => {
  const affected = await fixture(`
  dogfood-policy:
    if: github.event_name == 'pull_request'
  windows-build:
  `);
  const fixed = await fixture(`
  dogfood-policy:
    if: github.event_name == 'pull_request' || github.event_name == 'push'
    env:
      POLICY_BASE: \${{ github.event.before }}
      POLICY_HEAD: \${{ github.event.after }}
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0
  windows-build:
  `);

  assert.equal((await run('direct-push-dogfood-policy', affected)).code, 17);
  assert.equal((await run('direct-push-dogfood-policy', fixed)).code, 0);
});
