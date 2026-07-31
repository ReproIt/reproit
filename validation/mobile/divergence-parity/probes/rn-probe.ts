// Drive the real React Native causal fetch wrapper into an unmatched call.
//
// The capsule and the live call both come from the shared vectors
// (`sdk/capture-behavior-v1.json`, vocabularies.divergenceMarkers.parityScenario)
// so all three mobile platforms are asked the same question. The SDK writes
// `REPROIT:DIVERGENCE` to stderr itself; this probe only reports the frozen
// runner contract, which reaches the caller as a thrown message rather than a
// stream, on stdout.

import { installCausalFetch } from '../../../../sdk/reproit-react-native/src/causal';

const capsule = JSON.parse(process.env.REPROIT_CAPSULE_JSON as string);

// The wrapper needs a global fetch to wrap; it never reaches it under a capsule.
(globalThis as { fetch?: unknown }).fetch = async () => {
  throw new Error('the probe must never reach the network');
};

installCausalFetch({
  actionIndex: () => 0,
  capsule,
  emit: () => {},
});

void (async () => {
  try {
    await fetch(process.env.PROBE_URL as string);
    process.stdout.write('PROBE:NOMISS\n');
  } catch (error) {
    process.stdout.write(`PROBE:MISS ${(error as Error).message}\n`);
  }
})();
