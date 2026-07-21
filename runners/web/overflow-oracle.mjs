// Bounded layout-containment evidence for DOM-based runners.
//
// A container opts into the invariant with `data-reproit-contain`. This is an
// application contract, not a guess based on colors, borders, or class names.
// Every returned record is a geometry fact. The Rust core owns the tri-state
// verdict and rejects ambiguous evidence.

export function layoutOverflowScan() {
  const MAX_CHECKS = 128;
  const MAX_KEY_LENGTH = 256;
  const candidateRootKey = (element) => {
    const testId = (
      element.getAttribute('data-testid') || element.getAttribute('data-test-id') || ''
    ).trim();
    if (testId) return 'key:testid:' + testId;
    const id = (element.id || '').trim();
    return id ? 'key:id:' + id : null;
  };
  const rootKeyCounts = new Map();
  for (const element of document.querySelectorAll('[data-testid], [data-test-id], [id]')) {
    const key = candidateRootKey(element);
    if (key) rootKeyCounts.set(key, (rootKeyCounts.get(key) || 0) + 1);
  }
  const stableRootKey = (element) => {
    const key = candidateRootKey(element);
    return key && key.length <= MAX_KEY_LENGTH && rootKeyCounts.get(key) === 1 ? key : null;
  };
  const structuralKey = (element, container, containerKey) => {
    const direct = stableRootKey(element);
    if (direct) return direct;
    const parts = [];
    for (let node = element; node && node !== container; node = node.parentElement) {
      const parent = node.parentElement;
      if (!parent) return null;
      const peers = [...parent.children].filter((peer) => peer.tagName === node.tagName);
      const index = peers.indexOf(node);
      if (index < 0) return null;
      parts.push(node.tagName.toLowerCase() + '#' + index);
      if (parts.length > 12) return null;
    }
    const key = parts.length ? containerKey + '>' + parts.reverse().join('>') : containerKey;
    return key.length <= MAX_KEY_LENGTH ? key : null;
  };
  const visible = (element) => {
    if (!element.isConnected) return false;
    for (let node = element; node && node.nodeType === 1; node = node.parentElement) {
      const style = getComputedStyle(node);
      if (
        style.display === 'none' ||
        style.visibility === 'hidden' ||
        style.visibility === 'collapse' ||
        Number(style.opacity) === 0 ||
        style.contentVisibility === 'hidden' ||
        node.hidden ||
        node.inert ||
        node.getAttribute('aria-hidden') === 'true'
      )
        return false;
    }
    return true;
  };
  const transformedBetween = (element, container) => {
    for (let node = element; node; node = node.parentElement) {
      const style = getComputedStyle(node);
      if (style.transform !== 'none' || style.perspective !== 'none') return true;
      if (node === container) break;
    }
    return false;
  };
  const rect = (value) => ({
    left: value.left,
    top: value.top,
    right: value.right,
    bottom: value.bottom,
  });
  const contentRect = (element) => {
    const box = element.getBoundingClientRect();
    return {
      left: box.left + element.clientLeft,
      top: box.top + element.clientTop,
      right: box.left + element.clientLeft + element.clientWidth,
      bottom: box.top + element.clientTop + element.clientHeight,
    };
  };
  const policyOf = (container, subject) => {
    const declared = (container.getAttribute('data-reproit-overflow') || '')
      .trim()
      .toLowerCase();
    if (declared === 'scroll' || declared === 'truncate') return declared;
    const containerStyle = getComputedStyle(container);
    if (/(auto|scroll)/.test(containerStyle.overflowX + ' ' + containerStyle.overflowY)) {
      return 'scroll';
    }
    const subjectStyle = getComputedStyle(subject);
    if (subjectStyle.textOverflow === 'ellipsis' || subjectStyle.webkitLineClamp !== 'none') {
      return 'truncate';
    }
    return 'contain';
  };
  const ownTextRect = (element) => {
    const boxes = [];
    for (const child of element.childNodes) {
      if (child.nodeType !== Node.TEXT_NODE || !(child.textContent || '').trim()) continue;
      const range = document.createRange();
      range.selectNodeContents(child);
      boxes.push(...range.getClientRects());
    }
    const visibleBoxes = boxes.filter((box) => box.width > 0 && box.height > 0);
    if (!visibleBoxes.length) return null;
    return {
      left: Math.min(...visibleBoxes.map((box) => box.left)),
      top: Math.min(...visibleBoxes.map((box) => box.top)),
      right: Math.max(...visibleBoxes.map((box) => box.right)),
      bottom: Math.max(...visibleBoxes.map((box) => box.bottom)),
    };
  };
  const checks = [];
  let total = 0;
  const containers = [...document.querySelectorAll('[data-reproit-contain]')];
  for (const container of containers) {
    const containerKey = stableRootKey(container);
    if (!containerKey || !visible(container)) continue;
    const containerBox = contentRect(container);
    for (const subject of [container, ...container.querySelectorAll('*')]) {
      if (!visible(subject)) continue;
      const subjectBox = ownTextRect(subject);
      if (!subjectBox) continue;
      total++;
      if (checks.length >= MAX_CHECKS) continue;
      const subjectKey = structuralKey(subject, container, containerKey);
      if (!subjectKey) continue;
      checks.push({
        subjectKey,
        containerKey,
        authority: 'exact-layout',
        ownership: 'app',
        stableSamples: 1,
        transformed: transformedBetween(subject, container),
        policy: policyOf(container, subject),
        subjectRect: rect(subjectBox),
        containerRect: rect(containerBox),
      });
    }
  }
  checks.sort((left, right) =>
    (left.subjectKey + '\0' + left.containerKey).localeCompare(
      right.subjectKey + '\0' + right.containerKey,
    ),
  );
  return { complete: total <= MAX_CHECKS, checks };
}

function sameRect(left, right) {
  if (!left || !right) return false;
  return ['left', 'top', 'right', 'bottom'].every(
    (field) => Math.abs(Number(left[field]) - Number(right[field])) <= 0.5,
  );
}

export function confirmLayoutOverflow(first, second) {
  if (!first || !second) {
    return {
      version: 1,
      complete: false,
      defect: 'capture-unavailable',
      checks: [],
    };
  }
  if (!first.complete || !second.complete) {
    return {
      version: 1,
      complete: false,
      defect: 'evidence-limit-exceeded',
      checks: [],
    };
  }
  const keyOf = (check) => check.subjectKey + '\0' + check.containerKey;
  const secondByKey = new Map(second.checks.map((check) => [keyOf(check), check]));
  const checks = [];
  for (const check of first.checks) {
    const again = secondByKey.get(keyOf(check));
    if (
      !again ||
      check.policy !== again.policy ||
      check.transformed !== again.transformed ||
      !sameRect(check.subjectRect, again.subjectRect) ||
      !sameRect(check.containerRect, again.containerRect)
    )
      continue;
    checks.push({ ...again, stableSamples: 2 });
  }
  return { version: 1, complete: true, checks };
}
