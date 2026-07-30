// Host-side scenario/fuzz plumbing shared by the Electron and Tauri runners:
// the fuzz-config loader, the deterministic seeded RNG, the injected-value
// provenance ledger, and journey env interpolation. Host-pure and
// dependency-free, like shared/signature.mjs.
import { readFileSync } from 'node:fs';

function loadFuzz() {
  const p = process.env.REPROIT_FUZZ_CONFIG;
  if (!p) return {};
  try {
    return JSON.parse(readFileSync(p, 'utf8'));
  } catch {
    return {};
  }
}

// xorshift32: deterministic across replays and across runners for a given seed.
function rng(seed) {
  let s = seed >>> 0 || 1;
  return (n) => {
    s ^= s << 13;
    s >>>= 0;
    s ^= s >> 17;
    s ^= s << 5;
    s >>>= 0;
    return (s & 0x7fffffff) % n;
  };
}

// Provenance ledger for the broken-asset oracle: every value the fuzzer types
// is recorded so brokenAssetScan can exclude an asset (or tofu) that exists
// only because a fuzzer-injected value was reflected into the DOM, not the
// app's own rendered content. Session-wide (mirrors the web runner).
const INJECTED_VALUES = new Set();

// Substitute ${VAR} from the environment (same contract as the web runner):
// journeys encode `secret:` fills as ${REPROIT_SECRET_<ACCT>_<FIELD>}
// placeholders so plaintext credentials never touch disk. Unset vars expand
// to "" (a missing credential types blank, which the app rejects).
function expandEnv(s) {
  return String(s).replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (_, name) => process.env[name] || '');
}

export { loadFuzz, rng, INJECTED_VALUES, expandEnv };
