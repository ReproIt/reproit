// ReproIt RN runner: drives a React Native app over an Appium session and emits
// the SAME marker protocol as the Flutter and web runners, so the whole map /
// graph / fuzz / soak / a11y / evidence pipeline works on RN unchanged. Appium's
// accessibility source (the page-source XML) is to RN what the semantics tree is
// to Flutter and the a11y tree is to web.
//
// The state signature is the CANONICAL STRUCTURAL signature (docs/signature.md):
// we walk Appium's page-source XML into a canonical Node tree (role from native
// a11y traits + class -> the fixed vocabulary; id from resource-id / testID /
// accessibility-id; type for inputs; icon if available; children) and hash the
// normalized descriptor with FNV-1a. It is byte-identical to the Rust oracle
// (crates/reproit/src/model/signature.rs), the web/RN SDKs, and the golden
// vectors (signature_vectors.json). Localized text NEVER enters the hash; it is
// kept only as display-only labels + an elements list with structural selectors.
//
// Records (one JSON per line, parsed from stdout):
//   EXPLORE:STATE      {"sig":..,"labels":[..],"elements":[{sel,role,label,nokey?}]}
//   EXPLORE:EDGE       {"from":..,"action":"tap:<selector>"|"back","to":..}
//                      selector = "key:<id>" or "role:<role>#<idx>", never text.
//   EXPLORE:CONTENTBUG {"sig":..,"items":[{key,reason,text}]} per-state, label scan
//   EXPLORE:BLANKSCREEN {"sig":..,"items":[{key:"root",w,h}]} per-state, WSOD
//   EXPLORE:SAFEAREA   {"sig":..,"items":[{key,edge,by}]}  per-state, inset geometry
//   EXPLORE:PERMISSIONWALK {"sig":..,"permission":..}  per-state, denial sweep
//   EXPLORE:BROKENASSET {"sig":..,"items":[{key,reason:"tofu",detail}]} per-state
//   EXPLORE:HANG       {"from":..,"action":..,"bucket":..} per-transition watchdog
//   EXPLORE:JANK       {"from":..,"action":..,"bucket":..,"count":..} Android gfxinfo
//   EXPLORE:WAKELOCK   {"sig":..,"items":[{tag,kind}]} Android dumpsys-power leak
//   MEMORY:SAMPLE      {"t_ms":..,"heap_used":..}  Android PSS series under --soak
// The OVERFLOW/CONTENTBUG/HANG/JANK/MEMORY markers share the EXACT contract the
// web runner emits and the Rust core already parses (model/map.rs, modes/soak.rs);
// the core is unchanged. iOS LEAK is now covered COARSELY (session-level process
// RSS sampled per replay cycle: the booted-sim app is a host process whose pid the
// runner resolves over `simctl spawn booted launchctl list`, read with host `ps`);
// see sampleIosHeap. iOS JANK stays a documented gap (no clean, non-flaky,
// sim-attributable per-frame trace exists for a simulator app: Animation Hitches
// is unsupported on the sim, Metal System Trace captures host-wide GPU work not
// the sim app, and xctrace cannot attach to an in-sim process); the exact commands
// tried and why each fails are recorded in the HANG/JANK/LEAK section.
//
// Env (set by the orchestrator's react-native runner):
//   REPROIT_APPIUM_URL    Appium server base URL (e.g. http://127.0.0.1:4723)
//   REPROIT_APPIUM_CAPS   JSON capabilities (platformName, app, deviceName, ...)
//   REPROIT_FUZZ_CONFIG   seed/budget/replay/prefix json
//
// stdout is the marker stream; the orchestrator captures it like a drive log.
//
// Runtime validation: validation/backends/run-react-native-android.sh builds a
// bundled React Native release app, drives it through this runner on Appium,
// and requires a keyed press, structural state change, and EXPLORE:EDGE. Native
// SwiftUI/iOS and Compose/Android fixtures gate the other Appium platform ids.

// webdriverio is loaded LAZILY at the one call site that starts a session.
// A top-level import makes the whole module unloadable without the Appium
// driver installed, including the host-pure signatureOf/descriptorOf exports
// that runners/signature_test.mjs imports; that broke the parity gate, whose
// job installs no npm packages at all.
import { readFileSync, writeFileSync, existsSync, mkdirSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { execFileSync, spawn } from 'node:child_process';
import { inspectPlatformStep } from './inspect-control.mjs';

const APPIUM = process.env.REPROIT_APPIUM_URL || 'http://127.0.0.1:4723';
const CAPS = JSON.parse(process.env.REPROIT_APPIUM_CAPS || '{}');
const ACTION_BUDGET = 36;
const MAX_LABEL_LEN = 40;
// Layer-1 value-class cap (docs/signature.md "Value-state"): once a structural
// node has shown more than this many DISTINCT value-class combinations, the
// runner drops it to structural-only so an adversarial value generator cannot
// explode the graph. The oracle is stateless; the cap is purely runner-local.
const VALUE_CLASS_CAP = 8;
let causalActionIndex = 0;

async function advanceCausalAction(driver) {
  causalActionIndex += 1;
  if (isAndroid()) {
    await mobileShell(driver, 'setprop', ['debug.reproit.action', String(causalActionIndex)]);
  }
}

function log(line) {
  process.stdout.write(line + '\n');
}
function stageAndroidCausalBeforeLaunch(caps) {
  if (!isAndroid()) return true;
  const serial = caps['appium:udid'] || caps.udid;
  const adb = (args) =>
    execFileSync('adb', [...(serial ? ['-s', String(serial)] : []), ...args], { stdio: 'ignore' });
  try {
    adb(['shell', 'setprop', 'debug.reproit.fuzz', '1']);
    adb(['shell', 'setprop', 'debug.reproit.action', '0']);
    if (process.env.REPROIT_CAPSULE) {
      const destination = '/data/local/tmp/reproit-capsule.json';
      adb(['push', process.env.REPROIT_CAPSULE, destination]);
      adb(['shell', 'chmod', '0644', destination]);
      adb(['shell', 'setprop', 'debug.reproit.capsule', destination]);
    } else {
      // adb/Appium drop empty shell arguments. The SDK treats this explicit,
      // shell-safe sentinel as no capsule, preventing stale replay state.
      adb(['shell', 'setprop', 'debug.reproit.capsule', '__reproit_none__']);
    }
    return true;
  } catch (_) {
    // The post-session mobile:shell path below is the fallback for remote device
    // farms where adb is intentionally unavailable on the runner host.
    return false;
  }
}
function loadFuzz() {
  const p = process.env.REPROIT_FUZZ_CONFIG;
  if (!p) return {};
  try {
    return JSON.parse(readFileSync(p, 'utf8'));
  } catch {
    return {};
  }
}

// The multi-seed BATCH contract shared with every other runner (runners/web
// loadBatch, the Flutter scaffold's FuzzCfg.loadBatch, runners/linux-
// atspi.py load_batch). `reproit check` with gate.runs > 1 (and the multi-seed
// fuzz path) writes {"batch":[ <cfg>, ... ]} where each <cfg> is the single-seed
// shape ({seed, replay?, prefix?, ...}); a single run writes the bare <cfg>
// directly (no "batch" key). Returns { seeds, isBatch }; isBatch is true ONLY for
// the multi-seed shape, so the caller wraps each seed in SEED:BEGIN/SEED:END only
// then and the Rust core (fuzz.rs split_log_segments) can split the one drive log
// back into one segment per replay. WITHOUT this the runner read the {batch:..}
// object as a single fuzz config whose `replay`/`seed` were undefined, silently
// fell into a fresh EXPLORE walk, and never replayed the stored actions. As a result, a
// real crash repro re-confirmed as clean (PASS). See the replay branch in main().
function loadBatch() {
  const j = loadFuzz();
  if (j && Array.isArray(j.batch) && j.batch.length) {
    return { seeds: j.batch.map((b) => (b && typeof b === 'object' ? b : {})), isBatch: true };
  }
  return { seeds: [j || {}], isBatch: false };
}

const FUZZ_CONFIGURED = !!process.env.REPROIT_FUZZ_CONFIG;

function edgeKey(sig, action) {
  return sig + '|' + action;
}
function rememberActions(actionsByState, sig, actions) {
  const known = actionsByState.get(sig) || [];
  for (const action of actions) if (!known.includes(action)) known.push(action);
  actionsByState.set(sig, known);
}
function firstUntriedAction(actionsByState, tried, sig) {
  for (const action of actionsByState.get(sig) || []) {
    if (!tried.has(edgeKey(sig, action))) return action;
  }
  return null;
}
function hasFrontier(actionsByState, tried) {
  for (const sig of actionsByState.keys())
    if (firstUntriedAction(actionsByState, tried, sig)) return true;
  return false;
}
function rememberEdge(graph, from, action, to) {
  const edges = graph.get(from) || [];
  if (!edges.some((e) => e.action === action && e.to === to)) edges.push({ action, to });
  graph.set(from, edges);
}
function pathToFrontier(graph, actionsByState, tried, start) {
  if (firstUntriedAction(actionsByState, tried, start)) return [];
  const seen = new Set([start]);
  const q = [{ sig: start, path: [] }];
  for (let i = 0; i < q.length; i++) {
    const { sig, path } = q[i];
    for (const { action, to } of graph.get(sig) || []) {
      if (seen.has(to)) continue;
      seen.add(to);
      const nextPath = path.concat(action);
      if (firstUntriedAction(actionsByState, tried, to)) return nextPath;
      q.push({ sig: to, path: nextPath });
    }
  }
  return null;
}

// Layer-3 opt-in (docs/signature.md "Value-state"): read `value_nodes:`
// selectors from reproit.yaml. We avoid adding a YAML dependency: the block is
// a simple flat list of strings, so a tiny line parser is enough and keeps the
// runner dependency-free. Path precedence: REPROIT_CONFIG env, else
// ./reproit.yaml in the cwd. A missing/unparseable file yields an empty list,
// so value-state is strictly opt-in. Mirrors runners/web.
// The canonical signature model and the seeded rng are SHARED, not copied.
// This runner carried a byte-identical duplicate of shared/signature.mjs and
// of rng from shared/fuzz.mjs, so a drift in one copy could not be caught by
// the parity gate, which only covered electron and tauri until this change.
// esbuild inlines these for the bundled rn target, so the shipped runner stays
// self contained.
import {
  signatureOf,
  descriptorOf,
  valueClass,
  fnv1a,
  loadValueNodes,
} from '../shared/signature.mjs';
import { rng } from '../shared/fuzz.mjs';

export { signatureOf, descriptorOf, valueClass };

// ====================================================================
//  Appium page-source XML -> canonical Node tree
//  Appium exposes nested elements with platform-specific tags + attributes:
//    iOS (XCUITest): <XCUIElementTypeButton name=".." label=".." value=".."
//                     enabled=".." visible=".." accessible=".."/>
//    Android (UiA2): <android.widget.Button text=".." content-desc=".."
//                     resource-id=".." class=".." clickable=".."/>
//  We map each element to a canonical role from its tag/class + a11y traits
//  (NEVER from visible text), pull a stable id, refine input types, and recurse.
// ====================================================================

// iOS XCUIElementType* tag -> canonical role.
