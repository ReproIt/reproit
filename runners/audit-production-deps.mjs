// Audit a runner's production dependencies, with reviewed exceptions.
//
// `npm audit --audit-level=high` is the right gate but has no way to say "this
// one advisory has no non-breaking fix yet". The usual workarounds are to drop
// the audit step or lower the level, which stops the gate reporting anything.
// This keeps the gate at high and requires every exception to be written down
// in `audit-allow.json` next to the runner's package.json.
//
// An exception is not permanent. The run fails if an allowlisted advisory has
// stopped appearing (upstream shipped the fix, so delete the entry) or if its
// review date has passed. So the allowlist cannot quietly outlive its reason,
// and a NEW advisory still fails the build the first time it appears.
//
// Usage: node ../audit-production-deps.mjs   (from the runner's directory)

import { execFileSync } from "node:child_process";
import { readFileSync, existsSync } from "node:fs";

const BLOCKING = new Set(["high", "critical"]);
const ALLOWLIST = "audit-allow.json";

function audit() {
  // npm audit exits non-zero when it finds anything, so the report comes off
  // the failure path as often as not; both carry the same JSON on stdout.
  try {
    return execFileSync("npm", ["audit", "--omit=dev", "--json"], {
      encoding: "utf8",
      maxBuffer: 32 * 1024 * 1024,
    });
  } catch (error) {
    if (error.stdout) return error.stdout;
    throw error;
  }
}

/** Every distinct blocking advisory in the report, keyed by GHSA id. */
function advisories(report) {
  const found = new Map();
  for (const vulnerability of Object.values(report.vulnerabilities ?? {})) {
    for (const via of vulnerability.via ?? []) {
      if (typeof via !== "object" || !BLOCKING.has(via.severity)) continue;
      const id = String(via.url ?? "").split("/").pop();
      if (!id) continue;
      if (!found.has(id)) {
        found.set(id, { id, title: via.title, severity: via.severity, url: via.url, packages: new Set() });
      }
      found.get(id).packages.add(via.name);
    }
  }
  return found;
}

function allowlist() {
  if (!existsSync(ALLOWLIST)) return {};
  const entries = JSON.parse(readFileSync(ALLOWLIST, "utf8"));
  for (const [id, entry] of Object.entries(entries)) {
    if (!entry.reason || !entry.review) {
      throw new Error(`${ALLOWLIST}: ${id} needs both a "reason" and a "review" date`);
    }
    if (Number.isNaN(Date.parse(entry.review))) {
      throw new Error(`${ALLOWLIST}: ${id} has an unparseable review date ${entry.review}`);
    }
  }
  return entries;
}

const report = JSON.parse(audit());
const found = advisories(report);
const allowed = allowlist();
const today = new Date().toISOString().slice(0, 10);
const failures = [];

for (const advisory of found.values()) {
  const entry = allowed[advisory.id];
  if (!entry) {
    failures.push(
      `unreviewed ${advisory.severity} advisory ${advisory.id}: ${advisory.title}\n` +
        `    ${advisory.url}\n` +
        `    reached through: ${[...advisory.packages].sort().join(", ")}\n` +
        `    Upgrade if a non-breaking fix exists. If it does not, add ${advisory.id} to ` +
        `${ALLOWLIST}\n    with a reason and a review date.`,
    );
    continue;
  }
  if (entry.review < today) {
    failures.push(
      `the exception for ${advisory.id} was due for review on ${entry.review}\n` +
        `    ${advisory.url}\n` +
        `    Re-check whether a fix has shipped, then either upgrade or move the review date.`,
    );
  }
}

// A stale entry is a failure too: once upstream ships the fix the advisory stops
// appearing, and the exception must not linger and silence the next one.
for (const id of Object.keys(allowed)) {
  if (!found.has(id)) {
    failures.push(
      `${ALLOWLIST} still excepts ${id}, which no longer appears in the audit.\n` +
        `    The fix has shipped: delete the entry.`,
    );
  }
}

const counts = report.metadata?.vulnerabilities ?? {};
const scale = `${counts.critical ?? 0} critical, ${counts.high ?? 0} high`;
if (failures.length > 0) {
  console.error(`production dependency audit FAILED (${scale})\n`);
  for (const failure of failures) console.error(`  - ${failure}\n`);
  process.exit(1);
}

const reviewed = Object.keys(allowed).length;
console.log(
  `production dependency audit passed (${scale}; ` +
    `${reviewed} reviewed exception${reviewed === 1 ? "" : "s"})`,
);
for (const [id, entry] of Object.entries(allowed)) {
  console.log(`  ${id} accepted until ${entry.review}: ${entry.reason}`);
}
