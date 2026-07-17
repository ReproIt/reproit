#!/usr/bin/env node
import { readFile, writeFile } from 'node:fs/promises';

const [rendererPath, issuePath, outputPath] = process.argv.slice(2);
if (!rendererPath || !issuePath || !outputPath) {
  throw new Error('usage: conformance-policy.mjs <renderer-report> <issue-report> ' + '<output>');
}

const renderer = JSON.parse(await readFile(rendererPath, 'utf8'));
const issues = JSON.parse(await readFile(issuePath, 'utf8'));
if (issues.upstream?.commit !== renderer.upstream?.commit) {
  throw new Error(
    'renderer and known-issue reports were not captured from the same ' + 'upstream commit',
  );
}
for (const issue of [1298, 1410]) {
  if (
    !issues.knownIssues.some(
      (finding) => finding.issue === issue && finding.status === 'reproduced',
    )
  ) {
    throw new Error(`required pinned A2UI #${issue} reproduction is missing`);
  }
}
const knownIssueBackedFindings = issues.knownIssues.filter(
  (finding) => finding.status === 'reproduced',
);
const unexpectedFindings = [
  ...renderer.findings.map((finding) => ({ kind: 'renderer-divergence', ...finding })),
  ...renderer.metamorphicFindings.map((finding) => ({ kind: 'stream-equivalence', ...finding })),
  ...issues.newFindings.map((finding) => ({ kind: 'minimized-new-finding', ...finding })),
];
const report = {
  upstream: renderer.upstream,
  corpus: renderer.corpus,
  evidenceSha256: renderer.evidenceSha256,
  policy: {
    knownIssueBackedFindingsDoNotFail: true,
    unexpectedFindingsFail: true,
    pass: unexpectedFindings.length === 0,
  },
  knownIssueBackedFindings,
  unexpectedFindings,
};
await writeFile(outputPath, JSON.stringify(report, null, 2) + '\n');
console.log(
  JSON.stringify({
    pass: report.policy.pass,
    knownIssueBackedFindings: knownIssueBackedFindings.length,
    unexpectedFindings: unexpectedFindings.length,
    output: outputPath,
  }),
);
if (!report.policy.pass) process.exitCode = 1;
