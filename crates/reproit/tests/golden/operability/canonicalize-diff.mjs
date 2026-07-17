#!/usr/bin/env node
// Marker-drift guard: canonicalize + diff a LIVE operability EXPLORE:GROUNDTRUTH
// marker against the committed golden for one platform. The goldens live next to
// this script (tests/golden/operability/<platform>.json) and are the SINGLE
// source of truth the engine contract tests read (crates/reproit/src/model/map.rs
// gaps_from_golden). This script is what keeps that golden honest in CI: it
// re-captures the marker the REAL agent emits and fails the job (naming the
// platform + the changed field) the moment the live marker drifts from the
// golden, so a stale golden / drifted agent can never pass silently.
//
// CANONICALIZATION (both sides, before compare):
//   - JSON-parse,
//   - recursively SORT object keys (so field-order churn is not a diff),
//   - DROP the volatile top-level `sig` (a structural hash that legitimately
//     changes across toolchain/layout versions; it is not part of the a11y
//     contract the gap classifications assert).
// Anything else that changes -- an element added/removed, an a11y dim flipped,
// gestureKind renamed -- IS a real contract drift and fails the diff.
//
// USAGE:
//   node canonicalize-diff.mjs <platform> <liveMarkerFile>
//     <platform>        web | appkit | wpf | qt | gtk | flutter
//     <liveMarkerFile>  a file whose content is EITHER the captured agent stdout
//                       (the first `EXPLORE:GROUNDTRUTH {...}` line is used) OR a
//                       bare JSON payload. Pass "-" to read stdin.
//   ...| node canonicalize-diff.mjs flutter -      # live marker piped on stdin
//
// EXIT: 0 == live matches golden (drift-free). 1 == drift (diff printed). 2 ==
// usage / parse error. No deps; plain Node.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const PLATFORMS = ['web', 'appkit', 'wpf', 'qt', 'gtk', 'flutter'];

function fail(msg, code = 2) {
  console.error(`canonicalize-diff: ${msg}`);
  process.exit(code);
}

// Pull the GROUNDTRUTH payload out of a captured-stdout blob, or accept a bare
// JSON object. Returns the parsed object.
function extractGroundtruth(text, label) {
  const trimmed = text.trim();
  // Captured agent output: find the marker line.
  const line = trimmed
    .split('\n')
    .map((l) => l.trim())
    .find((l) => l.startsWith('EXPLORE:GROUNDTRUTH '));
  const raw = line ? line.slice('EXPLORE:GROUNDTRUTH '.length).trim() : trimmed;
  try {
    return JSON.parse(raw);
  } catch (e) {
    fail(`${label}: no parseable EXPLORE:GROUNDTRUTH JSON (${e.message})`);
  }
}

// Recursively sort object keys and DROP the top-level volatile `sig`, so the
// canonical form is stable across field-order and signature churn.
function canonicalize(value, isTop = false) {
  if (Array.isArray(value)) return value.map((v) => canonicalize(v, false));
  if (value && typeof value === 'object') {
    const out = {};
    for (const k of Object.keys(value).sort()) {
      if (isTop && k === 'sig') continue; // volatile: not part of the a11y contract
      out[k] = canonicalize(value[k], false);
    }
    return out;
  }
  return value;
}

// Walk two canonical trees and report the first divergent field path (dotted),
// so the failure names exactly what drifted. Returns null when identical.
function firstDiff(a, b, path = '') {
  const ta = a === null ? 'null' : Array.isArray(a) ? 'array' : typeof a;
  const tb = b === null ? 'null' : Array.isArray(b) ? 'array' : typeof b;
  if (ta !== tb) return { path: path || '(root)', golden: a, live: b };
  if (ta === 'array') {
    if (a.length !== b.length) {
      return { path: `${path}.length`, golden: a.length, live: b.length };
    }
    for (let i = 0; i < a.length; i++) {
      const d = firstDiff(a[i], b[i], `${path}[${i}]`);
      if (d) return d;
    }
    return null;
  }
  if (ta === 'object') {
    const keys = [...new Set([...Object.keys(a), ...Object.keys(b)])].sort();
    for (const k of keys) {
      if (!(k in a)) return { path: `${path}.${k}`, golden: '(absent)', live: b[k] };
      if (!(k in b)) return { path: `${path}.${k}`, golden: a[k], live: '(absent)' };
      const d = firstDiff(a[k], b[k], path ? `${path}.${k}` : k);
      if (d) return d;
    }
    return null;
  }
  if (a !== b) return { path: path || '(root)', golden: a, live: b };
  return null;
}

const [platform, liveArg] = process.argv.slice(2);
if (!platform || !liveArg) fail('usage: canonicalize-diff.mjs <platform> <liveMarkerFile|->');
if (!PLATFORMS.includes(platform)) {
  fail(`unknown platform "${platform}" (expected one of ${PLATFORMS.join(', ')})`);
}

const goldenPath = join(HERE, `${platform}.json`);
let goldenText;
try {
  goldenText = readFileSync(goldenPath, 'utf8');
} catch (e) {
  fail(`cannot read golden ${goldenPath}: ${e.message}`);
}

let liveText;
try {
  liveText = liveArg === '-' ? readFileSync(0, 'utf8') : readFileSync(liveArg, 'utf8');
} catch (e) {
  fail(`cannot read live marker ${liveArg}: ${e.message}`);
}

// APPKIT ONLY: drop non-operable static-text elements from BOTH sides before
// comparing. Whether AppKit exposes a label as its own AX static-text element
// varies by macOS version (macOS 14 runners surface one for the fixture's
// label; macOS 26 does not), so text rows cannot be contract on appkit. The
// operable elements and their gap verdicts, which ARE the contract, still
// diff exactly. gtk/web keep their non-operable rows: their toolkits expose
// them deterministically.
function normalize(platformId, gt) {
  if (platformId !== 'appkit' || !Array.isArray(gt.elements)) return gt;
  // Elements identify by `id` ("key:realButton", "role:text#0"), not a role
  // field; the OS-variant row is a non-operable role:text* id.
  return {
    ...gt,
    elements: gt.elements.filter(
      (e) => e.operable !== false || !String(e.id || '').startsWith('role:text')
    ),
  };
}

const goldenGroundtruth = extractGroundtruth(goldenText, `golden:${platform}`);
const liveGroundtruth = extractGroundtruth(liveText, `live:${platform}`);
const golden = canonicalize(normalize(platform, goldenGroundtruth), true);
const live = canonicalize(normalize(platform, liveGroundtruth), true);

const diff = firstDiff(golden, live);
if (!diff) {
  console.log(`OK [${platform}]: live EXPLORE:GROUNDTRUTH matches golden (sig dropped).`);
  process.exit(0);
}

console.error(
  `DRIFT [${platform}]: field "${diff.path}" changed -- ` +
    `golden=${JSON.stringify(diff.golden)} live=${JSON.stringify(diff.live)}`,
);
console.error(`  golden: ${goldenPath}`);
console.error(
  '  Re-capture the agent, confirm the contract still holds, then update the ' +
    'golden + the engine test assertions.',
);
process.exit(1);
