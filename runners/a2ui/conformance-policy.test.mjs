import assert from 'node:assert/strict';
import {execFile} from 'node:child_process';
import {mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {promisify} from 'node:util';
import test from 'node:test';

const execute = promisify(execFile);
const policy = new URL('./conformance-policy.mjs', import.meta.url);

async function runPolicy({knownIssues, issueCommit = 'abc', rendererCommit = 'abc', newFindings = []}) {
  const directory = await mkdtemp(join(tmpdir(), 'reproit-a2ui-policy-'));
  const rendererPath = join(directory, 'renderer.json');
  const issuePath = join(directory, 'issues.json');
  const outputPath = join(directory, 'report.json');
  await writeFile(rendererPath, JSON.stringify({
    upstream: {commit: rendererCommit},
    corpus: {}, evidenceSha256: 'evidence', findings: [], metamorphicFindings: [],
  }));
  await writeFile(issuePath, JSON.stringify({
    upstream: {commit: issueCommit}, knownIssues, newFindings,
  }));
  try {
    const execution = await execute(process.execPath, [policy.pathname, rendererPath, issuePath, outputPath]);
    return {execution, report: JSON.parse(await readFile(outputPath, 'utf8'))};
  } finally {
    await rm(directory, {recursive: true, force: true});
  }
}

const required = [1298, 1410].map(issue => ({issue, status: 'reproduced'}));

test('policy requires both pinned known-issue reproductions from the same commit', async () => {
  const {report} = await runPolicy({knownIssues: required});
  assert.equal(report.knownIssueBackedFindings.length, 2);
  await assert.rejects(runPolicy({knownIssues: required.slice(1)}), /required pinned A2UI #1298 reproduction is missing/);
  await assert.rejects(runPolicy({knownIssues: required, issueCommit: 'different'}), /same upstream commit/);
});

test('policy fails when a minimized new finding is present', async () => {
  await assert.rejects(
    runPolicy({knownIssues: required, newFindings: [{id: 'new-renderer-defect'}]}),
    error => error.code === 1,
  );
});
