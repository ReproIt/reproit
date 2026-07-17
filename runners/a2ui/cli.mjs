#!/usr/bin/env node
import { readFile, writeFile } from 'node:fs/promises';
import { capture, parseJsonl, rendererMatrix, replay, shrink } from './adapter.mjs';

function value(args, name) {
  const index = args.indexOf(name);
  if (index < 0 || !args[index + 1]) throw new Error(`missing ${name}`);
  return args[index + 1];
}

async function json(path) {
  return JSON.parse(await readFile(path, 'utf8'));
}

async function main() {
  const [command, ...args] = process.argv.slice(2);
  if (command === 'capture') {
    const stream = parseJsonl(await readFile(value(args, '--stream'), 'utf8'));
    const protocolDocument = await json(value(args, '--protocol-schema'));
    const catalogDocument = await json(value(args, '--catalog'));
    const oracle = await json(value(args, '--oracle'));
    const renderer = await json(value(args, '--renderer'));
    const snapshots = args.includes('--snapshots')
      ? parseJsonl(await readFile(value(args, '--snapshots'), 'utf8'))
      : [];
    const actions = args.includes('--actions')
      ? parseJsonl(await readFile(value(args, '--actions'), 'utf8'))
      : [];
    const capsule = capture({
      protocolVersion: value(args, '--protocol'),
      protocolDocument,
      catalog: { id: value(args, '--catalog-id'), document: catalogDocument },
      stream,
      renderer,
      clientDataSnapshots: snapshots,
      actions,
      oracle,
    });
    await writeFile(value(args, '--out'), JSON.stringify(capsule, null, 2) + '\n');
    return;
  }
  if (command === 'replay') {
    console.log(JSON.stringify(replay(await json(args[0])), null, 2));
    return;
  }
  if (command === 'shrink') {
    const result = shrink(await json(args[0]));
    await writeFile(value(args, '--out'), JSON.stringify(result.capsule, null, 2) + '\n');
    console.log(JSON.stringify({ ...result, capsule: undefined }, null, 2));
    return;
  }
  if (command === 'matrix') {
    console.log(JSON.stringify(rendererMatrix(await Promise.all(args.map(json))), null, 2));
    return;
  }
  throw new Error('usage: cli.mjs capture|replay|shrink|matrix ...');
}

main().catch((error) => {
  console.error(`reproit-a2ui: ${error.message}`);
  process.exitCode = 1;
});
