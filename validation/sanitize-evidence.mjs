import { homedir } from 'node:os';

function replacePath(text, path, replacement) {
  if (!path) return text;
  return text.split(path).join(replacement);
}

export function sanitizeEvidenceText(value, repositoryRoot) {
  let text = String(value);
  text = replacePath(text, repositoryRoot, '${REPROIT_ROOT}');
  text = replacePath(text, homedir(), '${HOME}');
  return text;
}

export function sanitizeEvidence(value, repositoryRoot) {
  if (typeof value === 'string') return sanitizeEvidenceText(value, repositoryRoot);
  if (Array.isArray(value)) return value.map((item) => sanitizeEvidence(item, repositoryRoot));
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, sanitizeEvidence(item, repositoryRoot)]),
    );
  }
  return value;
}

export function evidenceJson(value, repositoryRoot) {
  return `${JSON.stringify(sanitizeEvidence(value, repositoryRoot), null, 2)}\n`;
}
