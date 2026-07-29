#!/usr/bin/env node
import { readFile, readdir } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "./web/node_modules/esbuild/lib/main.js";

const runners = dirname(fileURLToPath(import.meta.url));
const targets = [
  ["source/tauri-snapshot", "tauri-snapshot.mjs", false],
  ["source/hygiene", "web/hygiene-oracles.mjs", true],
  ["source/a2ui", "web/a2ui-runner.mjs", true],
  ["source/web", "web/runner.mjs", true],
  ["source/react-native", "rn/runner.mjs", true],
  ["source/electron", "electron.mjs", false],
  ["source/tauri", "tauri.mjs", false],
];

for (const [sourceDirectory, outputFile, bundle] of targets) {
  const directory = join(runners, sourceDirectory);
  const names = (await readdir(directory))
    .filter((name) => name.endsWith(".mjs"))
    .sort();
  const source = (
    await Promise.all(names.map((name) => readFile(join(directory, name), "utf8")))
  ).join("");
  await build({
    stdin: {
      contents: source,
      resolveDir: dirname(join(runners, outputFile)),
      sourcefile: outputFile,
      loader: "js",
    },
    outfile: resolve(runners, outputFile),
    bundle,
    platform: "node",
    format: "esm",
    packages: "external",
    legalComments: "inline",
    minify: true,
    allowOverwrite: true,
    logLevel: "info",
  });
}
