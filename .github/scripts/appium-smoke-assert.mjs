// Assertion pass over the marker log a smoke run of runners/rn/runner.mjs
// produced (see appium-android-smoke.sh). The runner exits 0 even when its
// session throws (the exception marker is the machine-readable signal), so
// these assertions are what actually gate the CI job.
//
// Asserted, in order of the runner's lifecycle:
//   1. the Appium session was created (JOURNEY claimed role=a),
//   2. no runner-level exception marker was emitted,
//   3. at least one EXPLORE:STATE with a well-formed 8-hex canonical signature
//      and a non-empty structural elements list was captured,
//   4. at least one tap was attempted and at least one attempted tap actually
//      resolved + clicked (taps attempted > taps missed),
//   5. at least one EXPLORE:EDGE was recorded: a tap provably moved the app to
//      a different structural state,
//   6. the walk finished cleanly (JOURNEY DONE + "All tests passed", which also
//      means the crash oracle saw the target app stay in the foreground).
import { readFileSync } from 'node:fs';

const path = process.argv[2];
if (!path) {
  console.error('usage: node appium-smoke-assert.mjs <runner-log>');
  process.exit(2);
}
const text = readFileSync(path, 'utf8');
const lines = text.split(/\r?\n/);

const failures = [];
const ok = (cond, what) => {
  if (cond) console.log('smoke ok: ' + what);
  else failures.push(what);
};

ok(lines.some((l) => l.includes('JOURNEY claimed role=a')),
  'Appium session created (JOURNEY claimed)');

ok(!text.includes('EXCEPTION CAUGHT BY RN RUNNER'),
  'no runner exception marker');

const states = lines
  .filter((l) => l.startsWith('EXPLORE:STATE '))
  .map((l) => { try { return JSON.parse(l.slice('EXPLORE:STATE '.length)); } catch { return null; } })
  .filter((s) => s != null);
ok(states.length >= 1, 'at least one EXPLORE:STATE captured');
ok(states.some((s) => typeof s.sig === 'string' && /^[0-9a-f]{8}$/.test(s.sig)),
  'a state carries a well-formed 8-hex canonical signature');
ok(states.some((s) => Array.isArray(s.elements) && s.elements.length > 0),
  'a state carries a non-empty structural elements list');

const taps = lines.filter((l) => l.startsWith('FUZZ:ACT tap:')).length;
const misses = lines.filter((l) => l.startsWith('FUZZ:MISS ')).length;
ok(taps >= 1, 'at least one tap attempted (' + taps + ' attempted)');
ok(taps > misses,
  'at least one tap resolved and clicked (' + taps + ' attempted, ' + misses + ' missed)');

ok(lines.some((l) => l.startsWith('EXPLORE:EDGE ')),
  'a tap changed app state (EXPLORE:EDGE recorded)');

ok(lines.some((l) => l.includes('JOURNEY DONE')), 'walk finished (JOURNEY DONE)');
ok(text.includes('All tests passed'),
  'clean finish, crash oracle silent ("All tests passed")');

if (failures.length > 0) {
  for (const f of failures) console.error('smoke FAILED: ' + f);
  process.exit(1);
}
console.log('appium smoke: all assertions passed');
