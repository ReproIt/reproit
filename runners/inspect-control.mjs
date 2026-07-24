// Platform-neutral host control for a visible replay inspection.
//
// The target stays in its real platform surface. Before each replayed action,
// the runner publishes one bounded request and waits for the parent ReproIt
// process to answer step, continue, or abort.

import { mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const CONTROL_DIR = (process.env.REPROIT_INSPECT_CONTROL || '').trim();
const WAIT_MS = boundedWaitMs(process.env.REPROIT_INSPECT_WAIT_MS);

export function boundedWaitMs(raw) {
  const parsed = Number.parseInt(String(raw || ''), 10);
  if (!Number.isFinite(parsed)) return 240_000;
  return Math.min(900_000, Math.max(1_000, parsed));
}

export async function inspectPlatformStep({
  action,
  step,
  total,
  target = null,
  state = null,
}) {
  if (!CONTROL_DIR) return 'continue';
  await mkdir(CONTROL_DIR, { recursive: true });
  const requestPath = join(CONTROL_DIR, 'request.json');
  const responsePath = join(CONTROL_DIR, 'response.json');
  const sequence = step;
  const request = JSON.stringify({ sequence, step, total, action, target, state });
  const temp = join(CONTROL_DIR, `request-${process.pid}.tmp`);
  await writeFile(temp, request, { encoding: 'utf8', mode: 0o600 });
  await rm(requestPath, { force: true });
  await rename(temp, requestPath);

  const deadline = Date.now() + WAIT_MS;
  while (Date.now() < deadline) {
    try {
      const response = JSON.parse(await readFile(responsePath, 'utf8'));
      if (response.sequence === sequence) {
        if (response.decision === 'abort') throw new Error('inspection stopped by user');
        return response.decision === 'continue' ? 'continue' : 'step';
      }
    } catch (error) {
      if (String(error?.message || error).includes('inspection stopped')) throw error;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`inspection timed out while waiting at step ${step}`);
}
