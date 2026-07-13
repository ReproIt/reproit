// Dogfood the app-invariant oracle both directions. Under the fuzzer (the
// REPROIT_INVARIANT_FILE env var is set) a violating state appends a
// REPROIT_INVARIANT marker to that file, which the TUI backend re-emits as
// EXPLORE:INVARIANT; a clean state and a production run (no env var) append
// nothing.
//
// Runs with Node's built-in test runner, node >= 22 (TS types stripped):
//   node --test sdk/reproit-tui-ts/test/invariant.test.ts
//
// No em dashes anywhere, per project rules.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { Reporter } from "../reporter.ts";

function markers(path: string): Array<{ sig: string; items: Array<{ id: string; message: string }> }> {
  if (!existsSync(path)) return [];
  return readFileSync(path, "utf8")
    .split("\n")
    .filter((l) => l.startsWith("REPROIT_INVARIANT "))
    .map((l) => JSON.parse(l.slice("REPROIT_INVARIANT ".length)));
}

// Give the reporter's fs write (a cached dynamic import under Node ESM) a tick
// to land before asserting.
const settle = () => new Promise((r) => setTimeout(r, 20));

test("invariant reports only violations under the fuzzer", async () => {
  const path = join(tmpdir(), `reproit-inv-ts-${process.pid}-${Math.random()}.ndjson`);
  process.env.REPROIT_INVARIANT_FILE = path;
  try {
    const r = new Reporter({ appId: "t" });
    r.invariant("holds", () => true);
    r.invariant("neg", () => ({ ok: false, message: "count < 0" }));
    r.invariant("falsy", () => 0);
    r.invariant("throws", () => {
      throw new Error("kaboom");
    });
    r.observeContents("Count: -1", 0, 0, "key:Down");
    await settle();

    const m = markers(path);
    assert.equal(m.length, 1);
    assert.equal(m[0].sig, r.currentSig());
    const byId = Object.fromEntries(m[0].items.map((it) => [it.id, it.message]));
    assert.deepEqual(Object.keys(byId).sort(), ["falsy", "neg", "throws"]);
    assert.equal(byId.neg, "count < 0");
    assert.equal(byId.falsy, "");
    assert.equal(byId.throws, "kaboom");
  } finally {
    rmSync(path, { force: true });
    delete process.env.REPROIT_INVARIANT_FILE;
  }
});

test("a satisfied registry and a production run write nothing", async () => {
  const path = join(tmpdir(), `reproit-inv-ts2-${process.pid}-${Math.random()}.ndjson`);
  process.env.REPROIT_INVARIANT_FILE = path;
  try {
    const r = new Reporter({ appId: "t" });
    r.invariant("holds", () => true);
    r.observeContents("Count: 3", 0, 0, "load");
    await settle();
    assert.deepEqual(markers(path), []);
  } finally {
    rmSync(path, { force: true });
    delete process.env.REPROIT_INVARIANT_FILE;
  }

  // Inert without the gate (production): no marker even with a violation.
  const r2 = new Reporter({ appId: "t" });
  r2.invariant("violated", () => false);
  r2.observeContents("Count: 4", 0, 0, "load");
  await settle();
});
