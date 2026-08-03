#!/usr/bin/env node
import { readFile, readdir } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "./web/node_modules/esbuild/lib/main.js";

const runners = dirname(fileURLToPath(import.meta.url));
// A `source/<dir>` entry concatenates its .mjs parts into one output. A
// `source/<file>.mjs` entry ships that single module. The shared/ modules are
// imported at RUNTIME by electron.mjs and tauri.mjs (output-relative
// specifiers survive the non-bundling targets verbatim), so they must ship as
// real files next to the runners, exactly like the './web/*.mjs' oracles.
const targets = [
  ["source/shared/signature.mjs", "shared/signature.mjs", false],
  ["source/shared/fuzz.mjs", "shared/fuzz.mjs", false],
  ["source/shared/dom-walk.mjs", "shared/dom-walk.mjs", false],
  ["source/shared/video-flicker.mjs", "shared/video-flicker.mjs", false],
  ["source/tauri-snapshot", "tauri-snapshot.mjs", false],
  ["source/hygiene", "web/hygiene-oracles.mjs", true],
  ["source/a2ui", "web/a2ui-runner.mjs", true],
  ["source/web", "web/runner.mjs", true],
  ["source/react-native", "rn/runner.mjs", true],
  ["source/electron", "electron.mjs", false],
  ["source/tauri", "tauri.mjs", false],
];

for (const [sourcePath, outputFile, bundle] of targets) {
  let source;
  if (sourcePath.endsWith(".mjs")) {
    source = await readFile(join(runners, sourcePath), "utf8");
  } else {
    const directory = join(runners, sourcePath);
    const names = (await readdir(directory))
      .filter((name) => name.endsWith(".mjs"))
      .sort();
    source = (
      await Promise.all(names.map((name) => readFile(join(directory, name), "utf8")))
    ).join("");
  }
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
