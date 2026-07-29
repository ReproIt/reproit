#!/usr/bin/env node
import { copyFileSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const directory = dirname(fileURLToPath(import.meta.url));
const repository = resolve(directory, "..");
const sourceParts = [
  "reproit-web-config.part.js",
  "reproit-web-dom.part.js",
  "reproit-web-runtime.part.js",
];
const source = sourceParts
  .map((name) => readFileSync(join(directory, "src", name), "utf8"))
  .join("");
const scratch = mkdtempSync(join(tmpdir(), "reproit-web-build-"));
const entry = join(scratch, "reproit-web.js");
const output = join(directory, "reproit-web.js");
try {
  await import("node:fs/promises").then(({ writeFile }) => writeFile(entry, source));
  const esbuild = process.env.REPROIT_ESBUILD ||
    join(repository, "runners", "web", "node_modules", ".bin", "esbuild");
  const result = spawnSync(esbuild, [
    entry,
    "--minify",
    "--legal-comments=inline",
    "--outfile=" + output,
  ], { stdio: "inherit" });
  if (result.status !== 0) process.exit(result.status || 1);
  if (process.argv.includes("--sync-demos")) {
    copyFileSync(output, resolve(repository, "..", "reproit-cloud", "static", "demo", "reproit-web.js"));
    copyFileSync(output, resolve(repository, "..", "reproit-cloud-deploy", "service", "static", "demo", "reproit-web.js"));
  }
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
