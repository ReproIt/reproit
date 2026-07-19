// Semantic accessibility-state parity for native web controls.
//
// This oracle compares two independent, authoritative channels:
//   1. live native DOM properties (`checked`, `indeterminate`, `disabled`, ...),
//   2. Chromium's computed accessibility tree.
//
// It deliberately does not compare ARIA attributes with the accessibility tree:
// the latter is derived from the former, so agreement would not prove parity
// with application state. Custom widgets therefore need a future, explicit
// application-state authority and are outside this first autonomous slice.

import { createHash } from 'node:crypto';

// Only states whose native value and computed accessibility value are the same
// semantic contract belong here. In particular, `aria-disabled="true"` on an
// otherwise enabled native control is valid when the application suppresses
// activation itself, so comparing it with `HTMLButtonElement.disabled` would
// manufacture a contradiction. Keep disabled out until an independent
// activation probe can prove that the control still operates.
const SUPPORTED_PROPERTIES = new Set(['checked', 'expanded', 'selected']);

function stateFingerprint(identity, property) {
  return `sha256:${createHash('sha256')
    .update(`${identity}\0${property}`)
    .digest('hex')
    .slice(0, 24)}`;
}

function axValue(value) {
  if (value && typeof value === 'object' && 'value' in value) return value.value;
  return value;
}

function normalizedValue(property, value) {
  const raw = axValue(value);
  if (property === 'checked') {
    if (raw === 'mixed') return 'mixed';
    if (raw === true || raw === 'true') return 'true';
    if (raw === false || raw === 'false') return 'false';
    return null;
  }
  if (raw === true || raw === 'true') return 'true';
  if (raw === false || raw === 'false') return 'false';
  return null;
}

function resultOutcome(checks) {
  if (checks.some((check) => check.outcome === 'VIOLATION')) return 'VIOLATION';
  if (checks.some((check) => check.outcome === 'ABSTAIN')) return 'ABSTAIN';
  return checks.length ? 'SATISFIED' : 'ABSTAIN';
}

// Host-pure comparison. `domControls` contains live native-property snapshots
// with a backend DOM node id; `axNodes` is Accessibility.getFullAXTree output.
export function evaluateAccessibilityStateParity(domControls, axNodes) {
  const axByBackendId = new Map();
  for (const node of Array.isArray(axNodes) ? axNodes : []) {
    if (node?.backendDOMNodeId != null && !node.ignored) axByBackendId.set(node.backendDOMNodeId, node);
  }

  const checks = [];
  for (const control of Array.isArray(domControls) ? domControls : []) {
    for (const state of control.states || []) {
      const base = {
        identity: control.identity,
        property: state.property,
        fingerprint: stateFingerprint(control.identity, state.property),
        expected: String(state.value),
      };
      if (!SUPPORTED_PROPERTIES.has(state.property)) {
        checks.push({ ...base, outcome: 'ABSTAIN', reason: 'unsupported-property' });
        continue;
      }
      // An explicit ARIA value is authored semantic intent, not an independent
      // observation. Chromium's AX tree is derived from that value and it may
      // legitimately override a native property (for example an indeterminate
      // design-system checkbox). Without a third authority we cannot decide
      // which channel is wrong.
      if (state.semanticOverride) {
        checks.push({ ...base, outcome: 'ABSTAIN', reason: 'authored-semantic-override' });
        continue;
      }
      if (!control.settled) {
        checks.push({ ...base, outcome: 'ABSTAIN', reason: 'control-not-settled' });
        continue;
      }
      if (control.backendDOMNodeId == null) {
        checks.push({ ...base, outcome: 'ABSTAIN', reason: 'missing-dom-node-identity' });
        continue;
      }
      const axNode = axByBackendId.get(control.backendDOMNodeId);
      if (!axNode) {
        checks.push({ ...base, outcome: 'ABSTAIN', reason: 'missing-accessibility-node' });
        continue;
      }
      const property = (axNode.properties || []).find((item) => item.name === state.property);
      // Chromium omits false boolean properties on some native roles. Once the
      // exact native node and a role that supports the property are present,
      // omission is the normative false value. Checked is tri-state and is
      // required, so its omission remains an evidence gap.
      const actual = property
        ? normalizedValue(state.property, property.value)
        : state.property === 'checked'
          ? null
          : 'false';
      if (actual == null) {
        checks.push({ ...base, outcome: 'ABSTAIN', reason: 'missing-accessibility-state' });
      } else if (actual !== base.expected) {
        checks.push({ ...base, actual, outcome: 'VIOLATION', reason: 'semantic-state-mismatch' });
      } else {
        checks.push({ ...base, actual, outcome: 'SATISFIED' });
      }
    }
  }
  checks.sort((a, b) =>
    `${a.identity}\0${a.property}`.localeCompare(`${b.identity}\0${b.property}`),
  );
  return {
    outcome: resultOutcome(checks),
    checks,
    items: checks.filter((check) => check.outcome === 'VIOLATION'),
  };
}

// Runs in the page. Only native controls with a globally unique explicit id are
// eligible: visible text, DOM order, and generated selectors are not identity.
export function collectNativeAccessibilityStateInPage() {
  const candidates = document.querySelectorAll(
    'input[type="checkbox"],input[type="radio"],button,input:not([type="hidden"]),' +
      'select,textarea,option,details',
  );
  const result = [];
  for (const element of candidates) {
    if (!element.id || document.querySelectorAll(`[id="${CSS.escape(element.id)}"]`).length !== 1)
      continue;
    const style = getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    if (
      !element.isConnected ||
      style.display === 'none' ||
      style.visibility === 'hidden' ||
      element.closest('[hidden],[inert],[aria-hidden="true"]') ||
      !(rect.width > 0 && rect.height > 0)
    )
      continue;
    let settled = true;
    try {
      settled = !element
        .getAnimations({ subtree: true })
        .some((animation) => animation.playState === 'running' || animation.playState === 'pending');
    } catch (_) {
      settled = false;
    }
    const states = [];
    const tag = element.tagName.toLowerCase();
    const type = String(element.type || '').toLowerCase();
    if (tag === 'input' && (type === 'checkbox' || type === 'radio')) {
      states.push({
        property: 'checked',
        value: type === 'checkbox' && element.indeterminate ? 'mixed' : String(element.checked),
        semanticOverride: element.hasAttribute('aria-checked'),
      });
    }
    if (tag === 'option')
      states.push({
        property: 'selected',
        value: String(element.selected),
        semanticOverride: element.hasAttribute('aria-selected'),
      });
    if (tag === 'details')
      states.push({
        property: 'expanded',
        value: String(element.open),
        semanticOverride: element.hasAttribute('aria-expanded'),
      });
    if (states.length) result.push({ identity: `key:id:${element.id}`, id: element.id, settled, states });
  }
  return result;
}

async function captureAccessibilityStateParity(page) {
  const controls = await page.evaluate(collectNativeAccessibilityStateInPage);
  if (!controls.length) return { outcome: 'ABSTAIN', checks: [], items: [] };
  let cdp;
  try {
    cdp = await page.context().newCDPSession(page);
    const { nodes } = await cdp.send('Accessibility.getFullAXTree');
    for (const control of controls) {
      const expression = `document.getElementById(${JSON.stringify(control.id)})`;
      const remote = await cdp.send('Runtime.evaluate', {
        expression,
        returnByValue: false,
        objectGroup: 'reproit-accessibility-state',
      });
      if (!remote.result?.objectId) continue;
      const described = await cdp.send('DOM.describeNode', { objectId: remote.result.objectId });
      control.backendDOMNodeId = described.node?.backendNodeId;
    }
    await cdp
      .send('Runtime.releaseObjectGroup', { objectGroup: 'reproit-accessibility-state' })
      .catch(() => {});
    return evaluateAccessibilityStateParity(controls, nodes);
  } catch (_) {
    return {
      outcome: 'ABSTAIN',
      checks: controls.flatMap((control) =>
        control.states.map((state) => ({
          identity: control.identity,
          property: state.property,
          expected: String(state.value),
          outcome: 'ABSTAIN',
          reason: 'accessibility-tree-unavailable',
        })),
      ),
      items: [],
    };
  } finally {
    await cdp?.detach().catch(() => {});
  }
}

function checkKey(check) {
  return [
    check.identity,
    check.property,
    check.expected,
    check.actual ?? '',
    check.outcome,
    check.reason ?? '',
  ].join('\0');
}

// State must be identical in two independently captured, settled samples. Any
// state that changes, appears, or disappears between samples is explicitly an
// abstention and can never enter the violation marker stream.
export function confirmAccessibilityStateParity(first, second) {
  const firstChecks = Array.isArray(first?.checks) ? first.checks : [];
  const secondChecks = Array.isArray(second?.checks) ? second.checks : [];
  const firstBySubject = new Map(firstChecks.map((check) => [`${check.identity}\0${check.property}`, check]));
  const secondBySubject = new Map(
    secondChecks.map((check) => [`${check.identity}\0${check.property}`, check]),
  );
  const subjects = [...new Set([...firstBySubject.keys(), ...secondBySubject.keys()])].sort();
  const checks = subjects.map((subject) => {
    const a = firstBySubject.get(subject);
    const b = secondBySubject.get(subject);
    if (a && b && checkKey(a) === checkKey(b)) return a;
    const source = b || a;
    return {
      identity: source.identity,
      property: source.property,
      fingerprint: source.fingerprint,
      expected: source.expected,
      outcome: 'ABSTAIN',
      reason: 'state-not-settled',
    };
  });
  return {
    outcome: resultOutcome(checks),
    checks,
    items: checks.filter((check) => check.outcome === 'VIOLATION'),
  };
}

export async function scanAccessibilityStateParity(page, settleMs = 120) {
  const first = await captureAccessibilityStateParity(page);
  await page.waitForTimeout(settleMs);
  const second = await captureAccessibilityStateParity(page);
  return confirmAccessibilityStateParity(first, second);
}
